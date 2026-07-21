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

/// A handler that records the ops it received but rejects a configured node
/// via `should_handle` (proves `connected`/`disconnected` respect the same ACL
/// gate `handle_inbound` does).
struct RejectingHandler {
    pushed: Mutex<Vec<InboundOp>>,
    rejected: NodeId,
}

#[async_trait]
impl ava_engine::networking::ChainMessageSink for RejectingHandler {
    async fn push(&self, _node: NodeId, op: InboundOp) {
        self.pushed.lock().unwrap().push(op);
    }

    fn should_handle(&self, node: NodeId) -> bool {
        node != self.rejected
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

/// `connected_broadcasts_to_every_chain` — `ChainRouter::connected` delivers
/// `InboundOp::Connected` to every registered chain sink (Go `chain_router.go`
/// `Connected` broadcasting to `cr.chainHandlers`), and `disconnected` likewise
/// with `InboundOp::Disconnected`. A handler whose `should_handle` rejects the
/// node must not receive either (mirrors `handle_inbound`'s ACL gating).
#[tokio::test(start_paused = true)]
async fn connected_broadcasts_to_every_chain() {
    let mgr = timeout_mgr();
    let router = ChainRouter::new(mgr);

    let chain_b = Id::from([0xCCu8; 32]);
    let handler_a = Arc::new(RecordingHandler::default());
    let handler_b = Arc::new(RecordingHandler::default());
    router.add_chain(chain_a(), handler_a.clone());
    router.add_chain(chain_b, handler_b.clone());

    let node = NodeId::from([7u8; 20]);
    let version = ava_version::application::Application::new("avalanchego".to_string(), 1, 2, 3);

    // `connected` is awaited to completion inline (no spawn): by the time this
    // `.await` returns, every chain's push has already landed — no yield_now
    // needed (review follow-up, Task 8; see `connected_orders_before_later_inbound_ops`
    // for a test that actually depends on this completion-before-return
    // guarantee).
    router.connected(node, version.clone()).await;

    assert_eq!(
        handler_a.pushed.lock().unwrap().as_slice(),
        &[InboundOp::Connected {
            version: version.clone()
        }],
        "chain A must receive Connected"
    );
    assert_eq!(
        handler_b.pushed.lock().unwrap().as_slice(),
        &[InboundOp::Connected {
            version: version.clone()
        }],
        "chain B must receive Connected"
    );

    router.disconnected(node).await;

    assert_eq!(
        handler_a.pushed.lock().unwrap().as_slice(),
        &[
            InboundOp::Connected {
                version: version.clone()
            },
            InboundOp::Disconnected
        ],
        "chain A must receive Disconnected"
    );
    assert_eq!(
        handler_b.pushed.lock().unwrap().as_slice(),
        &[InboundOp::Connected { version }, InboundOp::Disconnected],
        "chain B must receive Disconnected"
    );
}

/// `connected_respects_should_handle` — a chain whose `should_handle` rejects
/// the connecting node receives neither `Connected` nor `Disconnected`.
#[tokio::test(start_paused = true)]
async fn connected_respects_should_handle() {
    let mgr = timeout_mgr();
    let router = ChainRouter::new(mgr);

    let node = NodeId::from([8u8; 20]);
    let rejecting = Arc::new(RejectingHandler {
        pushed: Mutex::new(Vec::new()),
        rejected: node,
    });
    router.add_chain(chain_a(), rejecting.clone());

    let version = ava_version::application::Application::new("avalanchego".to_string(), 1, 0, 0);
    router.connected(node, version).await;
    router.disconnected(node).await;

    assert!(
        rejecting.pushed.lock().unwrap().is_empty(),
        "a rejected node's connected/disconnected must not reach the chain"
    );
}

/// `connected_orders_before_later_inbound_ops` — review follow-up (Task 8
/// CRITICAL): `router.connected(node, v).await` MUST fully deliver the
/// `Connected` push before returning, so a caller that immediately follows it
/// with `router.handle_inbound(...)` for the same node (no yield in between —
/// exactly the shape of `ava-network`'s `Peer::finish_handshake` awaiting
/// `router.connected(...)` and then the read loop moving straight on to the
/// next inbound frame) can never have that later op observed by the chain
/// before `Connected` is.
///
/// This pins the fix, and would FAIL under the earlier `tokio::spawn`-per-push
/// design: `router.connected(...)` there returned as soon as the spawn calls
/// were issued, without waiting for the executor to ever poll them. Since
/// `RecordingHandler::push` has no internal `.await` point, a directly-awaited
/// push (from `handle_inbound`, called immediately after with no intervening
/// yield) runs to completion in a single poll and can never be preempted by a
/// spawned task that hasn't been scheduled yet — so the old code would record
/// `[Get, Connected]` here instead of `[Connected, Get]`. On the current-thread
/// flavor `#[tokio::test]` uses by default, this is not just "can" but
/// deterministically WOULD happen, since a freshly spawned task never runs
/// before the spawning task's current poll returns control to the executor.
#[tokio::test]
async fn connected_orders_before_later_inbound_ops() {
    let mgr = timeout_mgr();
    let router = ChainRouter::new(mgr);

    let handler = Arc::new(RecordingHandler::default());
    router.add_chain(chain_a(), handler.clone());

    let node = NodeId::from([9u8; 20]);
    let version = ava_version::application::Application::new("avalanchego".to_string(), 1, 2, 3);

    // No yield between these two calls — the ordering guarantee must come
    // from `connected` itself completing its pushes before returning, not
    // from any accidental scheduling gap the test inserts.
    router.connected(node, version.clone()).await;
    router
        .handle_inbound(InboundMessage {
            node,
            chain: chain_a(),
            op: InboundOp::Get {
                request_id: 1,
                container_id: Id::from([1u8; 32]),
            },
        })
        .await;

    let pushed = handler.pushed.lock().unwrap();
    assert_eq!(
        pushed.as_slice(),
        &[
            InboundOp::Connected { version },
            InboundOp::Get {
                request_id: 1,
                container_id: Id::from([1u8; 32]),
            }
        ],
        "Connected must be observed by the chain strictly before a later \
         inbound op from the same node, got {pushed:?}"
    );
}
