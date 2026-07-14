// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 `*Failed`-delivery integration tests (frontier stall rung; see
//! `docs/superpowers/specs/2026-07-14-m9.15-frontier-failed-delivery-design.md`).
//!
//! Invariant under test (Go's sender/router contract): **every registered
//! request yields exactly one of {response, `*Failed`} delivered to the owning
//! engine** — within the adaptive timeout for silent loss, and (for the
//! fetch/query ops, Go parity) *immediately* for sends the network layer
//! reports as not delivered.
//!
//! Unlike the `MockClock`-driven unit tests (`timeout.rs` / `router.rs`), these
//! tests assemble the **production** delivery chain under `RealClock`:
//! `AdaptiveTimeoutManager` → `ChainRouter` → chain handler sink →
//! `BootstrapperEngineAdapter`/`Bootstrapper`, with a real `OutboundSender`
//! over a recording mock `ava_network::network::Network` — the exact path the
//! live 5-beacon `mixed_network` run exercises.

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use ava_engine::networking::router::op;
use ava_engine::networking::{
    AdaptiveTimeoutConfig, AdaptiveTimeoutManager, BootstrapperEngineAdapter, ChainHandler,
    ChainMessageSink, ChainRouter, EngineManager, InboundMessage, InboundOp, OutboundSender,
    Router, transition_channel,
};
use ava_message::codec::{MsgBuilder, OutboundMessage};
use ava_message::proto::p2p;
use ava_network::network::{
    Allower, GossipConfig, Network, PeerInfo, SendConfig as NetSendConfig, UptimeResult,
};
use ava_snow::ConsensusContext;
use ava_snow::acceptor::NoOpAcceptor;
use ava_snow::state::{EngineState, EngineType};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, RealClock};
use ava_vm::block::ChainVm;
use ava_vm::testutil::{TestVm, init_test_vm, test_chain_context};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn chain_id() -> Id {
    Id::from([0x51; 32])
}
fn subnet_id() -> Id {
    Id::from([0x52; 32])
}
fn node(b: u8) -> NodeId {
    NodeId::from([b; 20])
}

/// An `Allower` that admits everyone (the primary-network case).
struct AllowAll;
impl Allower for AllowAll {
    fn is_allowed(&self, _node_id: &NodeId) -> bool {
        true
    }
}

/// One recorded outbound dispatch from the mock network.
#[derive(Clone)]
struct Recorded {
    msg: OutboundMessage,
    recipients: HashSet<NodeId>,
}

/// A recording mock `Network` implementing the production `send` contract: the
/// returned set is the nodes the message was actually queued to. Nodes listed
/// in `unsent` are reported as NOT sent (the live go1 connect-snapshot race).
#[derive(Default)]
struct MockNetwork {
    sent: Mutex<Vec<Recorded>>,
    unsent: HashSet<NodeId>,
}

