// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Task-6 gossip-loop tests: `PushGossiper`/`PullGossiper`/`GossipHandler`
//! (Go `network/p2p/gossip/{gossip.go,handler.go}` parity).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use prost::Message;
use tokio_util::sync::CancellationToken;

use ava_p2p::error::Result as P2pResult;
use ava_p2p::gossip::bloom::BloomSet;
use ava_p2p::gossip::handler::GossipHandler;
use ava_p2p::gossip::pull::PullGossiper;
use ava_p2p::gossip::push::PushGossiper;
use ava_p2p::gossip::{GossipParams, Gossipable, Marshaller, Set, every};
use ava_p2p::handler::Handler as _;
use ava_p2p::network::{P2pNetwork, protocol_prefix};
use ava_p2p::pb::sdk;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;
use ava_vm::app::AppError;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::error::Result as VmResult;

// These crates are only reachable transitively through `ava-p2p`'s own
// dependency graph in this test binary (not used directly here); silence
// `unused_crate_dependencies` rather than dropping them from `[dependencies]`,
// where the library crate genuinely needs them.
use ava_utils as _;
use parking_lot as _;
use thiserror as _;
use tracing as _;

const HANDLER_ID: u64 = 0;

fn test_node(byte: u8) -> NodeId {
    NodeId::from([byte; 20])
}

/// Builds a distinct [`Id`] per `index` (big-endian in the first 4 bytes,
/// zero-padded), matching `gossip/bloom.rs`'s test helper of the same name.
fn indexed_id(index: u32) -> Id {
    let mut bytes = [0u8; 32];
    if let Some(slot) = bytes.get_mut(0..4) {
        slot.copy_from_slice(&index.to_be_bytes());
    }
    Id::from(bytes)
}

/// A minimal `Gossipable` wrapping an [`Id`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TestItem(Id);

impl Gossipable for TestItem {
    fn gossip_id(&self) -> Id {
        self.0
    }
}

/// A stateless `Marshaller<TestItem>` that round-trips the raw 32 id bytes;
/// any other length is treated as a corrupt/malformed item.
#[derive(Debug, Default, Clone, Copy)]
struct TestMarshaller;

impl Marshaller<TestItem> for TestMarshaller {
    fn marshal(&self, t: &TestItem) -> P2pResult<Vec<u8>> {
        Ok(t.0.as_bytes().to_vec())
    }

    fn unmarshal(&self, bytes: &[u8]) -> P2pResult<TestItem> {
        Id::from_slice(bytes)
            .map(TestItem)
            .map_err(|e| ava_p2p::Error::Decode(e.to_string()))
    }
}

/// An in-memory `HashSet`-backed `Set<TestItem>` used by every test in this
/// file. `get_filter()` returns a fixed, settable `(filter, salt)` pair
/// rather than a real bloom filter — good enough for the tests that only
/// check the pull request carries `set.get_filter()`'s bytes verbatim (test
/// 2); tests that need a *functional* filter (test 4) build a real
/// [`BloomSet`] directly instead.
#[derive(Default)]
struct TestSet {
    items: StdMutex<HashMap<Id, TestItem>>,
    filter: StdMutex<(Vec<u8>, Vec<u8>)>,
}

impl TestSet {
    fn new() -> Self {
        Self::default()
    }

    fn with_filter(filter: Vec<u8>, salt: Vec<u8>) -> Self {
        Self {
            items: StdMutex::new(HashMap::new()),
            filter: StdMutex::new((filter, salt)),
        }
    }

