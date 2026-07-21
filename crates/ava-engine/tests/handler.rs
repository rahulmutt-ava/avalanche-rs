// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ChainHandler` actor tests (06 §5.2): one tokio task drains the queue +
//! VM channel + gossip ticker, dispatches to the active engine, and on `halt`
//! drains its async worker pool with **no leaked tasks**.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;

use ava_engine::networking::{
    ChainEngine, ChainHandler, ChainMessageSink, EngineManager, InboundOp, SnowmanEngineAdapter,
    transition_channel,
};
use ava_engine::snowman::Getter;
use ava_engine::snowman::engine::{Config as SnowmanConfig, SnowmanEngine};
use ava_snow::snowball::{DEFAULT_PARAMETERS, SnowballFactory};
use ava_snow::snowman::Topological;
use ava_snow::state::{EngineState, EngineType};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::VmEvent;
use ava_vm::block::ChainVm;
use ava_vm::testutil::{AppCall, TestVm, init_test_vm};
use tokio_util::sync::CancellationToken;

use support::{RecordingSender, validators};

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
    let (_transition_tx, transition_rx) = transition_channel(4);
    let (handler, sink, vm_tx) = ChainHandler::new(
        mgr,
        EngineState::NormalOp,
        16,
        Duration::from_secs(1),
        halt.clone(),
        transition_rx,
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
    let (_transition_tx, transition_rx) = transition_channel(4);
    let (handler, sink, _vm_tx) = ChainHandler::new(
        mgr,
        EngineState::NormalOp,
        16,
        Duration::from_secs(60),
        halt.clone(),
        transition_rx,
    );
    let tracker = handler.task_tracker();
    let join = handler.start();

    // AppRequestFailed is classified async.
    sink.push(
        NodeId::from([3u8; 20]),
        InboundOp::AppRequestFailed {
            request_id: 9,
            code: 0,
            message: String::new(),
        },
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

/// `sink_push_app_gossip_reaches_vm_via_dispatch` — the FULL production
/// pipeline, not a direct `adapter.handle()` call: `sink.push()` (the real
/// `ChainMessageSink` entry point the router uses) -> `HandlerMessage::classify`
/// (Async) -> the handler's bounded queue -> `ChainHandler::dispatch`'s `Async`
/// arm -> a REGISTERED `SnowmanEngineAdapter`'s `handle()` -> the VM's
/// `AppHandler::app_gossip`. Regression-pins the review fix to `dispatch`'s
/// `Async` arm, which used to be `let _ = (node, op);` — a silent drop that
/// meant every `App*` op was lost on this path even though the adapters'
/// own App arms (exercised directly in `engine_adapter.rs`) were correct.
#[tokio::test(start_paused = true)]
async fn sink_push_app_gossip_reaches_vm_via_dispatch() {
    let token = CancellationToken::new();
    let vm: TestVm = init_test_vm(&token).await.expect("init vm");
    let observer = vm.observer();
    let genesis = vm.last_accepted(&token).await.expect("genesis");

    let vm = Arc::new(AsyncMutex::new(vm));
    let sender = RecordingSender::new();

    let getter = Arc::new(Getter::new(
        Arc::clone(&vm),
        Arc::clone(&sender),
        token.clone(),
    ));

    let (vmgr, _) = validators(1);
    let mut params = DEFAULT_PARAMETERS;
    params.k = 1;
    params.alpha_preference = 1;
    params.alpha_confidence = 1;
    params.beta = 1;
    params.concurrent_repolls = 1;
    let consensus = Topological::new_default(SnowballFactory, params, genesis, 0).expect("topo");
    let snow_engine = SnowmanEngine::new(
        SnowmanConfig {
            subnet_id: Id::EMPTY,
            params,
            vm: Arc::clone(&vm),
            sender: Arc::clone(&sender),
            validators: vmgr,
            token: token.clone(),
        },
        Box::new(consensus),
    );
    let adapter = SnowmanEngineAdapter::new(snow_engine, getter, Arc::clone(&vm), token.clone());

    let mut mgr = EngineManager::new(EngineType::Snowman);
    mgr.register(EngineState::NormalOp, Box::new(adapter));

    let halt = CancellationToken::new();
    let (_transition_tx, transition_rx) = transition_channel(4);
    let (handler, sink, _vm_tx) = ChainHandler::new(
        mgr,
        EngineState::NormalOp,
        16,
        Duration::from_secs(60),
        halt.clone(),
        transition_rx,
    );
    let join = handler.start();

    let node = NodeId::from([44u8; 20]);
    sink.push(
        node,
        InboundOp::AppGossip {
            bytes: vec![1, 2, 3],
        },
    )
    .await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    halt.cancel();
    join.await.unwrap();

    assert!(
        matches!(
            observer.app_calls().last(),
            Some(AppCall::Gossip { node: n, bytes }) if *n == node && bytes == &vec![1u8, 2, 3]
        ),
        "AppGossip pushed via sink.push() must reach the VM through the real \
         decode->router->sink->dispatch path, got {:?}",
        observer.app_calls()
    );
}
