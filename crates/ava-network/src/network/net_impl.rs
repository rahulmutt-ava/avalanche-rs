// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `NetworkImpl` — the concrete networking runtime (`specs/05` §3.1/§3.4,
//! `specs/17` §2 #1/#2/#3/#4, §4.3).
//!
//! Mirrors Go `network/network.go`. The runtime owns:
//!
//! - the TCP **listener** + the inbound TLS **server upgrader** (#1 accept loop),
//! - the **dialer** + the outbound TLS **client upgrader** (#2 dialer),
//! - the **inbound conn-upgrade throttler** gating #1 (#3),
//! - `runTimers` (#4): the peer-list pull / bloom-reset / uptime tickers,
//! - the `connecting` / `connected` peer sets + the shared `IpTracker`,
//! - a root [`CancellationToken`] + a [`TaskTracker`] for graceful drain.
//!
//! `dispatch` runs the accept loop, dialer, and timers until the token is
//! cancelled, then drains every task. `start_close` is idempotent: it cancels
//! the token (tearing down every peer, which is a grandchild token) and closes
//! the listener.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava_types::node_id::NodeId;
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::Result;
use crate::config::PeerConfig;
use crate::dialer::Dialer;
use crate::metrics::Metrics;
use crate::network::ip_tracker::{
    IpTracker, PEER_LIST_BLOOM_RESET_FREQ, PEER_LIST_PULL_GOSSIP_FREQ,
};
use crate::network::peer_set::PeerSet;
use crate::network::tracked_ip::TrackedIp;
use crate::peer::peer::{Direction, Peer};
use crate::peer::upgrader::Upgrader;

/// How often the dialer scans the tracked-IP table for peers to (re)connect.
const DIAL_SCAN_INTERVAL: Duration = Duration::from_millis(250);

/// The concrete networking runtime.
pub struct NetworkImpl {
    peer_config: Arc<PeerConfig>,
    listener: Mutex<Option<TcpListener>>,
    listen_addr: SocketAddr,
    dialer: Dialer,
    server_upgrader: Upgrader,
    client_upgrader: Upgrader,
    ip_tracker: Arc<IpTracker>,
    /// node -> the address the dialer should (re)connect to.
    tracked_ips: Mutex<std::collections::HashMap<NodeId, TrackedIp>>,
    connecting: Arc<PeerSet>,
    connected: Arc<PeerSet>,
    /// Node-ids with an in-flight outbound dial (spawned by `handle_dial`, not
    /// yet admitted or failed). The scan dialer skips these so a slow/stalling
    /// TLS upgrade does not accumulate duplicate concurrent dials to the same
    /// peer — Go runs one dial goroutine per tracked IP; this guard set is the
    /// scan-loop equivalent. Cleared by `DialGuard` on every `handle_dial` exit.
    dialing: Mutex<std::collections::HashSet<NodeId>>,
    /// Serializes the compound "is this node already tracked? → register it"
    /// transition across `connecting`/`connected` for BOTH connection
    /// directions (Go's single `peersLock`). A leaf lock: held only across
    /// synchronous sections, never across an `.await`, never nested with
    /// `tracked_ips`.
    peers_lock: Mutex<()>,
    conn_upgrade_throttler:
        Arc<crate::throttling::inbound_conn_upgrade::InboundConnUpgradeThrottler>,
    /// Connection-level `avalanche_network_*` metrics (`specs/18` §2.1). Holds
    /// the `tls_conn_rejected`, `times_connected`, and `times_disconnected`
    /// counters incremented by the accept/dial/watch paths. `None` when the
    /// network runs without a metrics registry.
    metrics: Option<Metrics>,
    net_token: CancellationToken,
    tasks: TaskTracker,
}

/// Removes `node` from the in-flight `dialing` set on drop, covering every exit
/// path of the `handle_dial` task (dial failure, upgrade failure, admit) as well
/// as task cancellation.
struct DialGuard {
    net: Arc<NetworkImpl>,
    node: NodeId,
}

impl Drop for DialGuard {
    fn drop(&mut self) {
        self.net.dialing.lock().remove(&self.node);
    }
}

impl NetworkImpl {
    /// Build a network bound to `listener`, using `peer_config` for every peer.
    ///
    /// # Errors
    /// [`crate::Error::TlsConfig`] if building the TLS configs fails.
    pub fn new(peer_config: Arc<PeerConfig>, listener: TcpListener) -> Result<Arc<NetworkImpl>> {
        Self::new_inner(peer_config, listener, None)
    }

