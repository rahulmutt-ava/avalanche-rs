// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Five-beacon localhost-TLS regression gate for the `BeaconManager`
//! connectivity gate (M9.15 Task 4).
//!
//! Unlike `networked_bootstrap.rs` (which hand-fires the connectivity gate via
//! `connected_tx.send(true)`), this test wires the follower through the **real
//! production `BeaconManager`** via `wrap_with_beacon_gate`. The follower must
//! reach `NormalOp` without any manual gate fire — the gate must open
//! automatically once ≥ 4 of the 5 beacons have completed their TLS handshakes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ava_crypto::staking;
use ava_database::MemDb;
use ava_message::builder::Creator;
use ava_message::codec::MsgBuilder;
use ava_network::Identity;
use ava_network::config::PeerConfig;
use ava_network::metrics::Metrics as NetworkMetrics;
use ava_network::network::ip_tracker::IpTracker;
use ava_network::network::{Allower, Network, NetworkImpl};
use ava_network::peer::ip_signer::{Clock as PeerClock, IpSigner, SystemClock};
use ava_network::peer::metrics::PeerMetrics;
use ava_network::router::ExternalHandler;
use ava_network::throttling::inbound_msg_byte::InboundMsgByteThrottler;
use ava_network::throttling::outbound_msg::{OutboundMsgThrottler, OutboundMsgThrottlerConfig};
use ava_node::init::networking::{RouterBridge, wrap_with_beacon_gate};
use ava_snow::EngineState;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::ValidatorManager;
use ava_version::Application;
use ava_version::compatibility::Compatibility;
use ava_vm::testutil::TestVm;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use avalanchers::wiring::chains::{NetworkChainBootHandle, boot_chain_over_network};

/// An `Allower` that admits every node (the primary-network case).
struct AllowAll;
impl Allower for AllowAll {
    fn is_allowed(&self, _node_id: &NodeId) -> bool {
        true
    }
}

/// Reports weight 1 for the configured beacon node-ids; the only methods the
/// `BeaconManager` gate calls are `get_weight` and `num_validators`.
struct BeaconSet {
    members: HashSet<NodeId>,
}

impl ValidatorManager for BeaconSet {
    fn add_staker(
        &self,
        _s: Id,
        _n: NodeId,
        _pk: Option<ava_crypto::bls::PublicKey>,
        _tx: Id,
        _w: u64,
    ) -> ava_validators::error::Result<()> {
        unimplemented!()
    }

    fn add_weight(&self, _s: Id, _n: NodeId, _w: u64) -> ava_validators::error::Result<()> {
        unimplemented!()
    }

    fn remove_weight(&self, _s: Id, _n: NodeId, _w: u64) -> ava_validators::error::Result<()> {
        unimplemented!()
    }

    fn get_weight(&self, _s: Id, node_id: NodeId) -> u64 {
        u64::from(self.members.contains(&node_id))
    }

    fn get_validator(&self, _s: Id, _n: NodeId) -> Option<ava_validators::Validator> {
        unimplemented!()
    }

    fn get_validator_ids(&self, _s: Id) -> Vec<NodeId> {
        unimplemented!()
    }

    fn subset_weight(&self, _s: Id, _ids: &HashSet<NodeId>) -> ava_validators::error::Result<u64> {
        unimplemented!()
    }

    fn total_weight(&self, _s: Id) -> ava_validators::error::Result<u64> {
        unimplemented!()
    }

    fn num_validators(&self, _s: Id) -> usize {
        self.members.len()
    }

    fn num_subnets(&self) -> usize {
        unimplemented!()
    }

    fn sample(&self, _s: Id, _n: usize) -> ava_validators::error::Result<Vec<NodeId>> {
        unimplemented!()
    }

    fn register_callback_listener(
        &self,
        _s: Id,
        _l: Arc<dyn ava_validators::ManagerCallbackListener>,
    ) {
    }
}

/// A live `NetworkImpl` bound to a loopback port whose consensus handler is a
/// [`RouterBridge`], so inbound peer messages are decoded into engine ops and
/// routed to the booted chain's engine (M9.15 Task 3).
struct Node {
    network: Arc<NetworkImpl>,
    node_id: NodeId,
    listen_addr: SocketAddr,
    bridge: Arc<RouterBridge>,
    /// `Some` when this node was started with a beacon set: the real
    /// `BeaconManager` gate receiver, to pass as the chain's `start_gate`.
    gate: Option<tokio::sync::watch::Receiver<bool>>,
}

