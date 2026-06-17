// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Handler-driven engine adapters (M4.30a): a `ChainHandler` starting in
//! `Bootstrapping` drives the `BootstrapperEngineAdapter` through synthetic
//! beacon responses pushed via its `ChainHandlerSink`, the bootstrapper finishes
//! and requests a transition to `NormalOp`, and the handler activates the
//! `SnowmanEngineAdapter` (observed via a side-effect query after a VM notify).

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use ava_engine::networking::{
    BootstrapperEngineAdapter, ChainEngine, ChainHandler, ChainMessageSink, EngineManager,
    InboundOp, SnowmanEngineAdapter, transition_channel,
};
use ava_engine::snowman::bootstrap::{Bootstrapper, Config as BootConfig};
use ava_snow::acceptor::{Acceptor, NoOpAcceptor};
use ava_snow::snowball::{DEFAULT_PARAMETERS, SnowballFactory};
use ava_snow::snowman::Topological;
use ava_snow::state::{EngineState, EngineType};
use ava_snow::{ConsensusContext, Result as SnowResult};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::VmEvent;
use ava_vm::block::ChainVm;
use ava_vm::testutil::{TestVm, init_test_vm, test_chain_context};

use support::{RecordingSender, Sent, block_id, encode_block, validators};

struct CapturingAcceptor;

#[async_trait]
impl Acceptor for CapturingAcceptor {
    async fn accept(&self, _ctx: &ConsensusContext, _id: Id, _bytes: &[u8]) -> SnowResult<()> {
        Ok(())
    }
}

fn consensus_ctx() -> Arc<ConsensusContext> {
    Arc::new(ConsensusContext::new(
        test_chain_context(),
        "C".to_string(),
        Arc::new(CapturingAcceptor),
        Arc::new(NoOpAcceptor),
    ))
}

/// `handler_drives_bootstrap_then_normal_op` — the full handler-driven boot path.
#[tokio::test(start_paused = true)]
async fn handler_drives_bootstrap_then_normal_op() {
    let token = CancellationToken::new();

    // ---- Build a 3-block chain rooted at the VM genesis (heights 1..=3). ----
    let probe_vm: TestVm = init_test_vm(&token).await.expect("probe vm");
    let genesis = probe_vm.last_accepted(&token).await.expect("genesis");
    drop(probe_vm);

    let b1 = encode_block(genesis, 1, b"b1");
    let id1 = block_id(&b1);
    let b2 = encode_block(id1, 2, b"b2");
    let id2 = block_id(&b2);
    let b3 = encode_block(id2, 3, b"b3");
    let id3 = block_id(&b3);
    let chain_bytes = vec![b3, b2, b1];
    let tip = id3;

    // ---- Bootstrapper over its own VM + recording sender + beacon set. ----
    let boot_vm: TestVm = init_test_vm(&token).await.expect("boot vm");
    let boot_sender = RecordingSender::new();
    let ctx = consensus_ctx();

    let beacon_a = NodeId::from([10u8; 20]);
    let beacon_b = NodeId::from([11u8; 20]);
    let mut beacons = BTreeMap::new();
    beacons.insert(beacon_a, 1u64);
    beacons.insert(beacon_b, 1u64);

    let boot = Bootstrapper::new(BootConfig {
        subnet_id: Id::EMPTY,
        ctx: ctx.clone(),
        vm: Arc::new(AsyncMutex::new(boot_vm)),
        sender: boot_sender.clone(),
        beacons,
        token: token.clone(),
    });

    // ---- SnowmanEngine over its own VM + recording sender + validators. ----
    let snow_vm: TestVm = init_test_vm(&token).await.expect("snow vm");
    let snow_sender = RecordingSender::new();
    let (vmgr, _vids) = validators(4);
    // k must not exceed the validator count, mirroring `engine_flows.rs`.
    let mut snow_params = DEFAULT_PARAMETERS;
    snow_params.k = 4;
    snow_params.alpha_preference = 3;
    snow_params.alpha_confidence = 3;
    snow_params.beta = 1;
    snow_params.concurrent_repolls = 1;
    let consensus =
        Topological::new_default(SnowballFactory, snow_params, genesis, 0).expect("topo");
    let snow_engine = ava_engine::snowman::engine::SnowmanEngine::new(
        ava_engine::snowman::engine::Config {
            subnet_id: Id::EMPTY,
            params: snow_params,
            vm: Arc::new(AsyncMutex::new(snow_vm)),
            sender: snow_sender.clone(),
            validators: vmgr,
            token: token.clone(),
        },
        Box::new(consensus),
    );

    // ---- Transition channel + adapters ----
    let (transition_tx, transition_rx) = transition_channel(8);
    let boot_adapter = BootstrapperEngineAdapter::new(boot, transition_tx.clone(), 0);
    let snow_adapter = SnowmanEngineAdapter::new(snow_engine);

    let mut mgr = EngineManager::new(EngineType::Snowman);
    mgr.register(EngineState::Bootstrapping, Box::new(boot_adapter));
    mgr.register(EngineState::NormalOp, Box::new(snow_adapter));

    let halt = CancellationToken::new();
    let (handler, sink, vm_tx) = ChainHandler::new(
        mgr,
        EngineState::Bootstrapping,
        16,
        Duration::from_secs(3600),
        halt.clone(),
        transition_rx,
    );
    let join = handler.start();

    // The handler calls `start()` on the bootstrapper adapter, which begins
    // frontier discovery: GetAcceptedFrontier is sent.
    pump().await;
    assert!(
        boot_sender
            .snapshot()
            .iter()
            .any(|s| matches!(s, Sent::GetAcceptedFrontier { .. })),
        "bootstrap started: expected GetAcceptedFrontier, got {:?}",
        boot_sender.snapshot()
    );

    // Both beacons report the tip as their frontier (request id 1).
    sink.push(
        beacon_a,
        InboundOp::AcceptedFrontier {
            request_id: 1,
            container_id: tip,
        },
    )
    .await;
    sink.push(
        beacon_b,
        InboundOp::AcceptedFrontier {
            request_id: 1,
            container_id: tip,
        },
    )
    .await;
    pump().await;

    // Both beacons accept the tip (request id 2) -> fetch ancestry.
    sink.push(
        beacon_a,
        InboundOp::Accepted {
            request_id: 2,
            container_ids: vec![tip],
        },
    )
    .await;
    sink.push(
        beacon_b,
        InboundOp::Accepted {
            request_id: 2,
            container_ids: vec![tip],
        },
    )
    .await;
    pump().await;

    // Find the GetAncestors request the bootstrapper issued for the tip.
    let (node, req) = boot_sender
        .snapshot()
        .iter()
        .find_map(|s| match s {
            Sent::GetAncestors { node, req, id } if *id == tip => Some((*node, *req)),
            _ => None,
        })
        .expect("GetAncestors for the tip");

    // Serve the full ancestry: the range executes and the bootstrapper finishes,
    // requesting a transition to NormalOp.
    sink.push(
        node,
        InboundOp::Ancestors {
            request_id: req,
            containers: chain_bytes,
        },
    )
    .await;
    pump().await;

    // The bootstrapper handed off (set ctx state) AND requested NormalOp.
    assert_eq!(
        **ctx.state.load(),
        EngineState::NormalOp,
        "bootstrapper handed off"
    );

    // The handler should now dispatch to the SnowmanEngineAdapter. Prove it by a
    // VM notify -> notify_pending_txs -> build+issue+query side effect.
    vm_tx.send(VmEvent::PendingTxs).await.expect("vm notify");
    pump().await;

    assert!(
        snow_sender
            .snapshot()
            .iter()
            .any(|s| matches!(s, Sent::PushQuery { .. } | Sent::PullQuery { .. })),
        "NormalOp engine active: expected a query after PendingTxs, got {:?}",
        snow_sender.snapshot()
    );

    let _ = (id1, id2);
    halt.cancel();
    join.await.expect("handler join");
}

