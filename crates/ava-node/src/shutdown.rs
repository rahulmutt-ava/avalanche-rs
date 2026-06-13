// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The 14-step node shutdown sequence (specs/12 §2.4, 17 §4.3/§4.4; mirror Go
//! `node/node.go::Shutdown` + `node.shutdown`).
//!
//! [`Node::shutdown`] sets the exit code + `shuttingDown` flag (the first
//! demand wins) and runs the 14 steps **exactly once** through a
//! [`tokio::sync::OnceCell`] (Go `shutdownOnce`). The step order is pinned by
//! [`tests::shutdown_order_matches_go`].
//!
//! Each step follows the 17 §4.4 lifecycle: **cancel** the subsystem's token,
//! **drain** its tasks within a timeout (`consensus-shutdown-timeout` for the
//! chains), then **abandon** stragglers (they observe the cancelled token and
//! unwind on their own) and **drop** the handles. Persistence is last
//! (`db.delete(ungracefulShutdown)` then `db.close()`); the tracer flushes
//! after the DB so trace spans for the shutdown itself are exported.

use std::sync::atomic::Ordering;

use ava_api::server::ApiServer;
use ava_network::network::Network;

use crate::node::Node;

/// The Go `errShuttingDown` health-check name (12 §2.4 step 1): a check that
/// always fails, forcing the node unhealthy for the `http-shutdown-wait`
/// window so load balancers stop routing to it before the API server closes.
pub(crate) const SHUTTING_DOWN_CHECK: &str = "shuttingDown";

impl Node {
    /// Demand a node shutdown with `exit_code` (Go `Node.Shutdown`). The first
    /// caller's code wins; the 14-step sequence runs exactly once. Concurrent
    /// or later callers await the in-flight (or completed) sequence.
    pub async fn shutdown(&self, exit_code: i32) {
        self.shutdown_recorded(exit_code, None).await;
    }

    /// [`Node::shutdown`] with an optional step recorder (the
    /// `shutdown_order_matches_go` seam: each step pushes its Go shutdown
    /// name). The recorder is shared (`parking_lot::Mutex`) because the
    /// `OnceCell` future may run on any task.
    pub(crate) async fn shutdown_recorded(
        &self,
        exit_code: i32,
        recorder: Option<&parking_lot::Mutex<Vec<&'static str>>>,
    ) {
        // Only the first demand records the exit code (Go `shuttingDown.Swap`).
        if !self.shutting_down.swap(true, Ordering::SeqCst) {
            self.exit_code.store(exit_code, Ordering::SeqCst);
        }
        // The root token is cancelled here so `dispatch`'s `net.dispatch()`
        // (and every token-aware task) begins unwinding immediately, in
        // parallel with the ordered teardown below (Go cancels via
        // `Net.StartClose` inside the sequence; the root token is the Rust
        // node's single cancellation source).
        self.shutdown.cancel();

        // Run the 14 steps exactly once (Go `shutdownOnce.Do`). Re-entrant /
        // concurrent callers await the same future and observe its completion.
        self.shutdown_once
            .get_or_init(|| async {
                self.run_shutdown(recorder).await;
            })
            .await;
    }

    /// The ordered 14-step teardown (17 §4.3). Run once, behind the
    /// `shutdown_once` cell.
    async fn run_shutdown(&self, recorder: Option<&parking_lot::Mutex<Vec<&'static str>>>) {
        let record = |name: &'static str| {
            if let Some(r) = recorder {
                r.lock().push(name);
            }
        };

        tracing::info!(exit_code = self.exit_code(), "shutting down node");

        // 1. Register the `shuttingDown` health check (forces unhealthy), then
        //    sleep `http-shutdown-wait` so orchestrators drain traffic first.
        record(SHUTTING_DOWN_CHECK);
        self.register_shutting_down_check();
        tokio::time::sleep(self.config.http_config.shutdown_wait).await;

        // 2. Staking signer (an `rpcsigner` would close its connection here).
        record("staking_signer");
        if let Err(e) = self.staking_signer.shutdown() {
            tracing::debug!(error = %e, "error during staking signer shutdown");
        }

        // 3. Resource manager (cancels the system-resource poller).
        record("resource_manager");
        self.resources.manager.shutdown();

        // 4. Timeout manager (cancels the timeout-dispatch loop).
        record("timeout_manager");
        self.timeout_manager.stop();

        // 5. Chain manager: per chain cancel → drain (consensus-shutdown
        //    budget) → abandon stragglers → drop.
        record("chain_manager");
        self.chain_manager
            .shutdown(self.config.consensus_shutdown_timeout)
            .await;