impl Node {
    /// Bring up a real `NetworkImpl` on `127.0.0.1:0` with a fresh ECDSA staking
    /// identity, modeled on `ava_network::network::testutil::TestNetwork::start`
    /// but with a [`RouterBridge`] as the network→consensus handler.
    ///
    /// When `gate_beacons` is `Some`, the network's consensus handler is the
    /// **real production `BeaconManager`** gate (wrapping the bridge) — exactly
    /// the `init_networking` wiring. When `None`, the bare bridge is used (a
    /// beacon node that needs no gate).
    async fn start(gate_beacons: Option<Arc<dyn ValidatorManager>>) -> Node {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let listen_addr = listener.local_addr().expect("local addr");

        // A generated ECDSA staking identity (Go parity: ECDSA, not RSA).
        let identity = Identity::generate().expect("identity");
        let cert = staking::parse_certificate(identity.cert_der()).expect("cert");
        let node_id = staking::node_id_from_cert(&cert.raw);

        let bls = Arc::new(ava_crypto::bls::LocalSigner::generate().expect("bls"));
        let clock: Arc<dyn PeerClock> = Arc::new(SystemClock);
        let ip_signer = Arc::new(IpSigner::new(identity.clone(), bls, clock.clone()));
        let creator = Arc::new(Creator::new(MsgBuilder::default()));

        let bridge = Arc::new(RouterBridge::new());

        // When a beacon set is supplied, the network's consensus handler is the
        // REAL production BeaconManager gate (wrapping the bridge) — exactly the
        // init_networking wiring. Otherwise the bare bridge (a beacon node).
        let (consensus_handler, gate): (Arc<dyn ExternalHandler>, Option<_>) = match gate_beacons {
            Some(beacons) => {
                let (h, rx) =
                    wrap_with_beacon_gate(Arc::clone(&bridge) as Arc<dyn ExternalHandler>, beacons);
                (h, Some(rx))
            }
            None => (Arc::clone(&bridge) as Arc<dyn ExternalHandler>, None),
        };

        // Upgrade far in the future: the pre-upgrade floor applies (matches the
        // network testutil), so the handshake uses the stable compatibility.
        let upgrade_time = std::time::UNIX_EPOCH + Duration::from_secs(4_000_000_000);
        let compat = Arc::new(Compatibility::new(
            Application::new("avalanchego", 1, 14, 2),
            Application::new("avalanchego", 1, 14, 0),
            Application::new("avalanchego", 1, 13, 0),
            upgrade_time,
        ));

        let outbound = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig::default());
        let inbound = Arc::new(InboundMsgByteThrottler::new(
            32 * 1024 * 1024,
            6 * 1024 * 1024,
            2 * 1024 * 1024,
        ));
        let ip_tracker = Arc::new(IpTracker::new());

        let registry = prometheus::Registry::new();
        let metrics = NetworkMetrics::new(&registry).expect("network metrics");
        let peer_metrics = PeerMetrics::new(&registry).expect("peer metrics");
        inbound.set_metrics(&metrics);

        let peer_config = Arc::new(
            PeerConfig::new(
                1,
                node_id,
                identity,
                listen_addr,
                Application::new("avalanchego", 1, 14, 2),
                creator,
                Arc::clone(&consensus_handler),
                compat,
                ip_signer,
                outbound,
                inbound,
                ip_tracker,
                clock,
            )
            .with_peer_metrics(peer_metrics),
        );

        let network =
            NetworkImpl::new_with_metrics(peer_config, listener, metrics).expect("network");

        Node {
            network,
            node_id,
            listen_addr,
            bridge,
            gate,
        }
    }
}