    /// Build a network with the connection-level `avalanche_network_*` metrics
    /// attached (`specs/18` §2.1). `metrics` supplies the `tls_conn_rejected`,
    /// `times_connected`, and `times_disconnected` counters. The per-peer I/O
    /// metrics and the byte-throttler "remaining" gauges are wired into the
    /// `PeerConfig` / throttler by the caller before construction (see
    /// `PeerConfig::with_peer_metrics` / `InboundMsgByteThrottler::set_metrics`).
    ///
    /// # Errors
    /// [`crate::Error::TlsConfig`] if building the TLS configs fails.
    pub fn new_with_metrics(
        peer_config: Arc<PeerConfig>,
        listener: TcpListener,
        metrics: Metrics,
    ) -> Result<Arc<NetworkImpl>> {
        Self::new_inner(peer_config, listener, Some(metrics))
    }

    fn new_inner(
        peer_config: Arc<PeerConfig>,
        listener: TcpListener,
        metrics: Option<Metrics>,
    ) -> Result<Arc<NetworkImpl>> {
        let listen_addr = listener.local_addr()?;
        let server_cfg = crate::peer::tls_config::server_config(&peer_config.identity)?;
        let client_cfg = crate::peer::tls_config::client_config(&peer_config.identity)?;
        let server_upgrader = Upgrader::server(server_cfg);
        let client_upgrader = Upgrader::client(client_cfg);

        let conn_upgrade_throttler = Arc::new(
            crate::throttling::inbound_conn_upgrade::InboundConnUpgradeThrottler::new(
                Duration::from_secs(10),
                256,
            ),
        );

        Ok(Arc::new(NetworkImpl {
            ip_tracker: Arc::clone(&peer_config.ip_tracker),
            peer_config,
            listener: Mutex::new(Some(listener)),
            listen_addr,
            dialer: Dialer::default(),
            server_upgrader,
            client_upgrader,
            tracked_ips: Mutex::new(std::collections::HashMap::new()),
            connecting: Arc::new(PeerSet::new()),
            connected: Arc::new(PeerSet::new()),
            dialing: Mutex::new(std::collections::HashSet::new()),
            peers_lock: Mutex::new(()),
            conn_upgrade_throttler,
            metrics,
            net_token: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }))
    }

    /// The address the listener is bound to.
    #[must_use]
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    /// The NodeIDs of peers that have finished the handshake.
    #[must_use]
    pub fn connected_peers(&self) -> Vec<NodeId> {
        self.connected.node_ids()
    }

