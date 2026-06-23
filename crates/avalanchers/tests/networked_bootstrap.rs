// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Two-node localhost-TLS bootstrap-to-Finished integration test (M9.15 G5).
//!
//! The capstone of the M9.15 "networked bootstrap" plan: it exercises Tasks 1-4
//! end-to-end over a **real** localhost TLS link between two
//! [`NetworkImpl`](ava_network::network::NetworkImpl) instances.
//!
//! * Node **A** is a beacon: a [`TestVm`] seeded with `N ≥ 2` accepted blocks,
//!   booted **beaconless** so its engine short-circuits to `NormalOp` and its
//!   `Getter` answers `GetAcceptedFrontier` / `GetAccepted` / `GetAncestors` /
//!   `Get` (Task 2).
//! * Node **B** is a follower: a fresh `TestVm` at genesis, with A as its **sole
//!   bootstrap beacon**. B `manually_track`s A (the backoff dialer, Task 1) and
//!   waits for the **connectivity gate** (Task 4) before broadcasting its
//!   frontier request.
//!
//! Inbound wire messages are decoded into engine ops by the
//! [`RouterBridge`](ava_node::init::networking::RouterBridge) (Task 3) and routed
//! to each node's chain engine. The test drives both network event loops and
//! polls B until its engine reaches `NormalOp` (the bootstrapper's `Finished`
//! state), then asserts B converged on A's tip.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
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
use ava_node::init::networking::RouterBridge;
use ava_snow::EngineState;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::Application;
use ava_version::compatibility::Compatibility;
use ava_vm::testutil::{TestVm, TestVmObserver};
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

/// A live `NetworkImpl` bound to a loopback port whose consensus handler is a
/// [`RouterBridge`], so inbound peer messages are decoded into engine ops and
/// routed to the booted chain's engine (M9.15 Task 3).
struct Node {
    network: Arc<NetworkImpl>,
    node_id: NodeId,
    listen_addr: SocketAddr,
    bridge: Arc<RouterBridge>,
}

impl Node {
    /// Bring up a real `NetworkImpl` on `127.0.0.1:0` with a fresh ECDSA staking
    /// identity, modeled on `ava_network::network::testutil::TestNetwork::start`
    /// but with a [`RouterBridge`] as the network→consensus handler.
    async fn start() -> Node {
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
                Arc::clone(&bridge) as Arc<dyn ExternalHandler>,
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

/// Follower B bootstraps from beacon A over real localhost TLS, reaching the
/// bootstrapper's `Finished` (engine `NormalOp`) state with `B.last_accepted ==
/// A.tip`.
#[tokio::test]
async fn follower_bootstraps_from_beacon_to_finished() {
    // The synthetic accepted chain on A: genesis → … → height H (H ≥ 2).
    const TIP_HEIGHT: u64 = 3;
    let allower: Arc<dyn Allower> = Arc::new(AllowAll);

    // ---- Bring up two real networks on loopback. ----
    let a = Node::start().await;
    let b = Node::start().await;

    // B learns A's listen address and pins it (the backoff dialer, Task 1).
    b.network.manually_track(a.node_id, a.listen_addr);

    // ---- Boot A's chain: a TestVm with TIP_HEIGHT accepted blocks, beaconless
    // (Some(empty) ⇒ Bootstrapping → NormalOp), so its Getter answers. ----
    let a_token = CancellationToken::new();
    let a_vm = TestVm::resuming_at_height(TIP_HEIGHT);
    let a_obs: TestVmObserver = a_vm.observer();
    let a_handle: NetworkChainBootHandle = boot_chain_over_network(
        Id::EMPTY,
        ava_types::constants::PRIMARY_NETWORK_ID,
        Arc::clone(&a.network) as Arc<dyn Network>,
        Arc::clone(&allower),
        a_vm,
        b"genesis",
        Arc::new(MemDb::new()),
        a_token.clone(),
        None,                  // A: no connectivity gate — start immediately.
        Some(BTreeMap::new()), // A: beaconless ⇒ short-circuit to NormalOp.
    )
    .await
    .expect("boot A");

    // Wire A's chain router into A's network bridge so inbound peer ops reach A's
    // engine (Task 3 → Getter, Task 2).
    a.bridge.set_engine_router(Arc::clone(&a_handle.router));

    // ---- Boot B's chain: fresh TestVm at genesis, A as its SOLE beacon, gated
    // on B's connectivity (Task 4): the bootstrapper waits for the handshake to
    // A before broadcasting GetAcceptedFrontier. ----
    let b_token = CancellationToken::new();
    let b_vm = TestVm::new();
    let b_obs: TestVmObserver = b_vm.observer();

    // The connectivity gate: fires `true` once B has handshaken to its single
    // beacon (A). Built here (rather than the full `init_networking` BeaconManager)
    // so the test wires the gate explicitly; B's bridge does not need it because
    // A is B's only beacon and we drive the gate off the live connection below.
    let (connected_tx, connected_rx) = tokio::sync::watch::channel(false);

    let mut beacons = BTreeMap::new();
    beacons.insert(a.node_id, 1u64);

    let b_handle: NetworkChainBootHandle = boot_chain_over_network(
        Id::EMPTY,
        ava_types::constants::PRIMARY_NETWORK_ID,
        Arc::clone(&b.network) as Arc<dyn Network>,
        Arc::clone(&allower),
        b_vm,
        b"genesis",
        Arc::new(MemDb::new()),
        b_token.clone(),
        Some(connected_rx), // B: REAL connectivity gate (Task 4).
        Some(beacons),      // B: A is the sole bootstrap beacon.
    )
    .await
    .expect("boot B");

    b.bridge.set_engine_router(Arc::clone(&b_handle.router));

    // ---- Drive both network event loops (accept loop + dialer + timers). ----
    let a_dispatch = {
        let net = Arc::clone(&a.network);
        tokio::spawn(async move { net.dispatch().await })
    };
    let b_dispatch = {
        let net = Arc::clone(&b.network);
        tokio::spawn(async move { net.dispatch().await })
    };

    // ---- Rung 1: handshake. Wait until B sees A connected, then fire the
    // connectivity gate (Task 4) so B begins frontier discovery. ----
    let handshaken = wait_until_timeout(Duration::from_secs(20), || {
        b.network.connected_peers().contains(&a.node_id)
    })
    .await;
    assert!(handshaken, "B handshakes to beacon A over localhost TLS");

    connected_tx.send(true).expect("fire connectivity gate");

    // ---- Rungs 2-4: B broadcasts GetAcceptedFrontier → A answers → B fetches
    // ancestors → B reaches NormalOp (Phase::Finished). ----
    let finished = wait_until_timeout(Duration::from_secs(30), || {
        matches!(**b_handle.ctx.state.load(), EngineState::NormalOp)
    })
    .await;
    assert!(
        finished,
        "follower B reached the bootstrapper's Finished state"
    );

    // ---- Convergence: B's last-accepted block == A's tip. ----
    assert_eq!(
        b_obs.last_accepted_height(),
        TIP_HEIGHT,
        "follower B accepted up to the beacon's tip height"
    );
    assert_eq!(
        b_obs.last_accepted_id(),
        a_obs.last_accepted_id(),
        "follower B converged on beacon A's tip"
    );

    // ---- Clean shutdown. ----
    a.network.start_close();
    b.network.start_close();
    a_token.cancel();
    b_token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), a_dispatch).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), b_dispatch).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), a_handle.join).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), b_handle.join).await;
}
