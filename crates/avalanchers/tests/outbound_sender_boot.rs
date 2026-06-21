// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Node-assembly wire-up for the production [`OutboundSender`] (M9.15 STEP-q).
//!
//! [`avalanchers::wiring::chains::boot_chain_over_network`] boots one in-process
//! Snowman chain whose [`Sender`](ava_engine::common::sender::Sender) is the
//! **real** ava-network-backed
//! [`OutboundSender`](ava_engine::networking::sender::OutboundSender) — the
//! production replacement for the loopback/recording `RecordingSender`. This
//! test proves the production sender actually carries the bootstrapper's
//! outbound op out to the `Network`: with a self-beacon, the bootstrapper
//! broadcasts `GetAcceptedFrontier` to the beacon set through the `Sender`, and
//! we observe the marshaled `proto/p2p` wire message at a recording mock
//! `Network` (decoded back to assert the op, subnet, and recipient set).
//!
//! The live two-binary mixed-network arm (a real Go peer) is nightly-gated and
//! out of scope; the recording mock `Network` is the CI-runnable proof.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava_database::MemDb;
use ava_message::codec::{MsgBuilder, OutboundMessage};
use ava_message::ops::Op;
use ava_message::proto::p2p;
use ava_network::network::{
    Allower, GossipConfig, Network, PeerInfo, SendConfig as NetSendConfig, UptimeResult,
};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::testutil::TestVm;
use tokio_util::sync::CancellationToken;

use avalanchers::wiring::chains::boot_chain_over_network;

/// One recorded outbound dispatch from the mock network.
#[derive(Clone)]
struct Recorded {
    msg: OutboundMessage,
    recipients: HashSet<NodeId>,
    subnet: Id,
    via_gossip: bool,
}

/// An `Allower` that admits everyone (the primary-network case).
struct AllowAll;
impl Allower for AllowAll {
    fn is_allowed(&self, _node_id: &NodeId) -> bool {
        true
    }
}

#[derive(Default)]
struct MockNetwork {
    sent: Mutex<Vec<Recorded>>,
}

impl MockNetwork {
    fn sends(&self) -> Vec<Recorded> {
        self.sent.lock().unwrap().clone()
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
        cfg: NetSendConfig,
        subnet: Id,
        allower: &dyn Allower,
    ) -> HashSet<NodeId> {
        let recipients: HashSet<NodeId> = cfg
            .node_ids
            .iter()
            .filter(|n| allower.is_allowed(n))
            .copied()
            .collect();
        self.sent.lock().unwrap().push(Recorded {
            msg,
            recipients: recipients.clone(),
            subnet,
            via_gossip: false,
        });
        recipients
    }
    fn gossip(
        &self,
        msg: OutboundMessage,
        subnet: Id,
        _cfg: GossipConfig,
        _allower: &dyn Allower,
    ) -> HashSet<NodeId> {
        self.sent.lock().unwrap().push(Recorded {
            msg,
            recipients: HashSet::new(),
            subnet,
            via_gossip: true,
        });
        HashSet::new()
    }
}

/// Decode an outbound message's bytes back into its `p2p` oneof variant.
fn decode(msg: &OutboundMessage) -> p2p::message::Message {
    MsgBuilder::default()
        .parse_inbound(&msg.bytes)
        .expect("parse_inbound")
        .message
}

#[tokio::test]
async fn boot_over_network_carries_frontier_broadcast_out_to_the_network() {
    let network = Arc::new(MockNetwork::default());
    let allower: Arc<dyn Allower> = Arc::new(AllowAll);
    let token = CancellationToken::new();
    let subnet_id = ava_types::constants::PRIMARY_NETWORK_ID;
    let chain_id = Id::EMPTY;

    // Boot a chain with a self-beacon over the mock network. The bootstrapper
    // broadcasts `GetAcceptedFrontier` to the beacon set through the production
    // `OutboundSender`, which marshals it and hands it to `Network::send`.
    let handle = boot_chain_over_network(
        chain_id,
        subnet_id,
        Arc::clone(&network) as Arc<dyn Network>,
        Arc::clone(&allower),
        TestVm::new(),
        b"genesis",
        Arc::new(MemDb::new()),
        token.clone(),
    )
    .await
    .expect("boot_chain_over_network");

    // Poll the mock network's recorded sends until the frontier broadcast lands
    // (bounded, non-blocking — the handler task flips the engine asynchronously).
    let recorded = {
        let mut found = None;
        for _ in 0..200 {
            let sends = network.sends();
            if let Some(rec) = sends
                .iter()
                .find(|r| !r.via_gossip && r.msg.op == Op::GetAcceptedFrontier)
                .cloned()
            {
                found = Some(rec);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        found.expect("a GetAcceptedFrontier send within the timeout")
    };

    // The production sender carried the op out: right op, right subnet, and the
    // recipient set is the (self) beacon node set.
    assert_eq!(recorded.msg.op, Op::GetAcceptedFrontier, "op");
    assert_eq!(recorded.subnet, subnet_id, "subnet stamped from the chain");
    assert_eq!(
        recorded.recipients,
        handle.beacons.iter().copied().collect::<HashSet<NodeId>>(),
        "recipients are the beacon (self) node set"
    );
    assert!(
        !recorded.recipients.is_empty(),
        "self-beacon ⇒ at least one recipient"
    );

    // The wire message decodes back to a GetAcceptedFrontier carrying the chain id.
    let p2p::message::Message::GetAcceptedFrontier(f) = decode(&recorded.msg) else {
        panic!("expected GetAcceptedFrontier variant");
    };
    assert_eq!(
        f.chain_id.as_ref(),
        chain_id.as_bytes(),
        "chain id stamped into the wire message"
    );

    // Clean shutdown: cancel the token and await the handler task (no leak).
    token.cancel();
    let _ = handle.join.await;
}
