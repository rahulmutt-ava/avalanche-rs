// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The node run loop (specs/12 §2.3, 17 §4.3; mirror Go `node/node.go::Dispatch`
//! + `writeProcessContext`).
//!
//! [`Node::dispatch`] writes the process-context file (`{pid, uri,
//! stakingAddress}`), spawns the HTTP API server task (an unexpected exit
//! demands `shutdown(1)`), spawns the bootstrap-beacon-connection-timeout warn
//! task, manually-tracks the configured state-sync + bootstrap peers, then
//! runs the P2P event loop (`net.dispatch().await`). When that loop returns,
//! the node shuts down (`shutdown(1)`) and the process-context file is removed
//! so an orchestrator sees the node is no longer running.
//!
//! Every dispatch-spawned task registers on [`Node::tasks`] so shutdown can
//! account for them.

use std::sync::Arc;

use ava_api::server::ApiServer;
use ava_network::network::Network;

use crate::node::Node;

impl Node {
    /// Start the node's servers and block until the node exits (Go
    /// `Node.Dispatch`). Returns the recorded exit code.
    ///
    /// Takes `Arc<Self>` because the spawned API + warn tasks (and the P2P
    /// `dispatch`) need to share ownership of the node past this call frame.
    pub async fn dispatch(self: Arc<Self>) -> i32 {
        // Write the process context so an orchestrator can find the live node.
        if let Err(e) = self.write_process_context() {
            tracing::error!(
                path = %self.config.process_context_file_path,
                error = %e,
                "failed to write process context"
            );
            // Go returns the error; the Rust node treats a missing process
            // context as fatal at startup and shuts down.
            self.shutdown(1).await;
            return self.exit_code();
        }

        // Start the HTTP API server. When `shutdown` runs, it calls
        // `api_server.shutdown()`, which makes `serve()` return `Ok`; an
        // unexpected exit (or error) while NOT shutting down demands
        // `shutdown(1)` (Go's `RecoverAndPanic` + `Shutdown(1)`).
        {
            let node = Arc::clone(&self);
            self.tasks.spawn(async move {
                tracing::info!(uri = %node.api_uri, "API server listening");
                let result = node.api_server.serve().await;
                if !node.shutting_down() {
                    match result {
                        Ok(()) => tracing::error!("API server stopped unexpectedly"),
                        Err(e) => tracing::error!(error = %e, "API server dispatch failed"),
                    }
                }
                node.shutdown(1).await;
            });
        }

        // Warn if we fail to connect to enough bootstrap beacons in time. The
        // task resolves either when the timer fires (warn) or when the network
        // reports it is sufficiently connected.
        {
            let node = Arc::clone(&self);
            let mut connected = self.networking.on_sufficiently_connected.clone();
            let timeout = self
                .config
                .bootstrap_config
                .bootstrap_beacon_connection_timeout;
            self.tasks.spawn(async move {
                tokio::select! {
                    () = tokio::time::sleep(timeout) => {
                        if !node.shutting_down() {
                            tracing::warn!(
                                duration = ?timeout,
                                "failed to connect to bootstrap nodes"
                            );
                        }
                    }
                    // `changed()` resolves when the network sends `true`; a
                    // closed channel (sender dropped) also resolves and is
                    // treated as "no longer waiting".
                    res = connected.changed() => {
                        if res.is_ok() && *connected.borrow() {
                            tracing::debug!("sufficiently connected to bootstrap nodes");
                        }
                    }
                }
            });
        }

        // Manually track the configured state-sync peers, then the bootstrap
        // beacons (Go `Net.ManuallyTrack`). `state_sync_ids` and
        // `state_sync_ips` are parallel slices (Go indexes them in lockstep).
        let state_sync = &self.config.state_sync_config;
        for (id, ip) in state_sync
            .state_sync_ids
            .iter()
            .zip(state_sync.state_sync_ips.iter())
        {
            self.networking.net.manually_track(*id, *ip);
        }
        for beacon in &self.config.bootstrap_config.bootstrappers {
            self.networking.net.manually_track(beacon.id, beacon.ip);
        }

        // No more tasks register after this point; close the tracker so a
        // future `tasks.wait()` can complete once the spawned tasks finish.
        self.tasks.close();

        // Run the P2P event loop until it returns (a clean close during
        // shutdown, or an unexpected stop).
        let dispatch_result = Arc::clone(&self.networking.net).dispatch().await;
        if let Err(e) = &dispatch_result
            && !self.shutting_down()
        {
            tracing::error!(error = %e, "P2P networking stopped unexpectedly");
        }

        // If the P2P server isn't running, shut down (a no-op if already
        // shutting down; it blocks until the sequence completes).
        self.shutdown(1).await;

        // Remove the process-context file so an orchestrator sees the node is
        // no longer running (Go `os.Remove`; a missing file is not an error).
        self.remove_process_context();

        self.exit_code()
    }

