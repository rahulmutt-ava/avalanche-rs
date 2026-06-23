// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Beacon-connectivity gate for bootstrap start (M9.15 G2).
//!
//! Proves that [`boot_chain_over_network`] does **not** broadcast
//! `GetAcceptedFrontier` until the caller signals `on_sufficiently_connected =
//! true` via the injected `watch::Receiver<bool>`. This mirrors Go
//! `onSufficientlyConnected`: the bootstrapper's frontier discovery is held
//! until enough beacons have completed the handshake.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava_database::MemDb;
use ava_message::codec::OutboundMessage;
use ava_message::ops::Op;
use ava_network::network::{
    Allower, GossipConfig, Network, PeerInfo, SendConfig as NetSendConfig, UptimeResult,
};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::testutil::TestVm;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use avalanchers::wiring::chains::boot_chain_over_network;

/// An `Allower` that admits everyone.
struct AllowAll;
impl Allower for AllowAll {
    fn is_allowed(&self, _node_id: &NodeId) -> bool {
        true
    }
}

#[derive(Default)]
struct MockNetwork {
    sent: Mutex<Vec<Op>>,
}

impl MockNetwork {
    fn frontier_count(&self) -> usize {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|&&op| op == Op::GetAcceptedFrontier)
            .count()
    }
}

#[async_trait::async_trait]
impl Network for MockNetwork {
    async fn dispatch(self: Arc<Self>) -> ava_network::Result<()> {
        Ok(())
    }
    fn start_close(&self) {}
    fn manually_track(&self, _node_id: NodeId, _ip: SocketAddr) {}
    fn peer_info(&self, _node_ids: &[NodeId]) -> Vec<PeerInfo> {
        Vec::new()
    }
    fn node_uptime(&self) -> ava_network::Result<UptimeResult> {
        Ok(UptimeResult::default())
    }
    fn send(
        &self,
        msg: OutboundMessage,
        _cfg: NetSendConfig,
        _subnet: Id,
        _allower: &dyn Allower,
    ) -> HashSet<NodeId> {
        self.sent.lock().unwrap().push(msg.op);
        HashSet::new()
    }
    fn gossip(
        &self,
        msg: OutboundMessage,
        _subnet: Id,
        _cfg: GossipConfig,
        _allower: &dyn Allower,
    ) -> HashSet<NodeId> {
        self.sent.lock().unwrap().push(msg.op);
        HashSet::new()
    }
}

/// The bootstrapper must NOT send `GetAcceptedFrontier` before the connectivity
/// gate fires, and MUST send it once the gate is set to `true`.
#[tokio::test]
async fn bootstrap_waits_for_sufficient_beacons_before_frontier() {
    let network = Arc::new(MockNetwork::default());
    let allower: Arc<dyn Allower> = Arc::new(AllowAll);
    let token = CancellationToken::new();
    let subnet_id = ava_types::constants::PRIMARY_NETWORK_ID;
    let chain_id = Id::EMPTY;

    // Create a connectivity gate, initially false (no beacons connected yet).
    let (connected_tx, connected_rx) = watch::channel(false);

    // Boot the chain with a self-beacon and the connectivity gate.
    let handle = boot_chain_over_network(
        chain_id,
        subnet_id,
        Arc::clone(&network) as Arc<dyn Network>,
        Arc::clone(&allower),
        TestVm::new(),
        b"genesis",
        Arc::new(MemDb::new()),
        token.clone(),
        Some(connected_rx),
        None,
    )
    .await
    .expect("boot_chain_over_network");

    // Give the handler task a moment to run if the gate was not respected.
    // If start fires immediately we'd see GetAcceptedFrontier here.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Before connectivity signal: no GetAcceptedFrontier should have been sent.
    assert_eq!(
        network.frontier_count(),
        0,
        "bootstrapper must not send GetAcceptedFrontier before connectivity is signalled"
    );

    // Signal connectivity.
    connected_tx.send(true).expect("signal connectivity");

    // Now the bootstrapper starts and broadcasts the frontier request.
    let mut frontier_seen = false;
    for _ in 0..200 {
        if network.frontier_count() > 0 {
            frontier_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        frontier_seen,
        "bootstrapper must send GetAcceptedFrontier after connectivity is signalled"
    );

    // Clean shutdown.
    token.cancel();
    let _ = handle.join.await;
}
