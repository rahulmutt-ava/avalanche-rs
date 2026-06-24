// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Phase 1 loop-closing proof (M9.15 production network→consensus wiring).
//!
//! A chain booted over the production seam — the node's shared [`ChainRouter`]
//! (the one a `RouterBridge` routes inbound to) + a real `OutboundSender` over a
//! recording `Network` — answers an inbound `GetAcceptedFrontier` delivered
//! through that shared router. Proves the inbound loop is closed (peer op →
//! engine) AND the outbound reply leaves via the production sender, with no Go
//! node.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava_database::MemDb;
use ava_engine::networking::router::{ChainRouter, InboundMessage, InboundOp, Router};
use ava_engine::networking::timeout::{AdaptiveTimeoutConfig, AdaptiveTimeoutManager};
use ava_message::codec::OutboundMessage;
use ava_message::ops::Op;
use ava_network::network::{
    Allower, GossipConfig, Network, PeerInfo, SendConfig as NetSendConfig, UptimeResult,
};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, RealClock};
use ava_vm::testutil::TestVm;
use tokio_util::sync::CancellationToken;

use avalanchers::wiring::chains::boot_chain_over_network_core_for_test;

#[derive(Clone)]
struct Recorded {
    op: Op,
    recipients: HashSet<NodeId>,
}

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
        _subnet: Id,
        allower: &dyn Allower,
    ) -> HashSet<NodeId> {
        let recipients: HashSet<NodeId> = cfg
            .node_ids
            .iter()
            .filter(|n| allower.is_allowed(n))
            .copied()
            .collect();
        self.sent.lock().unwrap().push(Recorded {
            op: msg.op,
            recipients: recipients.clone(),
        });
        recipients
    }
    fn gossip(
        &self,
        _msg: OutboundMessage,
        _subnet: Id,
        _cfg: GossipConfig,
        _allower: &dyn Allower,
    ) -> HashSet<NodeId> {
        HashSet::new()
    }
}

#[tokio::test]
async fn inbound_frontier_request_routes_to_a_production_booted_chain() {
    let chain_id = Id::EMPTY;
    let subnet_id = ava_types::constants::PRIMARY_NETWORK_ID;
    let network = Arc::new(MockNetwork::default());
    let allower: Arc<dyn Allower> = Arc::new(AllowAll);
    let token = CancellationToken::new();

    // The node's ONE shared router (mirrors init_chain_manager). A RouterBridge
    // would `set_engine_router(router.clone())`; delivering through the router is
    // exactly what the bridge does after decoding inbound bytes.
    let clock: Arc<dyn Clock> = Arc::new(RealClock);
    let timeouts = Arc::new(
        AdaptiveTimeoutManager::new(
            &AdaptiveTimeoutConfig {
                initial_timeout: Duration::from_secs(2),
                minimum_timeout: Duration::from_secs(2),
                maximum_timeout: Duration::from_secs(10),
                timeout_coefficient: 1.0,
                timeout_halflife: Duration::from_secs(5),
            },
            Arc::clone(&clock),
        )
        .unwrap(),
    );
    let router = ChainRouter::new(timeouts);

    // Boot a beaconless beacon chain (Some(empty) ⇒ short-circuit to NormalOp,
    // so its Getter answers Get*), with N>=2 accepted blocks.
    let vm = TestVm::resuming_at_height(3);
    let handle = boot_chain_over_network_core_for_test(
        chain_id,
        subnet_id,
        Arc::clone(&router),
        Arc::clone(&clock),
        Arc::clone(&network) as Arc<dyn Network>,
        Arc::clone(&allower),
        vm,
        b"genesis",
        Arc::new(MemDb::new()),
        token.clone(),
        Some(BTreeMap::new()),
    )
    .await
    .expect("boot");

    // Wait until the chain reaches NormalOp (Getter active).
    let peer = NodeId::from_slice(&[7u8; 20]).expect("peer node id");
    let reply = {
        let mut found = None;
        for _ in 0..300 {
            // Deliver an inbound GetAcceptedFrontier through the shared router.
            router
                .handle_inbound(InboundMessage {
                    node: peer,
                    chain: chain_id,
                    op: InboundOp::GetAcceptedFrontier { request_id: 1 },
                })
                .await;
            if let Some(r) = network
                .sends()
                .into_iter()
                .find(|r| r.op == Op::AcceptedFrontier && r.recipients.contains(&peer))
            {
                found = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        found.expect("an AcceptedFrontier reply to the requesting peer")
    };

    assert_eq!(reply.op, Op::AcceptedFrontier, "reply op");
    assert!(
        reply.recipients.contains(&peer),
        "reply addressed back to the requesting peer"
    );

    token.cancel();
    let _ = handle.join.await;
}