    /// Write the process-context JSON to `--process-context-file-path` (Go
    /// `writeProcessContext`).
    ///
    /// # Errors
    /// Serialization or filesystem-write failures.
    pub fn write_process_context(&self) -> std::io::Result<()> {
        tracing::info!(
            path = %self.config.process_context_file_path,
            "writing process context"
        );
        // Go `node.ProcessContext` shape: `{pid, uri, stakingAddress}` (pretty
        // JSON). Built via `serde_json::json!` to avoid a `serde` derive dep.
        let ctx = serde_json::json!({
            "pid": std::process::id(),
            "uri": self.api_uri,
            "stakingAddress": self.networking.staking_address.to_string(),
        });
        let bytes = serde_json::to_vec_pretty(&ctx)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = &self.config.process_context_file_path;
        if path.is_empty() {
            return Ok(());
        }
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)
    }

    /// Remove the process-context file (Go `os.Remove`; a missing file is fine).
    fn remove_process_context(&self) {
        let path = &self.config.process_context_file_path;
        if path.is_empty() {
            return;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::error!(%path, error = %e, "removal of process context file failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::testutil::build_node;

    /// `writeProcessContext` writes `{pid, uri, stakingAddress}` JSON to the
    /// configured path (12 §2.3).
    #[tokio::test(flavor = "multi_thread")]
    async fn write_process_context_writes_pid_uri_staking() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir(): {e}"));
        let node = build_node(dir.path()).await;

        node.write_process_context()
            .unwrap_or_else(|e| panic!("write_process_context(): {e}"));

        let path = &node.config.process_context_file_path;
        assert!(!path.is_empty(), "the process-context path is configured");
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read process context {path}: {e}"));
        let json: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse process context: {e}"));

        assert_eq!(
            json.get("pid").and_then(serde_json::Value::as_u64),
            Some(u64::from(std::process::id())),
            "process context records this pid"
        );
        assert_eq!(
            json.get("uri").and_then(serde_json::Value::as_str),
            Some(node.api_uri.as_str()),
            "process context records the api uri"
        );
        assert_eq!(
            json.get("stakingAddress")
                .and_then(serde_json::Value::as_str),
            Some(node.networking.staking_address.to_string().as_str()),
            "process context records the bound staking address"
        );
    }

    /// An unexpected API-server exit (its `serve()` returning while the node is
    /// not shutting down) triggers `shutdown(1)`. Here we close the API server
    /// before dispatch so `serve()` returns immediately, standing in for the
    /// unexpected exit, and assert the node ends up shut down with exit 1.
    #[tokio::test(flavor = "multi_thread")]
    async fn api_dispatch_failure_triggers_shutdown_1() {
        use ava_api::server::ApiServer;

        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir(): {e}"));
        let node = build_node(dir.path()).await;

        // Pre-signal the API server to stop: its `serve()` will return `Ok`
        // immediately. Because the node is not yet shutting down when the API
        // task observes that exit, it demands `shutdown(1)`.
        node.api_server
            .shutdown()
            .await
            .unwrap_or_else(|e| panic!("api_server.shutdown(): {e}"));

        // `dispatch` blocks until shutdown completes; the network's `dispatch`
        // returns once the root token (cancelled by `shutdown`) fires.
        let exit = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            Arc::clone(&node).dispatch(),
        )
        .await
        .unwrap_or_else(|_| panic!("dispatch did not return after API server stop"));

        assert_eq!(exit, 1, "an API-server stop drives shutdown(1)");
        assert!(node.shutting_down(), "node shut down after API server stop");
        assert_eq!(node.exit_code(), 1, "exit code is 1");

        // The process-context file is removed on exit.
        assert!(
            !std::path::Path::new(&node.config.process_context_file_path).exists(),
            "process context file is removed on exit"
        );
    }
}