    fn contains(&self, id: &Id) -> bool {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    fn len(&self) -> usize {
        self.items.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Removes `id`, simulating the item being mined/evicted out from under
    /// a `PushGossiper` that still has it queued.
    fn remove(&self, id: &Id) {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
}

impl Set<TestItem> for TestSet {
    fn add(&self, t: TestItem) -> P2pResult<()> {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(t.0, t);
        Ok(())
    }

    fn has(&self, id: &Id) -> bool {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    fn iterate(&self, f: &mut dyn FnMut(&TestItem) -> bool) {
        for item in self
            .items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            if !f(item) {
                break;
            }
        }
    }

    fn get_filter(&self) -> (Vec<u8>, Vec<u8>) {
        self.filter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// One recorded `send_app_request` call's arguments.
type RecordedRequest = (HashSet<NodeId>, u32, Vec<u8>);
/// One recorded `send_app_gossip` call's arguments.
type RecordedGossip = (SendConfig, Vec<u8>);

/// Records every `send_app_request`/`send_app_gossip` call it receives;
/// every method otherwise succeeds trivially (mirrors `client.rs`'s
/// `RecordingSender`, duplicated here per this crate's established
/// per-module-recorder convention).
#[derive(Default)]
struct RecordingSender {
    requests: StdMutex<Vec<RecordedRequest>>,
    gossips: StdMutex<Vec<RecordedGossip>>,
}

#[async_trait]
impl AppSender for RecordingSender {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        nodes: &HashSet<NodeId>,
        request_id: u32,
        bytes: Vec<u8>,
    ) -> VmResult<()> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((nodes.clone(), request_id, bytes));
        Ok(())
    }

    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn send_app_error(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _code: i32,
        _message: &str,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn send_app_gossip(
        &self,
        _token: &CancellationToken,
        config: SendConfig,
        bytes: Vec<u8>,
    ) -> VmResult<()> {
        self.gossips
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((config, bytes));
        Ok(())
    }
}

/// A `Handler` that is never expected to be invoked in these tests — only
/// present so `P2pNetwork::add_handler` has something to register (mirrors
/// `client.rs`'s `NoopHandler`).
struct NoopHandler;

#[async_trait]
impl ava_p2p::handler::Handler for NoopHandler {
    async fn app_gossip(&self, _node: NodeId, _msg: &[u8]) {}

    async fn app_request(
        &self,
        _node: NodeId,
        _deadline: Instant,
        _msg: &[u8],
    ) -> Result<Vec<u8>, AppError> {
        Ok(Vec::new())
    }
}

#[tokio::test(start_paused = true)]
async fn push_cycle_emits_prefixed_push_gossip() {
    let sender = Arc::new(RecordingSender::default());
    let network = P2pNetwork::new(test_node(0), sender.clone());
    let client = network
        .add_handler(HANDLER_ID, Arc::new(NoopHandler))
        .unwrap();

    let id_a = indexed_id(1);
    let id_b = indexed_id(2);
    let set = Arc::new(TestSet::new());
    set.add(TestItem(id_a)).unwrap();
    set.add(TestItem(id_b)).unwrap();

    let push = PushGossiper::new(TestMarshaller, set.clone(), client, GossipParams::default());
    push.add(TestItem(id_a));
    push.add(TestItem(id_b));

    let token = CancellationToken::new();
    push.gossip_cycle(&token).await.unwrap();

    let gossips = sender.gossips.lock().unwrap();
    assert_eq!(
        gossips.len(),
        1,
        "gossip_cycle sends exactly one PushGossip batch for the two queued items"
    );
    let (cfg, bytes) = gossips.first().unwrap();
    assert_eq!(
        cfg.validators,
        GossipParams::default().push_cfg.validators,
        "push cfg validators count"
    );

    let expected_prefix = protocol_prefix(HANDLER_ID);
    assert_eq!(
        expected_prefix,
        vec![0x00],
        "handler id 0 encodes to a single 0x00 varint byte"
    );
    assert!(
        bytes.starts_with(&expected_prefix),
        "gossip bytes carry the handler-id varint prefix"
    );

    let payload = bytes.get(expected_prefix.len()..).unwrap();
    let decoded = sdk::PushGossip::decode(payload).unwrap();
    let expected: Vec<Bytes> = vec![
        Bytes::from(TestMarshaller.marshal(&TestItem(id_a)).unwrap()),
        Bytes::from(TestMarshaller.marshal(&TestItem(id_b)).unwrap()),
    ];
    assert_eq!(
        decoded.gossip, expected,
        "PushGossip carries both marshaled items"
    );
}

#[tokio::test(start_paused = true)]
async fn pull_cycle_requests_with_current_filter() {
    let sender = Arc::new(RecordingSender::default());
    let network = P2pNetwork::new(test_node(0), sender.clone());
    let client = network
        .add_handler(HANDLER_ID, Arc::new(NoopHandler))
        .unwrap();

    let token = CancellationToken::new();
    let peer = test_node(1);
    network
        .handle_connected(&token, peer, Application::new("avalanchers", 1, 0, 0))
        .await
        .unwrap();

    let filter_bytes = vec![9, 9, 9];
    let salt_bytes = vec![1, 2, 3, 4];
    let set = Arc::new(TestSet::with_filter(
        filter_bytes.clone(),
        salt_bytes.clone(),
    ));

    let pull = PullGossiper::new(
        TestMarshaller,
        set,
        client,
        network.clone(),
        GossipParams::default(),
    );
    pull.pull_cycle(&token).await.unwrap();

    let requests = sender.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "pull_cycle issues exactly one request");
    let (nodes, _request_id, bytes) = requests.first().unwrap();
    assert_eq!(
        *nodes,
        HashSet::from([peer]),
        "request goes to the sampled peer"
    );

    let expected_prefix = protocol_prefix(HANDLER_ID);
    let payload = bytes.get(expected_prefix.len()..).unwrap();
    let decoded = sdk::PullGossipRequest::decode(payload).unwrap();
    assert_eq!(
        decoded.filter.as_ref(),
        filter_bytes.as_slice(),
        "request filter matches set.get_filter()"
    );
    assert_eq!(
        decoded.salt.as_ref(),
        salt_bytes.as_slice(),
        "request salt matches set.get_filter()"
    );
}

#[tokio::test(start_paused = true)]
async fn pull_response_admits_items() {
    let sender = Arc::new(RecordingSender::default());
    let network = P2pNetwork::new(test_node(0), sender.clone());
    let client = network
        .add_handler(HANDLER_ID, Arc::new(NoopHandler))
        .unwrap();

    let token = CancellationToken::new();
    let peer = test_node(1);
    network
        .handle_connected(&token, peer, Application::new("avalanchers", 1, 0, 0))
        .await
        .unwrap();

    let set = Arc::new(TestSet::new());
    let pull = PullGossiper::new(
        TestMarshaller,
        set.clone(),
        client,
        network.clone(),
        GossipParams::default(),
    );
    pull.pull_cycle(&token).await.unwrap();

    let request_id = {
        let requests = sender.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        requests.first().unwrap().1
    };

    let id_a = indexed_id(1);
    let id_b = indexed_id(2);
    let response = sdk::PullGossipResponse {
        gossip: vec![
            Bytes::from(TestMarshaller.marshal(&TestItem(id_a)).unwrap()),
            // Corrupt: not a valid 32-byte Id.
            Bytes::from(vec![1, 2, 3]),
            Bytes::from(TestMarshaller.marshal(&TestItem(id_b)).unwrap()),
        ],
    };
    let response_bytes = response.encode_to_vec();

    network
        .handle_app_response(&token, peer, request_id, &response_bytes)
        .await
        .unwrap();

    assert!(set.contains(&id_a), "valid item A was admitted");
    assert!(set.contains(&id_b), "valid item B was admitted");
    assert_eq!(
        set.len(),
        2,
        "the corrupt item is skipped; only the 2 valid items are admitted"
    );
}

#[tokio::test(start_paused = true)]
async fn handler_answers_pull_excluding_known() {
    let id_a = indexed_id(1);
    let id_b = indexed_id(2);
    let set = Arc::new(TestSet::new());
    set.add(TestItem(id_a)).unwrap();
    set.add(TestItem(id_b)).unwrap();

    let handler = GossipHandler::<TestItem, TestMarshaller, TestSet>::new(
        TestMarshaller,
        set,
        None,
        GossipParams::default(),
    );

    // The requester's own filter, containing only item A.
    let mut requester_filter = BloomSet::new(64, 0.01, 0.05).unwrap();
    requester_filter.add(&id_a);
    let (filter_bytes, salt_bytes) = requester_filter.marshal();

    let request = sdk::PullGossipRequest {
        salt: Bytes::from(salt_bytes),
        filter: Bytes::from(filter_bytes),
    };
    let request_bytes = request.encode_to_vec();

    let response_bytes = handler
        .app_request(test_node(1), Instant::now(), &request_bytes)
        .await
        .unwrap();
    let response = sdk::PullGossipResponse::decode(response_bytes.as_slice()).unwrap();

    let expected_b = Bytes::from(TestMarshaller.marshal(&TestItem(id_b)).unwrap());
    assert_eq!(
        response.gossip,
        vec![expected_b],
        "only item B (not known to the requester's filter) is returned"
    );
}

#[tokio::test(start_paused = true)]
async fn handler_admits_pushed_and_forwards() {
    let sender = Arc::new(RecordingSender::default());
    let network = P2pNetwork::new(test_node(0), sender.clone());
    let client = network
        .add_handler(HANDLER_ID, Arc::new(NoopHandler))
        .unwrap();

    let set = Arc::new(TestSet::new());
    let push = Arc::new(PushGossiper::new(
        TestMarshaller,
        set.clone(),
        client,
        GossipParams::default(),
    ));

    let handler = GossipHandler::<TestItem, TestMarshaller, TestSet>::new(
        TestMarshaller,
        set.clone(),
        Some(push.clone()),
        GossipParams::default(),
    );

    let id_c = indexed_id(3);
    let push_msg = sdk::PushGossip {
        gossip: vec![Bytes::from(
            TestMarshaller.marshal(&TestItem(id_c)).unwrap(),
        )],
    };
    let msg_bytes = push_msg.encode_to_vec();

    handler.app_gossip(test_node(1), &msg_bytes).await;

    assert!(
        set.contains(&id_c),
        "app_gossip admits the pushed item into the set"
    );

    let token = CancellationToken::new();
    push.gossip_cycle(&token).await.unwrap();

    let gossips = sender.gossips.lock().unwrap();
    assert_eq!(gossips.len(), 1, "the forwarded item was queued for push");
    let (_cfg, bytes) = gossips.first().unwrap();
    let expected_prefix = protocol_prefix(HANDLER_ID);
    let payload = bytes.get(expected_prefix.len()..).unwrap();
    let decoded = sdk::PushGossip::decode(payload).unwrap();
    assert_eq!(
        decoded.gossip,
        vec![Bytes::from(
            TestMarshaller.marshal(&TestItem(id_c)).unwrap()
        )],
        "the push gossiper's queue picked up the item forwarded by the handler"
    );
}

/// A malformed pull request (not a valid `PullGossipRequest` encoding) maps
/// to `err_unexpected()` (Go `Handler.AppRequest`'s `ParseAppRequest`
/// failure branch, `handler.go:59-63`) rather than panicking or silently
/// answering.
#[tokio::test(start_paused = true)]
async fn handler_rejects_malformed_pull_request() {
    let set = Arc::new(TestSet::new());
    let handler = GossipHandler::<TestItem, TestMarshaller, TestSet>::new(
        TestMarshaller,
        set,
        None,
        GossipParams::default(),
    );

    // A filter that fails `ReadFilter::parse` (too short to contain even the
    // hash-seed count byte + minimum entries).
    let request = sdk::PullGossipRequest {
        salt: Bytes::from(vec![0u8; 32]),
        filter: Bytes::from(vec![0xff]),
    };
    let request_bytes = request.encode_to_vec();

    let err = handler
        .app_request(test_node(1), Instant::now(), &request_bytes)
        .await
        .unwrap_err();
    assert_eq!(err.code, -1, "err_unexpected() code");
    assert_eq!(err.message, "unexpected error");
}

/// Supplementary coverage for [`every`] (not one of the brief's five named
/// tests, but exercises the loop the brief also requires): the cycle runs
/// repeatedly on `period` under a paused clock, and stops once the token is
/// cancelled.
#[tokio::test(start_paused = true)]
async fn every_runs_cycle_on_period_and_stops_on_cancel() {
    let calls = Arc::new(AtomicU32::new(0));
    let token = CancellationToken::new();
    let period = Duration::from_millis(100);

    let calls_clone = calls.clone();
    let token_clone = token.clone();
    let handle = tokio::spawn(async move {
        every(token_clone, period, || {
            let calls = calls_clone.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;
    });

    // Under a paused clock, sleeping cooperatively lets the spawned task's
    // `interval.tick()` fast-forward through as many periods as elapse.
    for _ in 0..3 {
        tokio::time::sleep(period).await;
    }
    token.cancel();
    handle.await.unwrap();

    assert!(
        calls.load(Ordering::SeqCst) >= 3,
        "cycle ran at least 3 times over 3 periods, got {}",
        calls.load(Ordering::SeqCst)
    );
}

/// Review follow-up: an item pushed once is re-sent via `regossip_cfg` once
/// `regossip_period` has elapsed, and NOT before (Go `gossip.go:433-473`'s
/// per-item `lastGossiped` throttle, `gossip.go:506-512`).
#[tokio::test(start_paused = true)]
async fn regossip_fires_after_period_with_regossip_cfg() {
    let sender = Arc::new(RecordingSender::default());
    let network = P2pNetwork::new(test_node(0), sender.clone());
    let client = network
        .add_handler(HANDLER_ID, Arc::new(NoopHandler))
        .unwrap();

    let id_a = indexed_id(1);
    let set = Arc::new(TestSet::new());
    set.add(TestItem(id_a)).unwrap();

    let params = GossipParams::default();
    let push = PushGossiper::new(TestMarshaller, set.clone(), client, params.clone());
    push.add(TestItem(id_a));

    let token = CancellationToken::new();
    push.gossip_cycle(&token).await.unwrap();
    {
        let gossips = sender.gossips.lock().unwrap();
        assert_eq!(gossips.len(), 1, "first cycle sends the initial push batch");
        assert_eq!(
            gossips.first().unwrap().0.validators,
            params.push_cfg.validators,
            "first send uses push_cfg"
        );
    }

    // Immediately again: not yet due, must not resend.
    push.gossip_cycle(&token).await.unwrap();
    assert_eq!(
        sender.gossips.lock().unwrap().len(),
        1,
        "regossip must not fire before regossip_period has elapsed"
    );

    tokio::time::advance(params.regossip_period + Duration::from_millis(1)).await;
    push.gossip_cycle(&token).await.unwrap();

    let gossips = sender.gossips.lock().unwrap();
    assert_eq!(
        gossips.len(),
        2,
        "regossip fires once regossip_period has elapsed"
    );
    let (cfg, bytes) = gossips.get(1).unwrap();
    assert_eq!(
        cfg.validators, params.regossip_cfg.validators,
        "regossip cfg validators count"
    );

    let expected_prefix = protocol_prefix(HANDLER_ID);
    let payload = bytes.get(expected_prefix.len()..).unwrap();
    let decoded = sdk::PushGossip::decode(payload).unwrap();
    assert_eq!(
        decoded.gossip,
        vec![Bytes::from(
            TestMarshaller.marshal(&TestItem(id_a)).unwrap()
        )],
        "the same item is re-sent on regossip"
    );
}

/// Review follow-up: pins the per-item regossip mechanism. A regossip
/// backlog bigger than one `target_message_size` batch must drain across
/// successive cycles, not wait a full extra `regossip_period` per batch.
///
/// This test MUST FAIL under the earlier cycle-level `last_regossip` gate:
/// that version sets one `last_regossip` timestamp for the whole
/// `PushGossiper` the instant *any* regossip send happens, so the very next
/// cycle (only `push_period` later) finds regossip "not due" again and sends
/// nothing — the second cycle's assertion below (`gossips.len() == 4`) would
/// see `3` instead.
#[tokio::test(start_paused = true)]
async fn regossip_backlog_drains_across_cycles() {
    let sender = Arc::new(RecordingSender::default());
    let network = P2pNetwork::new(test_node(0), sender.clone());
    let client = network
        .add_handler(HANDLER_ID, Arc::new(NoopHandler))
        .unwrap();

    let id_a = indexed_id(1);
    let id_b = indexed_id(2);
    let set = Arc::new(TestSet::new());
    set.add(TestItem(id_a)).unwrap();
    set.add(TestItem(id_b)).unwrap();

    // One marshaled `TestItem` is 32 bytes; a target smaller than that forces
    // exactly one item per drain call (the loop always admits at least one
    // item before re-checking the size bound against `target_message_size`).
    let params = GossipParams {
        target_message_size: 10,
        regossip_period: Duration::from_millis(200),
        ..GossipParams::default()
    };

    let push = PushGossiper::new(TestMarshaller, set.clone(), client, params.clone());
    push.add(TestItem(id_a));
    push.add(TestItem(id_b));

    let token = CancellationToken::new();
    // Two push cycles to move both items onto the regossip queue (each
    // capped at one item by the small target size); neither is due for
    // regossip yet (no time has advanced), so only the two push_cfg sends
    // happen.
    push.gossip_cycle(&token).await.unwrap();
    push.gossip_cycle(&token).await.unwrap();
    assert_eq!(
        sender.gossips.lock().unwrap().len(),
        2,
        "both items pushed individually (capped by the small target size)"
    );

    tokio::time::advance(params.regossip_period + Duration::from_millis(1)).await;

    // Cycle 1: both items are due, but the small target size caps the
    // regossip drain at one item.
    push.gossip_cycle(&token).await.unwrap();
    assert_eq!(
        sender.gossips.lock().unwrap().len(),
        3,
        "cycle 1's regossip drain sends exactly one of the two due items"
    );

    // ~push_period later — regossip_period has NOT elapsed again since
    // cycle 1's regossip send. Under the per-item mechanism the *other*
    // item's timestamp was never refreshed (it wasn't included in cycle
    // 1's batch), so it is still overdue and drains now.
    tokio::time::advance(params.push_period).await;
    push.gossip_cycle(&token).await.unwrap();
    let gossips = sender.gossips.lock().unwrap();
    assert_eq!(
        gossips.len(),
        4,
        "cycle 2 drains the remainder of the regossip backlog"
    );

    let expected_prefix = protocol_prefix(HANDLER_ID);
    let mut regossiped_ids = HashSet::new();
    for (cfg, bytes) in gossips.iter().skip(2) {
        assert_eq!(
            cfg.validators, params.regossip_cfg.validators,
            "regossip cfg validators count"
        );
        let payload = bytes.get(expected_prefix.len()..).unwrap();
        let decoded = sdk::PushGossip::decode(payload).unwrap();
        assert_eq!(
            decoded.gossip.len(),
            1,
            "each regossip cycle sends exactly one item (small target size)"
        );
        let marshaled = decoded.gossip.first().unwrap().clone();
        let item = TestMarshaller.unmarshal(&marshaled).unwrap();
        regossiped_ids.insert(item.0);
    }
    assert_eq!(
        regossiped_ids,
        HashSet::from([id_a, id_b]),
        "both backlogged items were eventually regossiped, one per cycle"
    );
}

/// Review follow-up: an item the set no longer `has()` by the time its
/// regossip comes due is dropped rather than re-sent, and is recorded in the
/// discarded cache — re-adding it later pretends it was just gossiped
/// (Go `gossip.go:499-503,586-591`).
#[tokio::test(start_paused = true)]
async fn dropped_from_set_items_are_discarded_not_regossiped() {
    let sender = Arc::new(RecordingSender::default());
    let network = P2pNetwork::new(test_node(0), sender.clone());
    let client = network
        .add_handler(HANDLER_ID, Arc::new(NoopHandler))
        .unwrap();

    let id_a = indexed_id(1);
    let set = Arc::new(TestSet::new());
    set.add(TestItem(id_a)).unwrap();

    let params = GossipParams::default();
    let push = PushGossiper::new(TestMarshaller, set.clone(), client, params.clone());
    push.add(TestItem(id_a));

    let token = CancellationToken::new();
    // First cycle pushes the item (the set still has it) and queues it for
    // regossip.
    push.gossip_cycle(&token).await.unwrap();
    assert_eq!(sender.gossips.lock().unwrap().len(), 1);

    // The set no longer has the item (mined/evicted).
    set.remove(&id_a);

    tokio::time::advance(params.regossip_period + Duration::from_millis(1)).await;
    push.gossip_cycle(&token).await.unwrap();

    assert_eq!(
        sender.gossips.lock().unwrap().len(),
        1,
        "an item the set no longer has is dropped, not regossiped"
    );

    // Re-adding the id now finds it in the discarded cache and enqueues it
    // straight onto the regossip queue with a fresh timestamp, pretending it
    // was just gossiped (Go gossip.go:586-591) — so a gossip_cycle
    // immediately afterward must NOT resend it yet. (If the discarded-cache
    // routing were broken and `add` fell through to `to_gossip` instead,
    // this next cycle's push drain would send it immediately and the
    // assertion below would see `2`.)
    set.add(TestItem(id_a)).unwrap();
    push.add(TestItem(id_a));
    push.gossip_cycle(&token).await.unwrap();
    assert_eq!(
        sender.gossips.lock().unwrap().len(),
        1,
        "re-added-from-discarded item is pretended freshly gossiped, not sent immediately"
    );
}
