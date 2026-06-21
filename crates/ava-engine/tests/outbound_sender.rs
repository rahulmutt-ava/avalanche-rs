// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Tests for [`ava_engine::networking::sender::OutboundSender`] — the real
//! ava-network-backed `Sender` (Go `snow/networking/sender.sender`).
//!
//! Each engine `send_*` call must translate into the matching `proto/p2p` wire
//! message, addressed to the chosen recipients, and handed to
//! [`ava_network::network::Network::send`] / `gossip`. We drive it against a
//! recording mock `Network` and decode the marshaled bytes back to assert the
//! op, the recipients, the subnet, and every field of the wire message.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava_engine::common::sender::{SendConfig, Sender};
use ava_engine::networking::sender::OutboundSender;
use ava_message::codec::{MsgBuilder, OutboundMessage};
use ava_message::ops::Op;
use ava_message::proto::p2p;
use ava_network::network::{
    Allower, GossipConfig, Network, PeerInfo, SendConfig as NetSendConfig, UptimeResult,
};
use ava_types::id::Id;
use ava_types::node_id::NodeId;

/// One recorded outbound dispatch from the mock network.
#[derive(Clone)]
struct Recorded {
    msg: OutboundMessage,
    recipients: HashSet<NodeId>,
    subnet: Id,
    /// `true` if the dispatch came through `gossip` rather than `send`.
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
    fn last(&self) -> Recorded {
        self.sent
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a recorded send")
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

const TIMEOUT: Duration = Duration::from_secs(2);

fn chain_id() -> Id {
    Id::from_slice(&[7u8; 32]).unwrap()
}
fn subnet_id() -> Id {
    Id::from_slice(&[9u8; 32]).unwrap()
}
fn node(b: u8) -> NodeId {
    NodeId::from_slice(&[b; 20]).unwrap()
}

fn harness() -> (Arc<MockNetwork>, OutboundSender) {
    let net = Arc::new(MockNetwork::default());
    let sender = OutboundSender::new(
        net.clone(),
        Arc::new(AllowAll),
        chain_id(),
        subnet_id(),
        TIMEOUT,
    );
    (net, sender)
}

/// Decode an outbound message's bytes back into its `p2p` oneof variant.
fn decode(msg: &OutboundMessage) -> p2p::message::Message {
    MsgBuilder::default()
        .parse_inbound(&msg.bytes)
        .expect("parse_inbound")
        .message
}

#[test]
fn push_query_translates_to_wire_and_targets_all_nodes() {
    let (net, sender) = harness();
    let nodes: HashSet<NodeId> = [node(1), node(2)].into_iter().collect();

    sender.send_push_query(&nodes, 7, vec![0xAA, 0xBB], 42);

    let rec = net.last();
    assert!(!rec.via_gossip, "push_query goes through send, not gossip");
    assert_eq!(rec.msg.op, Op::PushQuery, "op");
    assert_eq!(rec.subnet, subnet_id(), "subnet");
    assert_eq!(rec.recipients, nodes, "recipients");

    let p2p::message::Message::PushQuery(q) = decode(&rec.msg) else {
        panic!("expected PushQuery variant");
    };
    assert_eq!(q.chain_id.as_ref(), chain_id().as_bytes(), "chain_id");
    assert_eq!(q.request_id, 7, "request_id");
    assert_eq!(q.container.as_ref(), &[0xAA, 0xBB], "container");
    assert_eq!(q.requested_height, 42, "requested_height");
    assert_eq!(
        q.deadline,
        TIMEOUT.as_nanos() as u64,
        "deadline = configured timeout (relative nanos)"
    );
}

#[test]
fn chits_translates_to_wire_and_targets_single_node() {
    let (net, sender) = harness();
    let preferred = Id::from_slice(&[1u8; 32]).unwrap();
    let preferred_at = Id::from_slice(&[2u8; 32]).unwrap();
    let accepted = Id::from_slice(&[3u8; 32]).unwrap();

    sender.send_chits(node(5), 11, preferred, preferred_at, accepted, 99);

    let rec = net.last();
    assert_eq!(rec.msg.op, Op::Chits, "op");
    assert_eq!(
        rec.recipients,
        [node(5)].into_iter().collect(),
        "single recipient"
    );

    let p2p::message::Message::Chits(c) = decode(&rec.msg) else {
        panic!("expected Chits variant");
    };
    assert_eq!(c.request_id, 11, "request_id");
    assert_eq!(
        c.preferred_id.as_ref(),
        preferred.as_bytes(),
        "preferred_id"
    );
    assert_eq!(
        c.preferred_id_at_height.as_ref(),
        preferred_at.as_bytes(),
        "preferred_id_at_height"
    );
    assert_eq!(c.accepted_id.as_ref(), accepted.as_bytes(), "accepted_id");
    assert_eq!(c.accepted_height, 99, "accepted_height");
}

#[test]
fn get_translates_to_wire() {
    let (net, sender) = harness();
    let container = Id::from_slice(&[4u8; 32]).unwrap();

    sender.send_get(node(8), 3, container);

    let rec = net.last();
    assert_eq!(rec.msg.op, Op::Get, "op");
    assert_eq!(
        rec.recipients,
        [node(8)].into_iter().collect(),
        "single recipient"
    );
    let p2p::message::Message::Get(g) = decode(&rec.msg) else {
        panic!("expected Get variant");
    };
    assert_eq!(g.request_id, 3, "request_id");
    assert_eq!(
        g.container_id.as_ref(),
        container.as_bytes(),
        "container_id"
    );
    assert_eq!(g.deadline, TIMEOUT.as_nanos() as u64, "deadline");
}

#[test]
fn accepted_frontier_translates_to_wire() {
    let (net, sender) = harness();
    let container = Id::from_slice(&[6u8; 32]).unwrap();

    sender.send_accepted_frontier(node(2), 1, container);

    let rec = net.last();
    assert_eq!(rec.msg.op, Op::AcceptedFrontier, "op");
    let p2p::message::Message::AcceptedFrontier(f) = decode(&rec.msg) else {
        panic!("expected AcceptedFrontier variant");
    };
    assert_eq!(f.request_id, 1, "request_id");
    assert_eq!(
        f.container_id.as_ref(),
        container.as_bytes(),
        "container_id"
    );
}

#[tokio::test]
async fn app_gossip_goes_through_gossip_path() {
    let (net, sender) = harness();
    let cfg = SendConfig {
        validators: 4,
        ..Default::default()
    };

    sender
        .send_app_gossip(cfg, vec![1, 2, 3])
        .await
        .expect("send_app_gossip");

    let rec = net.last();
    assert!(rec.via_gossip, "app_gossip must use the gossip path");
    assert_eq!(rec.msg.op, Op::AppGossip, "op");
    assert_eq!(rec.subnet, subnet_id(), "subnet");
    let p2p::message::Message::AppGossip(g) = decode(&rec.msg) else {
        panic!("expected AppGossip variant");
    };
    assert_eq!(g.app_bytes.as_ref(), &[1, 2, 3], "app_bytes");
}

#[tokio::test]
async fn app_request_targets_nodes_and_carries_deadline() {
    let (net, sender) = harness();
    let nodes: HashSet<NodeId> = [node(1)].into_iter().collect();

    sender
        .send_app_request(&nodes, 21, vec![9, 9])
        .await
        .expect("send_app_request");

    let rec = net.last();
    assert!(!rec.via_gossip, "app_request is a targeted send");
    assert_eq!(rec.msg.op, Op::AppRequest, "op");
    assert_eq!(rec.recipients, nodes, "recipients");
    let p2p::message::Message::AppRequest(r) = decode(&rec.msg) else {
        panic!("expected AppRequest variant");
    };
    assert_eq!(r.request_id, 21, "request_id");
    assert_eq!(r.app_bytes.as_ref(), &[9, 9], "app_bytes");
    assert_eq!(r.deadline, TIMEOUT.as_nanos() as u64, "deadline");
}