    /// Atomically admit a freshly-upgraded peer for BOTH directions. Under
    /// `peers_lock`, reject the connection if the node is already
    /// connected/connecting (a duplicate — `io` is dropped, closing the
    /// socket, and NO actor is spawned), otherwise spawn the peer actor and
    /// register it in `connecting`. Returns whether the peer was admitted.
    fn admit_peer<IO>(
        self: &Arc<Self>,
        node_id: NodeId,
        cert: ava_crypto::staking::Certificate,
        direction: Direction,
        io: IO,
    ) -> bool
    where
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        let handle = {
            let _guard = self.peers_lock.lock();
            if self.connected.contains(&node_id) || self.connecting.contains(&node_id) {
                tracing::debug!(%node_id, "duplicate connection rejected");
                return false;
            }
            let handle = Peer::spawn(
                Arc::clone(&self.peer_config),
                node_id,
                cert,
                direction,
                io,
                &self.net_token,
                &self.tasks,
            );
            self.connecting.insert(handle.clone());
            handle
        };
        self.spawn_watcher(handle);
        true
    }

    /// Watch an admitted peer: promote it `connecting` → `connected` on
    /// handshake completion, remove it (notifying the router) on close. Every
    /// membership transition takes `peers_lock` so it is atomic w.r.t.
    /// `admit_peer`. (`admit_peer` already inserted the handle into
    /// `connecting`.)
    fn spawn_watcher(self: &Arc<Self>, handle: crate::peer::handle::PeerHandle) {
        let node = handle.node_id();
        let this = Arc::clone(self);
        self.tasks.spawn(async move {
            tokio::select! {
                () = handle.finished_handshake() => {
                    {
                        let _guard = this.peers_lock.lock();
                        this.connecting.remove(&node);
                        this.connected.insert(handle.clone());
                    }
                    tracing::debug!(%node, "rung 3: app handshake complete (promoted to connected)");
                    // Reconnect-backoff: reset the backoff for this peer so the
                    // next outbound dial (after disconnect) starts fresh.
                    if let Some(t) = this.tracked_ips.lock().get_mut(&node) {
                        t.record_success(Instant::now());
                    }
                    // metrics: a completed handshake (`times_connected`,
                    // `specs/18` §2.1).
                    if let Some(m) = &this.metrics {
                        m.observe_connected();
                    }
                }
                () = handle.closed() => {
                    {
                        let _guard = this.peers_lock.lock();
                        this.connecting.remove(&node);
                        this.connected.remove(&node);
                    }
                    tracing::debug!(%node, "rung 3: peer handshake failed / connection closed");
                    return;
                }
            }
            // Promoted to connected: now wait for it to close.
            handle.closed().await;
            {
                let _guard = this.peers_lock.lock();
                this.connecting.remove(&node);
                this.connected.remove(&node);
            }
            // metrics: a post-handshake disconnect (`times_disconnected`,
            // `specs/18` §2.1).
            if let Some(m) = &this.metrics {
                m.observe_disconnected();
            }
            this.peer_config.router.disconnected(node);
        });
    }

    /// Upgrade an accepted inbound TCP stream and spawn its peer actor.
    fn handle_accepted(self: &Arc<Self>, stream: TcpStream) {
        let this = Arc::clone(self);
        self.tasks.spawn(async move {
            match this.server_upgrader.upgrade(stream).await {
                Ok((node_id, tls, cert)) => {
                    this.admit_peer(node_id, cert, Direction::Inbound, tls);
                }
                Err(_) => {
                    // metrics: an inbound connection rejected at the TLS upgrade
                    // (unsupported leaf cert / failed handshake), Go
                    // `tls_conn_rejected` (`specs/18` §2.1).
                    if let Some(m) = &this.metrics {
                        m.observe_tls_conn_rejected();
                    }
                }
            }
        });
    }

    /// Dial `addr` (outbound), upgrade it, and spawn its peer actor.
    fn handle_dial(self: &Arc<Self>, node_id: NodeId, addr: SocketAddr) {
        let this = Arc::clone(self);
        self.tasks.spawn(async move {
            // Clear the in-flight mark on any exit path (Task-1 `select_dial_targets`
            // set it before spawning us).
            let _dial_guard = DialGuard {
                net: Arc::clone(&this),
                node: node_id,
            };
            let stream = match this.dialer.dial(addr).await {
                Ok(s) => {
                    tracing::debug!(%addr, "rung 1-2: outbound TCP+TLS dial connected");
                    s
                }
                Err(e) => {
                    // Surface the dial failure (TCP refused / unreachable) — Go
                    // logs the dialer error at debug. Previously swallowed, which
                    // hid live-interop bring-up failures (M9.15 D3).
                    tracing::debug!(%addr, error = %e, "rung 1: outbound dial failed");
                    return;
                }
            };
            match this.client_upgrader.upgrade(stream).await {
                Ok((node_id, tls, cert)) => {
                    tracing::debug!(%addr, %node_id, "rung 3: outbound peer upgraded (admitting)");
                    this.admit_peer(node_id, cert, Direction::Outbound, tls);
                }
                Err(e) => {
                    // The outbound TLS upgrade failed (e.g. a rustls↔Go TLS 1.3
                    // mutual-auth stall, a rejected leaf cert, or a closed
                    // connection). Previously this `Err` was swallowed, so the
                    // exact failing rung was invisible — the M9.15 live mixed-net
                    // handshake root-cause work depends on seeing it (D3).
                    tracing::debug!(%addr, error = %e, "outbound TLS upgrade failed");
                    if let Some(m) = &this.metrics {
                        m.observe_outbound_tls_conn_upgrade_failed();
                    }
                }
            }
        });
    }

    /// The accept loop (#1), gated by the conn-upgrade throttler (#3).
    async fn run_accept(self: &Arc<Self>, listener: TcpListener) {
        loop {
            let accepted = tokio::select! {
                biased;
                () = self.net_token.cancelled() => return,
                r = listener.accept() => r,
            };
            let (stream, peer_addr) = match accepted {
                Ok(v) => v,
                // Listener error (closed): stop accepting.
                Err(_) => return,
            };
            // Conn-upgrade throttle (#3): drop the TCP connection if refused.
            if !self.conn_upgrade_throttler.should_upgrade(peer_addr.ip()) {
                drop(stream);
                continue;
            }
            self.handle_accepted(stream);
        }
    }

    /// Scan the tracked-IP table for peers to (re)dial at `now`. Skips any node
    /// already `connected`, `connecting`, or with an in-flight dial (`dialing`),
    /// applies the reconnect backoff, records the attempt, and marks each
    /// selected node as in-flight before returning it. Factored out of the
    /// dialer loop so the in-flight guard is deterministically unit-testable.
    fn select_dial_targets(&self, now: Instant) -> Vec<(NodeId, SocketAddr)> {
        let mut tracked = self.tracked_ips.lock();
        let mut dialing = self.dialing.lock();
        let mut out = Vec::new();
        for (n, t) in tracked.iter_mut() {
            if self.connected.contains(n) || self.connecting.contains(n) || dialing.contains(n) {
                continue;
            }
            if !t.should_dial(now) {
                continue;
            }
            t.record_attempt(now);
            dialing.insert(*n);
            out.push((*n, t.addr));
        }
        out
    }

    /// The dialer loop (#2): periodically (re)dial tracked IPs we are not yet
    /// connected/connecting to.
    async fn run_dialer(self: &Arc<Self>) {
        let mut ticker = tokio::time::interval(DIAL_SCAN_INTERVAL);
        loop {
            tokio::select! {
                biased;
                () = self.net_token.cancelled() => return,
                _ = ticker.tick() => {
                    for (node, addr) in self.select_dial_targets(Instant::now()) {
                        self.handle_dial(node, addr);
                    }
                }
            }
        }
    }

    /// `runTimers` (#4): the peer-list pull / bloom-reset / uptime tickers.
    async fn run_timers(self: &Arc<Self>) {
        let mut pull = tokio::time::interval(PEER_LIST_PULL_GOSSIP_FREQ);
        let mut bloom_reset = tokio::time::interval(PEER_LIST_BLOOM_RESET_FREQ);
        loop {
            tokio::select! {
                biased;
                () = self.net_token.cancelled() => return,
                _ = pull.tick() => {
                    // Ask each connected peer for its peer list (debounced cap-1).
                    for handle in self.connected.handles() {
                        let _ = handle.start_send_get_peer_list();
                    }
                }
                _ = bloom_reset.tick() => {
                    // Bloom-filter reset cadence (full reset wiring is M2.20+);
                    // the tick exists so the loop matches the Go topology.
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl super::Network for NetworkImpl {
    async fn dispatch(self: Arc<Self>) -> Result<()> {
        let listener = self
            .listener
            .lock()
            .take()
            .ok_or_else(|| crate::Error::Io("network already dispatched".into()))?;

        let accept = {
            let this = Arc::clone(&self);
            self.tasks
                .spawn(async move { this.run_accept(listener).await })
        };
        let dialer = {
            let this = Arc::clone(&self);
            self.tasks.spawn(async move { this.run_dialer().await })
        };
        let timers = {
            let this = Arc::clone(&self);
            self.tasks.spawn(async move { this.run_timers().await })
        };

        // Wait for shutdown.
        self.net_token.cancelled().await;

        // Stop accepting new work, then drain everything.
        let _ = accept.await;
        let _ = dialer.await;
        let _ = timers.await;
        self.connecting.close_all();
        self.connected.close_all();
        self.tasks.close();
        self.tasks.wait().await;
        Ok(())
    }

    fn start_close(&self) {
        // Idempotent: cancel the network token (a parent of every peer token)
        // and close the listener.
        self.net_token.cancel();
        let _ = self.listener.lock().take();
        self.connecting.close_all();
        self.connected.close_all();
    }

    fn manually_track(&self, node_id: NodeId, ip: SocketAddr) {
        self.ip_tracker.manually_track(node_id, ip);
        self.tracked_ips.lock().insert(node_id, TrackedIp::new(ip));
    }

    fn peer_info(&self, node_ids: &[NodeId]) -> Vec<super::PeerInfo> {
        let ids = if node_ids.is_empty() {
            self.connected.node_ids()
        } else {
            node_ids.to_vec()
        };
        ids.into_iter()
            .filter(|n| self.connected.contains(n))
            .map(|node_id| super::PeerInfo {
                node_id,
                ip: self.listen_addr,
                version: self.peer_config.my_version.display(),
                is_ingress: false,
            })
            .collect()
    }

    fn node_uptime(&self) -> Result<super::UptimeResult> {
        Ok(super::UptimeResult::default())
    }

    fn send(
        &self,
        msg: ava_message::codec::OutboundMessage,
        cfg: super::SendConfig,
        _subnet: ava_types::id::Id,
        allower: &dyn super::Allower,
    ) -> std::collections::HashSet<NodeId> {
        let mut sent = std::collections::HashSet::new();
        for node in &cfg.node_ids {
            if !allower.is_allowed(node) {
                continue;
            }
            if let Some(handle) = self.connected.get(node)
                && handle.send(msg.clone())
            {
                sent.insert(*node);
            }
        }
        sent
    }

    fn gossip(
        &self,
        msg: ava_message::codec::OutboundMessage,
        _subnet: ava_types::id::Id,
        cfg: super::GossipConfig,
        allower: &dyn super::Allower,
    ) -> std::collections::HashSet<NodeId> {
        let mut sent = std::collections::HashSet::new();
        let limit = cfg
            .validators
            .saturating_add(cfg.non_validators)
            .saturating_add(cfg.peers);
        for handle in self.connected.handles() {
            if sent.len() >= limit && limit > 0 {
                break;
            }
            let node = handle.node_id();
            if !allower.is_allowed(&node) {
                continue;
            }
            if handle.send(msg.clone()) {
                sent.insert(node);
            }
        }
        sent
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::network::Network;
    use crate::network::testutil::TestNetwork;
    use ava_types::node_id::NodeId;

    /// Two admissions for the SAME node-id: the first wins and spawns exactly
    /// one peer actor; the second is rejected. This is the deterministic guard
    /// for the atomic dedup gate (M9.15 inbound at-most-once follow-up).
    #[tokio::test]
    async fn admit_peer_dedups_same_node_id() {
        let tn = TestNetwork::start().await;
        let net = tn.network();

        // Two independent certs — dedup keys on node-id, not on the cert.
        let mk_cert = || {
            ava_crypto::staking::parse_certificate(
                crate::Identity::generate()
                    .expect("generate identity")
                    .cert_der(),
            )
            .expect("parse certificate")
        };
        let node = NodeId::from_slice(&[9u8; 20]).expect("node id");

        let (io1, _b1) = tokio::io::duplex(64);
        let (io2, _b2) = tokio::io::duplex(64);

        let first = net.admit_peer(node, mk_cert(), Direction::Inbound, io1);
        let second = net.admit_peer(node, mk_cert(), Direction::Inbound, io2);

        assert!(first, "admit_peer: first admission for a fresh node wins");
        assert!(
            !second,
            "admit_peer: second admission for the same node is rejected"
        );
        assert_eq!(
            net.connecting.len(),
            1,
            "admit_peer: only one peer actor admitted"
        );

        net.start_close();
    }

    /// The `DialGuard` clears a node's in-flight mark when the dial task exits
    /// (dropped), re-opening it for a future scan. Guards the clear-path that
    /// keeps a peer from being permanently locked out after a dial completes.
    #[tokio::test]
    async fn dial_guard_clears_in_flight_mark_on_drop() {
        let tn = TestNetwork::start().await;
        let net = tn.network();

        let node = NodeId::from_slice(&[8u8; 20]).expect("node id");
        net.dialing.lock().insert(node);
        assert!(
            net.dialing.lock().contains(&node),
            "precondition: node marked in-flight"
        );

        {
            let _guard = DialGuard {
                net: Arc::clone(net),
                node,
            };
            assert!(net.dialing.lock().contains(&node), "guard alive: mark held");
        }
        assert!(
            !net.dialing.lock().contains(&node),
            "guard dropped: mark cleared"
        );

        net.start_close();
    }

    /// A node with an in-flight dial (marked in `dialing`) is NOT re-selected
    /// by the scan dialer even after its backoff window elapses — the guard
    /// that stops duplicate concurrent dials during a slow/stalling upgrade.
    /// Regression guard for the M9.15 run_dialer TOCTOU.
    #[tokio::test]
    async fn select_dial_targets_skips_in_flight_dials() {
        let tn = TestNetwork::start().await;
        let net = tn.network();

        let node = NodeId::from_slice(&[7u8; 20]).expect("node id");
        let addr: SocketAddr = "127.0.0.1:19651".parse().expect("addr");
        net.manually_track(node, addr);

        let t0 = Instant::now();

        // First scan: fresh tracked IP is dial-ready → selected and marked.
        let first = net.select_dial_targets(t0);
        assert_eq!(first, vec![(node, addr)], "fresh tracked ip is dialed once");
        assert!(
            net.dialing.lock().contains(&node),
            "selected node is marked in-flight"
        );

        // Backoff window (1-2s) has elapsed at t0+3s, so should_dial passes —
        // but the in-flight guard must still hold the node out. On current
        // code (no guard) this returns the node again: the RED assertion.
        let second = net.select_dial_targets(t0 + Duration::from_secs(3));
        assert!(second.is_empty(), "in-flight dial is not re-launched");

        // Dial completes (task cleared the mark): the node is dial-ready again.
        net.dialing.lock().remove(&node);
        let third = net.select_dial_targets(t0 + Duration::from_secs(6));
        assert_eq!(
            third,
            vec![(node, addr)],
            "re-dial once the in-flight guard clears"
        );

        net.start_close();
    }
}