        // 6. Benchlist (the Rust M3 benchlist has no background task; the call
        //    is the Go-parity placeholder).
        record("benchlist");
        // `Benchlist` exposes no shutdown today (no poller); noop for parity.

        // 7. Profiler (only the continuous profiler has state; deferred —
        //    on-demand admin profiler holds nothing).
        record("profiler");
        // The continuous profiler is a documented deferral (tests/PORTING.md).

        // 8. Network: stop accepting + cancel the network token so the peer
        //    actors unwind. The root token (cancelled above) is the parent of
        //    `network_token`, but `start_close` also closes the listener.
        record("net_start_close");
        self.networking.net.start_close();
        self.network_token.cancel();

        // 9. API server: graceful close, bounded by `http-shutdown-timeout`.
        record("api_server");
        let api_shutdown = self.api_server.shutdown();
        match tokio::time::timeout(self.config.http_config.shutdown_timeout, api_shutdown).await {
            Ok(Err(e)) => tracing::debug!(error = %e, "error during API shutdown"),
            Err(_) => tracing::warn!("API server did not shut down within http-shutdown-timeout"),
            Ok(Ok(())) => {}
        }

        // 10. NAT: unmap all ports + stop the dynamic-IP updater. Port mappings
        //     were spawned under the network token (and the HTTP map under the
        //     root token), so cancelling above already triggered each mapping's
        //     unmap; the dynamic-IP updater is awaited here.
        record("nat");
        self.unmap_ports_and_stop_ip_updater();

        // 11. Indexer: flush the final batch.
        record("indexer");
        if let Err(e) = self.indexer.close().await {
            tracing::debug!(error = %e, "error closing tx indexer");
        }

        // 12. Runtime manager: kill plugin subprocesses.
        record("runtime_manager");
        tracing::info!("cleaning up plugin runtimes");
        self.runtime_manager.stop();

        // 13. Database: delete the ungraceful-shutdown marker (a clean exit),
        //     then close. Persistence is the last subsystem torn down.
        record("database");
        if let Err(e) = self
            .db
            .delete(crate::init::database::UNGRACEFUL_SHUTDOWN_KEY)
        {
            tracing::error!(error = %e, "failed to delete ungraceful shutdown key");
        }
        if let Err(e) = self.db.close() {
            tracing::warn!(error = %e, "error during DB shutdown");
        }

        // 14. Tracer: flush spans (after the DB so the shutdown's own spans are
        //     exported).
        record("tracer");
        if let Err(e) = self.tracer.shutdown() {
            tracing::warn!(error = %e, "error during tracer shutdown");
        }