impl MockNetwork {
    fn with_unsent(unsent: HashSet<NodeId>) -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            unsent,
        }
    }

    fn snapshot(&self) -> Vec<Recorded> {
        self.sent.lock().expect("mock network lock").clone()
    }

    /// Decoded p2p variants of everything recorded so far.
    fn decoded(&self) -> Vec<p2p::message::Message> {
        self.snapshot().iter().map(|r| decode(&r.msg)).collect()
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
            .filter(|n| allower.is_allowed(n) && !self.unsent.contains(n))
            .copied()
            .collect();
        self.sent.lock().expect("mock network lock").push(Recorded {
            msg,
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

/// Decode an outbound message's bytes back into its `p2p` oneof variant.
fn decode(msg: &OutboundMessage) -> p2p::message::Message {
    MsgBuilder::default()
        .parse_inbound(&msg.bytes)
        .expect("parse_inbound")
        .message
}

/// A `ChainMessageSink` that records every pushed op (the narrow-link probe).
#[derive(Default)]
struct RecordingSink {
    pushed: Mutex<Vec<(NodeId, InboundOp)>>,
}

#[async_trait::async_trait]
impl ChainMessageSink for RecordingSink {
    async fn push(&self, node: NodeId, op: InboundOp) {
        self.pushed
            .lock()
            .expect("recording sink lock")
            .push((node, op));
    }
}

impl RecordingSink {
    fn snapshot(&self) -> Vec<(NodeId, InboundOp)> {
        self.pushed.lock().expect("recording sink lock").clone()
    }
}

/// The short configured timeout every test uses (production is 5s; the test
/// scales it down so "within ~5× the timeout" stays fast).
const TEST_TIMEOUT: Duration = Duration::from_millis(200);

fn short_timeout_config() -> AdaptiveTimeoutConfig {
    AdaptiveTimeoutConfig {
        initial_timeout: TEST_TIMEOUT,
        minimum_timeout: Duration::from_millis(50),
        maximum_timeout: Duration::from_secs(2),
        timeout_coefficient: 1.0,
        timeout_halflife: Duration::from_secs(60),
    }
}

/// A long-timeout config: a `*Failed` observed well before this fires can only
/// have come from the immediate (unsent ⇒ fail-now) leg, never the timer.
fn long_timeout_config() -> AdaptiveTimeoutConfig {
    AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(60),
        minimum_timeout: Duration::from_secs(1),
        maximum_timeout: Duration::from_secs(120),
        timeout_coefficient: 1.0,
        timeout_halflife: Duration::from_secs(60),
    }
}

/// Poll `pred` every 10ms until it returns true or `deadline` elapses. Returns
/// whether the predicate was satisfied.
async fn wait_until<F: FnMut() -> bool>(deadline: Duration, mut pred: F) -> bool {
    let start = tokio::time::Instant::now();
    loop {
        if pred() {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------------
// Task 1 — the timer backstop, link by link, under RealClock
// ---------------------------------------------------------------------------

/// Narrow link: `ChainRouter::register_request` + `AdaptiveTimeoutManager`
/// under `RealClock` must synthesize the `*Failed` op into the registered
/// chain sink within ~5× the configured timeout. (The unit test
/// `router.rs::timeout_synthesizes_failed` covers only the `MockClock` path —
/// this is the RealClock/`sleep_until` production path the live node runs.)
#[tokio::test]
async fn timer_backstop_delivers_failed_op_under_real_clock() {
    let clock: Arc<dyn Clock> = Arc::new(RealClock);
    let mgr =
        Arc::new(AdaptiveTimeoutManager::new(&short_timeout_config(), clock).expect("manager"));
    let router = ChainRouter::new(mgr);

    let sink = Arc::new(RecordingSink::default());
    router.add_chain(chain_id(), sink.clone());

    // Register an outbound GetAcceptedFrontier to a beacon that never answers.
    router.register_request(node(1), chain_id(), 7, op::GET_ACCEPTED_FRONTIER);

    let delivered = wait_until(TEST_TIMEOUT * 5, || {
        sink.snapshot().iter().any(|(n, o)| {
            *n == node(1) && matches!(o, InboundOp::GetAcceptedFrontierFailed { request_id: 7 })
        })
    })
    .await;

    assert!(
        delivered,
        "timer backstop (RealClock): GetAcceptedFrontierFailed must reach the chain sink \
         within 5x the configured timeout; sink saw {:?}",
        sink.snapshot()
    );
}

/// Full production chain: `AdaptiveTimeoutManager` (RealClock) → `ChainRouter`
/// → `ChainHandler` sink → `BootstrapperEngineAdapter` → real `Bootstrapper`
/// (2 beacons) whose `Sender` is a real `OutboundSender` over the recording
/// mock network. Beacon A replies through the production inbound path
/// (`router.handle_inbound`); beacon B never answers. The synthesized
/// `GetAcceptedFrontierFailed(B)` must complete frontier discovery, observable
/// as the follow-up `GetAccepted` broadcast recorded at the mock network —
/// exactly the transition the live 5-beacon run never made.
#[tokio::test]
async fn frontier_discovery_completes_when_one_beacon_never_answers() {
    let token = CancellationToken::new();

    let clock: Arc<dyn Clock> = Arc::new(RealClock);
    let mgr =
        Arc::new(AdaptiveTimeoutManager::new(&short_timeout_config(), clock).expect("manager"));
    let router = ChainRouter::new(mgr);

    // Recording network that delivers to everyone (B receives the request but
    // stays silent — the pure timer-backstop case).
    let net = Arc::new(MockNetwork::default());
    let sender = Arc::new(OutboundSender::new(
        net.clone(),
        Arc::new(AllowAll),
        Arc::clone(&router) as Arc<dyn Router>,
        chain_id(),
        subnet_id(),
        router.current_timeout(),
    ));

    // Real bootstrapper over the in-memory test VM, 2 beacons.
    let vm: TestVm = init_test_vm(&token).await.expect("init vm");
    let tip = vm.last_accepted(&token).await.expect("genesis");
    let vm = Arc::new(AsyncMutex::new(vm));

    let beacon_a = node(10);
    let beacon_b = node(11);
    let mut beacons = BTreeMap::new();
    beacons.insert(beacon_a, 1u64);
    beacons.insert(beacon_b, 1u64);

    let ctx = Arc::new(ConsensusContext::new(
        test_chain_context(),
        "C".to_string(),
        Arc::new(NoOpAcceptor),
        Arc::new(NoOpAcceptor),
    ));
    let boot =
        ava_engine::snowman::bootstrap::Bootstrapper::new(ava_engine::snowman::bootstrap::Config {
            subnet_id: subnet_id(),
            ctx: ctx.clone(),
            vm: Arc::clone(&vm),
            sender: Arc::clone(&sender),
            beacons,
            token: token.clone(),
        });
    let getter = Arc::new(ava_engine::snowman::Getter::new(
        Arc::clone(&vm),
        Arc::clone(&sender),
        token.clone(),
    ));
    let (transition_tx, transition_rx) = transition_channel(8);
    let adapter = BootstrapperEngineAdapter::new(boot, transition_tx, 0, getter);

    let mut engines = EngineManager::new(EngineType::Snowman);
    engines.register(EngineState::Bootstrapping, Box::new(adapter));

    let halt = CancellationToken::new();
    let (handler, sink, _vm_tx) = ChainHandler::new(
        engines,
        EngineState::Bootstrapping,
        16,
        Duration::from_secs(3600),
        halt.clone(),
        transition_rx,
    );
    // The production registration: inbound ops (and timer-synthesized *Failed
    // ops) route to this chain's handler sink.
    router.add_chain(chain_id(), Arc::new(sink));
    let join = handler.start();

    // The handler's start() drives Bootstrapper::start → the frontier broadcast.
    let broadcast_seen = wait_until(Duration::from_secs(2), || {
        net.decoded()
            .iter()
            .any(|m| matches!(m, p2p::message::Message::GetAcceptedFrontier(_)))
    })
    .await;
    assert!(
        broadcast_seen,
        "bootstrapper start(): expected a GetAcceptedFrontier broadcast, got {:?}",
        net.decoded()
    );
    let req = net
        .decoded()
        .iter()
        .find_map(|m| match m {
            p2p::message::Message::GetAcceptedFrontier(g) => Some(g.request_id),
            _ => None,
        })
        .expect("frontier request id");

    // Beacon A replies through the production inbound path; B stays silent.
    router
        .handle_inbound(InboundMessage {
            node: beacon_a,
            chain: chain_id(),
            op: InboundOp::AcceptedFrontier {
                request_id: req,
                container_id: tip,
            },
        })
        .await;

    // Within ~5× the configured timeout the router must synthesize
    // GetAcceptedFrontierFailed(B), completing the responded set (2/2) and
    // beginning frontier agreement: the GetAccepted broadcast reaches the wire.
    let agreed = wait_until(TEST_TIMEOUT * 5, || {
        net.decoded()
            .iter()
            .any(|m| matches!(m, p2p::message::Message::GetAccepted(_)))
    })
    .await;

    assert!(
        agreed,
        "frontier discovery must complete via the timer-synthesized \
         GetAcceptedFrontierFailed(B); network saw {:?}",
        net.decoded()
    );

    halt.cancel();
    token.cancel();
    join.await.expect("handler join");
}
