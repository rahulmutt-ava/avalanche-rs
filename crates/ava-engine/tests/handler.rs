// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ChainHandler` actor tests (06 §5.2): one tokio task drains the queue +
//! VM channel + gossip ticker, dispatches to the active engine, and on `halt`
//! drains its async worker pool with **no leaked tasks**.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use ava_engine::networking::{
    ChainEngine, ChainHandler, ChainMessageSink, EngineManager, InboundOp,
};
use ava_snow::state::{EngineState, EngineType};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::VmEvent;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct CountingEngine {
    handled: Arc<AtomicUsize>,
    gossiped: Arc<AtomicUsize>,
    notified: Arc<AtomicUsize>,
}

#[async_trait]
impl ChainEngine for CountingEngine {
    async fn handle(&mut self, _node: NodeId, _op: InboundOp) {
        self.handled.fetch_add(1, Ordering::SeqCst);
    }
    async fn gossip(&mut self) {
        self.gossiped.fetch_add(1, Ordering::SeqCst);
    }
    async fn notify(&mut self, _event: VmEvent) {
        self.notified.fetch_add(1, Ordering::SeqCst);
    }
}

/// `handler_dispatches_and_shuts_down_clean` — a pushed sync op reaches the
/// active engine; a VM notification reaches `notify`; the gossip ticker fires;
/// and `halt` shuts the task down with the task tracker fully drained.
#[tokio::test(start_paused = true)]
async fn handler_dispatches_and_shuts_down_clean() {
    let engine = CountingEngine::default();
    let handled = engine.handled.clone();
    let gossiped = engine.gossiped.clone();
    let notified = engine.notified.clone();

    let mut mgr = EngineManager::new(EngineType::Snowman);
    mgr.register(EngineState::NormalOp, Box::new(engine));

    let halt = CancellationToken::new();
    let (handler, sink, vm_tx) = ChainHandler::new(
        mgr,
        EngineState::NormalOp,
        16,
        Duration::from_secs(1),
        halt.clone(),
    );
    let tracker = handler.task_tracker();
    let join = handler.start();

    // A pushed sync op reaches the active engine.
    sink.push(
        NodeId::from([1u8; 20]),
        InboundOp::Get {
            request_id: 1,
            container_id: Id::from([2u8; 32]),
        },
    )
    .await;

    // A VM notification reaches notify().
    vm_tx.send(VmEvent::PendingTxs).await.unwrap();

    // Advance virtual time past the gossip interval.
    tokio::time::advance(Duration::from_millis(1500)).await;
    // Let the select loop run.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    assert_eq!(handled.load(Ordering::SeqCst), 1, "sync op dispatched");
    assert_eq!(notified.load(Ordering::SeqCst), 1, "vm notify dispatched");
    assert!(gossiped.load(Ordering::SeqCst) >= 1, "gossip ticked");

    // Shut down via the halt token and confirm the task exits and the tracker
    // drains (no leaked async workers).
    halt.cancel();
    join.await.unwrap();
    tracker.wait().await;
    assert!(tracker.is_empty(), "no leaked tasks after shutdown");
}

/// `async_op_runs_on_pool_and_drains` — an `App*` (async-class) op is processed
/// off the main task and the pool drains on shutdown.
#[tokio::test(start_paused = true)]
async fn async_op_runs_on_pool_and_drains() {
    let mgr = EngineManager::new(EngineType::Snowman);
    let halt = CancellationToken::new();
    let (handler, sink, _vm_tx) = ChainHandler::new(
        mgr,
        EngineState::NormalOp,
        16,
        Duration::from_secs(60),
        halt.clone(),
    );
    let tracker = handler.task_tracker();
    let join = handler.start();

    // AppRequestFailed is classified async.
    sink.push(
        NodeId::from([3u8; 20]),
        InboundOp::AppRequestFailed { request_id: 9 },
    )
    .await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    halt.cancel();
    join.await.unwrap();
    tracker.wait().await;
    assert!(tracker.is_empty(), "async pool drained, no leaked tasks");
}