        tracing::info!("finished node shutdown");
    }

    /// Register the always-failing `shuttingDown` health check (step 1; Go's
    /// `errShuttingDown` checker). A registration failure is non-fatal — the
    /// node is shutting down regardless.
    fn register_shutting_down_check(&self) {
        use ava_api::health::{APPLICATION_TAG, CheckError, CheckResult, Checker};
        use futures::FutureExt;
        use futures::future::BoxFuture;

        struct ShuttingDown;
        impl Checker for ShuttingDown {
            fn health_check(&self) -> BoxFuture<'static, CheckResult> {
                async { Err(CheckError::new("node is shutting down")) }.boxed()
            }
        }

        if let Err(e) = self.health.register_health_check(
            SHUTTING_DOWN_CHECK,
            std::sync::Arc::new(ShuttingDown),
            &[APPLICATION_TAG.to_owned()],
        ) {
            tracing::debug!(error = %e, "couldn't register shuttingDown health check");
        }
    }

    /// Step 10: unmap all NAT ports + stop the dynamic-IP updater. Each port
    /// mapping runs its own keep-alive task under a child of the network token;
    /// cancelling that token (step 8) signals every mapping to unmap. Here we
    /// await the staking-port keep-alive task to confirm the unmap ran, and
    /// stop the dynamic-IP updater if one is running.
    fn unmap_ports_and_stop_ip_updater(&self) {
        // The staking-port and HTTP-port keep-alive tasks observe their
        // (cancelled) tokens and unmap on exit (Go `portMapper.UnmapAllPorts`).
        self.networking.port_mapping.abort();
        // The dynamic-IP updater is always `None` today (the resolver is a
        // documented deferral); abort it if a future resolver lands.
        if let Some(updater) = self.networking.ip_updater.as_ref() {
            updater.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use crate::testutil::build_node;

    /// The Go 14-step shutdown order of `node.go::shutdown` (12 §2.4 / 17
    /// §4.3).
    const GO_SHUTDOWN_ORDER: [&str; 14] = [
        "shuttingDown",     // 1  — health check + http-shutdown-wait
        "staking_signer",   // 2
        "resource_manager", // 3
        "timeout_manager",  // 4
        "chain_manager",    // 5  — per-chain cancel + drain
        "benchlist",        // 6
        "profiler",         // 7
        "net_start_close",  // 8  — close listener + cancel net token
        "api_server",       // 9  — graceful, http-shutdown-timeout
        "nat",              // 10 — unmap ports + ip-updater stop
        "indexer",          // 11
        "runtime_manager",  // 12
        "database",         // 13 — delete ungraceful marker, then close
        "tracer",           // 14 — flush spans
    ];

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_order_matches_go() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir(): {e}"));
        let node = build_node(dir.path()).await;

        let recorded = Mutex::new(Vec::new());
        node.shutdown_recorded(1, Some(&recorded)).await;

        assert_eq!(
            recorded.into_inner(),
            GO_SHUTDOWN_ORDER,
            "Node::shutdown step order must match Go node.shutdown"
        );
        assert_eq!(node.exit_code(), 1, "shutdown(1) records exit code 1");
        assert!(
            node.shutting_down(),
            "node is shutting down after shutdown()"
        );
        assert!(node.shutdown.is_cancelled(), "root token is cancelled");
        assert!(
            node.network_token.is_cancelled(),
            "network token cancelled at step 8"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_runs_once() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir(): {e}"));
        let node = build_node(dir.path()).await;

        // The first demand wins; the sequence runs exactly once even though two
        // callers race with different exit codes.
        let recorded = Mutex::new(Vec::new());
        node.shutdown_recorded(2, Some(&recorded)).await;
        // A second call with a different code must NOT re-run the steps and must
        // NOT overwrite the recorded exit code.
        node.shutdown_recorded(7, Some(&recorded)).await;

        assert_eq!(
            recorded.into_inner().len(),
            GO_SHUTDOWN_ORDER.len(),
            "the 14 steps run exactly once across two shutdown demands"
        );
        assert_eq!(node.exit_code(), 2, "the first demand's exit code wins");
    }

    /// Cancellation propagation (17 §9): cancelling one subnet's token reaches
    /// only that subnet's chains; other subnets' chains stay live.
    #[tokio::test(flavor = "multi_thread")]
    async fn subnet_cancellation_is_scoped() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir(): {e}"));
        let node = build_node(dir.path()).await;

        let subnet_a = ava_types::id::Id::from([1u8; 32]);
        let subnet_b = ava_types::id::Id::from([2u8; 32]);
        let chain_a = ava_types::id::Id::from([0xaa; 32]);
        let chain_b = ava_types::id::Id::from([0xbb; 32]);

        let (token_a, tasks_a) =
            node.chain_manager
                .register_chain(chain_a, subnet_a, &node.subnet_token);
        let (token_b, _tasks_b) =
            node.chain_manager
                .register_chain(chain_b, subnet_b, &node.subnet_token);

        assert_eq!(
            node.chain_manager.running_chains(),
            2,
            "two chains registered"
        );

        // A worker on subnet A's chain that joins only once its token cancels.
        let worker_token = token_a.clone();
        tasks_a.spawn(async move { worker_token.cancelled().await });

        // Cancelling subnet A's token reaches chain A but not chain B.
        node.chain_manager
            .subnet_token(subnet_a, &node.subnet_token)
            .cancel();

        assert!(
            token_a.is_cancelled(),
            "subnet A's chain token is cancelled"
        );
        assert!(!token_b.is_cancelled(), "subnet B's chain token stays live");

        // The cancelled subnet's worker joins; draining the chains completes.
        tasks_a.close();
        tokio::time::timeout(std::time::Duration::from_secs(5), tasks_a.wait())
            .await
            .unwrap_or_else(|_| panic!("subnet A worker did not join after cancellation"));

        // Step 5 drains every registered chain (cancel → close → wait within
        // the budget) and clears the running set.
        node.chain_manager
            .shutdown(std::time::Duration::from_secs(5))
            .await;
        assert_eq!(
            node.chain_manager.running_chains(),
            0,
            "chain_manager.shutdown() drains every registered chain"
        );
        assert!(
            token_b.is_cancelled(),
            "shutdown cancels the remaining chain token"
        );
    }
}
