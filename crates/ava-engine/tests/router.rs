// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ChainRouter` routing + timeout->`*Failed` synthesis tests (06 §5.1).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use ava_engine::networking::{
    AdaptiveTimeoutConfig, AdaptiveTimeoutManager, ChainRouter, InboundMessage, InboundOp, Router,
};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::MockClock;

/// A handler that records the ops it received.
#[derive(Default)]
struct RecordingHandler {
    pushed: Mutex<Vec<InboundOp>>,
    count: AtomicUsize,
}

#[async_trait]
impl ava_engine::networking::ChainMessageSink for RecordingHandler {
    async fn push(&self, _node: NodeId, op: InboundOp) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.pushed.lock().unwrap().push(op);
    }
}

fn chain_a() -> Id {
    Id::from([0xAA; 32])
}

fn unknown_chain() -> Id {
    Id::from([0xBB; 32])
}

fn timeout_mgr() -> Arc<AdaptiveTimeoutManager> {
    let clock = MockClock::at(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
    let cfg = AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(2),
        minimum_timeout: Duration::from_millis(500),
        maximum_timeout: Duration::from_secs(10),
        timeout_coefficient: 2.0,
        timeout_halflife: Duration::from_secs(60),
    };
    let clock_arc: Arc<dyn ava_utils::clock::Clock> = Arc::new(clock);
    Arc::new(AdaptiveTimeoutManager::new(&cfg, clock_arc).unwrap())
}

/// `router_routes_to_chain_handler` — an inbound message for a registered chain
/// reaches that chain's handler; a message for an unknown chain is dropped.
#[tokio::test(start_paused = true)]
async fn router_routes_to_chain_handler() {
    let mgr = timeout_mgr();
    let router = ChainRouter::new(mgr);

    let handler = Arc::new(RecordingHandler::default());
    router.add_chain(chain_a(), handler.clone());

    let node = NodeId::from([1u8; 20]);

    // A known-chain message is delivered.
    router
        .handle_inbound(InboundMessage {
            node,
            chain: chain_a(),
            op: InboundOp::Get {
                request_id: 7,
                container_id: Id::from([9u8; 32]),
            },
        })
        .await;
    tokio::task::yield_now().await;
    assert_eq!(handler.count.load(Ordering::SeqCst), 1);

    // An unknown-chain message is silently dropped.
    router
        .handle_inbound(InboundMessage {
            node,
            chain: unknown_chain(),
            op: InboundOp::Get {
                request_id: 8,
                container_id: Id::from([9u8; 32]),
            },
        })
        .await;
    tokio::task::yield_now().await;
    assert_eq!(
        handler.count.load(Ordering::SeqCst),
        1,
        "unknown chain dropped"
    );
}

/// `timeout_synthesizes_failed` — a registered outbound request that times out
/// synthesizes the matching `*Failed` op into the chain handler.
#[tokio::test(start_paused = true)]
async fn timeout_synthesizes_failed() {
    let clock = MockClock::at(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
    let cfg = AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(2),
        minimum_timeout: Duration::from_millis(500),
        maximum_timeout: Duration::from_secs(10),
        timeout_coefficient: 2.0,
        timeout_halflife: Duration::from_secs(60),
    };
    let clock_arc: Arc<dyn ava_utils::clock::Clock> = Arc::new(clock.clone());
    let mgr = Arc::new(AdaptiveTimeoutManager::new(&cfg, clock_arc).unwrap());
    let router = ChainRouter::new(mgr);

    let handler = Arc::new(RecordingHandler::default());
    router.add_chain(chain_a(), handler.clone());

    let node = NodeId::from([2u8; 20]);

    // Register an outbound Get; it expects a Put response with request_id 42.
    router.register_request(node, chain_a(), 42, InboundOp::failed_kind_for_get());

    // No response arrives; advance past the deadline.
    clock.advance(Duration::from_secs(3));
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    let pushed = handler.pushed.lock().unwrap();
    assert_eq!(pushed.len(), 1, "exactly one *Failed synthesized");
    assert!(
        matches!(pushed[0], InboundOp::GetFailed { request_id: 42 }),
        "expected GetFailed, got {:?}",
        pushed[0]
    );
}

/// Exactly-once `*Failed` regression (M9.15 review finding #1): if the
/// background dispatch loop's timer fires and claims a request's pending
/// entry *before* `fail_request` runs for the same id (the sender's "unsent
/// ⇒ fail now" leg racing the timeout backstop between `register_request` and
/// `fail_request`), `fail_request` must be a no-op — it must NOT synthesize a
/// second `*Failed` for a request the timer already delivered one for.
/// `QueryFailed`/`GetFailed` are not idempotent on the engine side (a
/// duplicate re-enters the `chits()` self-vote path, `engine.rs:545-560`), so
/// double delivery is a correctness bug, not just noise.
#[tokio::test(start_paused = true)]
async fn fail_request_after_timer_already_fired_does_not_double_deliver() {
    let clock = MockClock::at(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
    let cfg = AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(2),
        minimum_timeout: Duration::from_millis(500),
        maximum_timeout: Duration::from_secs(10),
        timeout_coefficient: 2.0,
        timeout_halflife: Duration::from_secs(60),
    };
    let clock_arc: Arc<dyn ava_utils::clock::Clock> = Arc::new(clock.clone());
    let mgr = Arc::new(AdaptiveTimeoutManager::new(&cfg, clock_arc).unwrap());
    let router = ChainRouter::new(mgr);

    let handler = Arc::new(RecordingHandler::default());
    router.add_chain(chain_a(), handler.clone());

    let node = NodeId::from([3u8; 20]);
    let op_tag = InboundOp::failed_kind_for_get();

    router.register_request(node, chain_a(), 55, op_tag);

    // Pre-claim the entry: let the background dispatch loop's timer fire
    // FIRST (simulating the timer winning the register/fail_request race),
    // removing the pending entry and delivering the one legitimate GetFailed.
    clock.advance(Duration::from_secs(3));
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert_eq!(
        handler.pushed.lock().unwrap().len(),
        1,
        "timer must have already delivered the one legitimate GetFailed"
    );

    // The sender's "unsent" leg now calls fail_request for the SAME id — this
    // must be a no-op: the timer already claimed (and delivered) this
    // request's terminal *Failed.
    router.fail_request(node, chain_a(), 55, op_tag);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert_eq!(
        handler.pushed.lock().unwrap().len(),
        1,
        "fail_request must NOT synthesize a second *Failed once the timer has \
         already claimed this request (exactly-once property)"
    );
}