/// A dummy engine that requests a transition to `NormalOp` on its first `handle`,
/// and records when its `start` hook is called.
struct TransitioningEngine {
    tx: tokio::sync::mpsc::Sender<EngineState>,
}

#[async_trait]
impl ChainEngine for TransitioningEngine {
    async fn handle(&mut self, _node: NodeId, _op: InboundOp) {
        let _ = self.tx.send(EngineState::NormalOp).await;
    }
}

struct TargetEngine {
    started: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl ChainEngine for TargetEngine {
    async fn handle(&mut self, _node: NodeId, _op: InboundOp) {}
    async fn start(&mut self) {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `transition_switches_active_engine_and_calls_start` — a transition request
/// switches the active engine and the handler calls `start()` on the newly
/// active engine.
#[tokio::test(start_paused = true)]
async fn transition_switches_active_engine_and_calls_start() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (transition_tx, transition_rx) = transition_channel(4);
    let started = Arc::new(AtomicUsize::new(0));

    let src = TransitioningEngine {
        tx: transition_tx.clone(),
    };
    let dst = TargetEngine {
        started: started.clone(),
    };

    let mut mgr = EngineManager::new(EngineType::Snowman);
    mgr.register(EngineState::Bootstrapping, Box::new(src));
    mgr.register(EngineState::NormalOp, Box::new(dst));

    let halt = CancellationToken::new();
    let (handler, sink, _vm_tx) = ChainHandler::new(
        mgr,
        EngineState::Bootstrapping,
        16,
        Duration::from_secs(3600),
        halt.clone(),
        transition_rx,
    );
    let join = handler.start();

    sink.push(
        NodeId::from([1u8; 20]),
        InboundOp::GetFailed { request_id: 1 },
    )
    .await;
    pump().await;

    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "the newly-active engine's start() must be called exactly once"
    );

    halt.cancel();
    join.await.expect("join");
}

/// Yield enough times for the single-task select loop to drain queued work.
async fn pump() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}