/// Poll `cond` every 50ms until it returns `true` or `timeout` elapses.
async fn wait_until_timeout<F: FnMut() -> bool>(timeout: Duration, mut cond: F) -> bool {
    tokio::time::timeout(timeout, async {
        loop {
            if cond() {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or(false)
}

/// Five beacon nodes + one follower over real localhost TLS. The follower's
/// connectivity gate is the production `BeaconManager` (required_conns = 4), so
/// this exercises the real gate path end-to-end — the coverage missing from
/// `networked_bootstrap.rs` (which hand-fires the gate). The follower must reach
/// `NormalOp` and converge on the beacons' tip.
///
/// This exercises the real `BeaconManager` connectivity gate end-to-end (the
/// coverage `networked_bootstrap.rs` lacks, since it hand-fires the gate). The
/// bootstrapper tolerates a beacon that has not yet handshaked when the gate
/// fires at `required_conns` (< all beacons): the missing frontier reply is
/// recovered via the request-timeout-synthesized `GetAcceptedFrontierFailed`
/// (see `ava-engine` bootstrap failure accounting), so the follower reaches
/// `NormalOp` deterministically.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn follower_bootstraps_through_real_beacon_gate() {
    const TIP_HEIGHT: u64 = 3;
    const N_BEACONS: usize = 5;
    let allower: Arc<dyn Allower> = Arc::new(AllowAll);

    // ---- Bring up 5 beacon nodes (no gate). ----
    let mut beacons = Vec::new();
    for _ in 0..N_BEACONS {
        beacons.push(Node::start(None).await);
    }

    // ---- Boot each beacon's chain: beaconless TestVm at TIP_HEIGHT so its
    // Getter answers. Keep handles/tokens/observers alive for the test. ----
    let mut beacon_tokens = Vec::new();
    let mut beacon_handles = Vec::new();
    let mut beacon_obs = Vec::new();
    for node in &beacons {
        let token = CancellationToken::new();
        let vm = TestVm::resuming_at_height(TIP_HEIGHT);
        beacon_obs.push(vm.observer());
        let handle = boot_chain_over_network(
            Id::EMPTY,
            ava_types::constants::PRIMARY_NETWORK_ID,
            Arc::clone(&node.network) as Arc<dyn Network>,
            Arc::clone(&allower),
            vm,
            b"genesis",
            Arc::new(MemDb::new()),
            token.clone(),
            None,                  // beacon: no gate.
            Some(BTreeMap::new()), // beaconless ⇒ short-circuit to NormalOp.
            None,
            None, // T16: params `None` ⇒ k=1 (byte-identical pre-fix boot).
        )
        .await
        .expect("boot beacon");
        node.bridge.set_engine_router(Arc::clone(&handle.router));
        beacon_tokens.push(token);
        beacon_handles.push(handle);
    }

    // ---- Bring up the follower WITH the real beacon-gate over all 5 beacons. ----
    let beacon_ids: HashSet<NodeId> = beacons.iter().map(|b| b.node_id).collect();
    let follower = Node::start(Some(Arc::new(BeaconSet {
        members: beacon_ids.clone(),
    }) as Arc<dyn ValidatorManager>))
    .await;

    // Follower pins all 5 beacon addresses (the backoff dialer).
    for b in &beacons {
        follower.network.manually_track(b.node_id, b.listen_addr);
    }

    // Boot the follower's chain: fresh TestVm, all 5 beacons as bootstrappers,
    // gated on the REAL BeaconManager gate.
    let follower_token = CancellationToken::new();
    let follower_vm = TestVm::new();
    let follower_obs = follower_vm.observer();
    let mut frontier_beacons = BTreeMap::new();
    for id in &beacon_ids {
        frontier_beacons.insert(*id, 1u64);
    }
    let follower_handle: NetworkChainBootHandle = boot_chain_over_network(
        Id::EMPTY,
        ava_types::constants::PRIMARY_NETWORK_ID,
        Arc::clone(&follower.network) as Arc<dyn Network>,
        Arc::clone(&allower),
        follower_vm,
        b"genesis",
        Arc::new(MemDb::new()),
        follower_token.clone(),
        follower.gate.clone(), // REAL BeaconManager gate (the point of this test).
        Some(frontier_beacons),
        None,
        None, // T16: params `None` ⇒ k=1 (byte-identical pre-fix boot).
    )
    .await
    .expect("boot follower");
    follower
        .bridge
        .set_engine_router(Arc::clone(&follower_handle.router));

    // ---- Drive all network event loops. ----
    let mut dispatches = Vec::new();
    for node in &beacons {
        let net = Arc::clone(&node.network);
        dispatches.push(tokio::spawn(async move { net.dispatch().await }));
    }
    {
        let net = Arc::clone(&follower.network);
        dispatches.push(tokio::spawn(async move { net.dispatch().await }));
    }

    // ---- The follower handshakes ≥4 of 5 beacons ⇒ the real gate fires ⇒
    // bootstrapper broadcasts GetAcceptedFrontier ⇒ reaches NormalOp. ----
    // 120s: 6-node TLS bring-up needs cold-runtime/CI headroom (warm runs complete <1s).
    let finished = wait_until_timeout(Duration::from_secs(120), || {
        matches!(**follower_handle.ctx.state.load(), EngineState::NormalOp)
    })
    .await;
    assert!(
        finished,
        "follower did NOT reach NormalOp within 120s through the real beacon gate"
    );

    // ---- Convergence: follower tip == a beacon's tip. ----
    assert_eq!(
        follower_obs.last_accepted_height(),
        TIP_HEIGHT,
        "follower last_accepted_height did not converge to the beacon tip height"
    );
    assert_eq!(
        follower_obs.last_accepted_id(),
        beacon_obs[0].last_accepted_id(),
        "follower last_accepted_id did not converge to the beacon tip id"
    );

    // ---- Clean shutdown. ----
    follower.network.start_close();
    for b in &beacons {
        b.network.start_close();
    }
    follower_token.cancel();
    for t in &beacon_tokens {
        t.cancel();
    }
    for d in dispatches {
        let _ = tokio::time::timeout(Duration::from_secs(10), d).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), follower_handle.join).await;
    for h in beacon_handles {
        let _ = tokio::time::timeout(Duration::from_secs(5), h.join).await;
    }
}
