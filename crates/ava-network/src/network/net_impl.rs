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

    /// Spawn the bookkeeping that promotes a peer to `connected` on handshake
    /// completion and removes it (notifying the router) on close.
    fn watch_peer(self: &Arc<Self>, handle: crate::peer::handle::PeerHandle) {
        let node = handle.node_id();
        self.connecting.insert(handle.clone());

        let this = Arc::clone(self);
        self.tasks.spawn(async move {
            tokio::select! {
                () = handle.finished_handshake() => {
                    this.connecting.remove(&node);
                    this.connected.insert(handle.clone());
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
                    this.connecting.remove(&node);
                    this.connected.remove(&node);
                    return;
                }
            }
            // Promoted to connected: now wait for it to close.
            handle.closed().await;
            this.connecting.remove(&node);
            this.connected.remove(&node);
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
            let upgraded = this.server_upgrader.upgrade(stream).await;
            match upgraded {
                Ok((node_id, tls, cert)) => {
                    let handle = Peer::spawn(
                        Arc::clone(&this.peer_config),
                        node_id,
                        cert,
                        Direction::Inbound,
                        tls,
                        &this.net_token,
                        &this.tasks,
                    );
                    this.watch_peer(handle);
                }
                Err(_) => {
                    // metrics: an inbound connection rejected at the TLS upgrade
                    // (unsupported leaf cert / failed handshake), Go
                    // `tls_conn_rejected` (`specs/18` §2.1). Mirrors Go's
                    // listener upgrade-failure counter.
                    if let Some(m) = &this.metrics {
                        m.observe_tls_conn_rejected();
                    }
                }
            }
        });
    }

    /// Dial `addr` (outbound), upgrade it, and spawn its peer actor.
    fn handle_dial(self: &Arc<Self>, addr: SocketAddr) {
        let this = Arc::clone(self);
        self.tasks.spawn(async move {
            let stream = match this.dialer.dial(addr).await {
                Ok(s) => s,
                Err(_) => return,
            };
            let upgraded = this.client_upgrader.upgrade(stream).await;
            if let Ok((node_id, tls, cert)) = upgraded {
                // Avoid a duplicate if already connected/connecting.
                if this.connected.contains(&node_id) || this.connecting.contains(&node_id) {
                    return;
                }
                let handle = Peer::spawn(
                    Arc::clone(&this.peer_config),
                    node_id,
                    cert,
                    Direction::Outbound,
                    tls,
                    &this.net_token,
                    &this.tasks,
                );
                this.watch_peer(handle);
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

    /// The dialer loop (#2): periodically (re)dial tracked IPs we are not yet
    /// connected/connecting to.
    async fn run_dialer(self: &Arc<Self>) {
        let mut ticker = tokio::time::interval(DIAL_SCAN_INTERVAL);
        loop {
            tokio::select! {
                biased;
                () = self.net_token.cancelled() => return,
                _ = ticker.tick() => {
                    let now = Instant::now();
                    let targets: Vec<(NodeId, SocketAddr)> = {
                        let mut tracked = self.tracked_ips.lock();
                        let mut out = Vec::new();
                        for (n, t) in tracked.iter_mut() {
                            if self.connected.contains(n) || self.connecting.contains(n) {
                                continue;
                            }
                            if !t.should_dial(now) {
                                continue;
                            }
                            t.record_attempt(now);
                            out.push((*n, t.addr));
                        }
                        out
                    };
                    for (node, addr) in targets {
                        let _ = node;
                        self.handle_dial(addr);
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
