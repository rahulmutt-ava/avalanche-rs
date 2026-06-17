// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M3.27 `pipeline_wrapping_order` (specs 07 §8.2, 00 §11.1.2): the
//! `create_snowman_chain` pipeline builds the VM stack in the exact ratified
//! order, the DB stack with the documented prefixes, initializes the VM, and
//! registers the handler with the router + timeout manager.

// Test-fixture index/expect on known-small data is clearer than checked variants.
#![allow(clippy::indexing_slicing, clippy::expect_used)]

// Crate deps linked by the lib/support but not named directly by this target.
use assert_matches as _;
use ava_codec as _;
use ava_crypto as _;
use ava_version as _;
use proptest as _;
use rustls_pemfile as _;
use serde_json as _;
use sha2 as _;
use thiserror as _;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_chains::create_chain::{
    BOOTSTRAPPING_DB_PREFIX, ChangeNotifier, DbStack, VM_DB_PREFIX, WrappedVm, build_db_stack,
    create_snowman_chain, wrap_snowman_vm,
};
use ava_database::{DynDatabase, MemDb};
use ava_engine::networking::router::{ChainMessageSink, Router};
use ava_proposervm::ProposerVm;
use ava_snow::snowball::DEFAULT_PARAMETERS;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, MockClock};
use ava_validators::validator::GetValidatorOutput;
use ava_validators::{DefaultManager, ValidatorManager};
use ava_vm::middleware::{MeterVm, TracedVm};
use ava_vm::testutil::{NoopAppSender, TestVm, test_chain_context};
use ava_vm::{AppSender, ChainVm, Vm};
use prometheus::Registry;

mod support;
use support::{FixedState, RecordingSender, Sent, staking_identity};

/// A router that records which chains were registered (the manager registers
/// each chain's handler sink with the router + timeout manager).
#[derive(Default)]
struct RecordingRouter {
    registered: Mutex<Vec<Id>>,
}

#[async_trait]
impl Router for RecordingRouter {
    fn add_chain(&self, chain: Id, _handler: Arc<dyn ChainMessageSink>) {
        self.registered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(chain);
    }

    async fn handle_inbound(&self, _msg: ava_engine::networking::router::InboundMessage) {}

    async fn register_request(&self, _node: NodeId, _chain: Id, _request_id: u32, _op_tag: u8) {}

    fn health_check(&self) -> bool {
        true
    }
}

/// `pipeline_wrapping_order` — the VM stack is built in the exact ratified order
/// `inner → tracedvm(primaryAlias) → proposervm → metervm → tracedvm("proposervm")
///  → change-notifier`, the DB stack carries the documented prefixes, the VM
/// initializes to genesis, and the handler is registered with the router.
#[tokio::test]
async fn pipeline_wrapping_order() {
    let token = CancellationToken::new();
    let reg = Registry::new();

    // ---- DB stack: base → meterdb → prefixdb(chainID) → {prefix(VM), prefix(bs)}.
    let chain_id = Id::from([5u8; 32]);
    let base = MemDb::new();
    let stack: DbStack = build_db_stack(chain_id, base, &reg).expect("db stack");
    // A write under the VM DB lands only under the VM namespace, not the
    // bootstrapping one (distinct prefixes).
    stack.vm_db.put(b"k", b"v").expect("vm put");
    assert_eq!(stack.vm_db.get(b"k").expect("vm get"), b"v");
    assert!(
        matches!(
            stack.bootstrapping_db.get(b"k"),
            Err(ava_database::Error::NotFound)
        ),
        "VM and bootstrapping DBs are namespaced apart ({VM_DB_PREFIX:?} vs {BOOTSTRAPPING_DB_PREFIX:?})"
    );

    // ---- VM wrapping order. The *type* is the proof of order (00 §11.1.2):
    // ChangeNotifier< TracedVm< MeterVm< ProposerVm< TracedVm<TestVm>, FixedState > > > >.
    let (identity, node_id) = staking_identity();
    let set = {
        let mut m = BTreeMap::new();
        m.insert(
            node_id,
            GetValidatorOutput {
                node_id,
                public_key: None,
                weight: 1,
            },
        );
        m
    };
    let validator_state = FixedState { set };
    let ctx = test_chain_context();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(std::time::UNIX_EPOCH));

    let wrapped: WrappedVm<TestVm, FixedState> = wrap_snowman_vm(
        TestVm::new(),
        "P",
        Arc::clone(&ctx),
        Arc::clone(&clock),
        validator_state.clone(),
        Arc::clone(&stack.vm_db),
        Some(identity.clone()),
        &reg,
        Arc::new(|| {}),
    )
    .expect("wrap");

    // The `WrappedVm<TestVm, FixedState>` binding above is the compile-time proof
    // of the exact wrapping order (00 §11.1.2). Walk it outermost → innermost via
    // the concrete `.inner()` accessors as a run-time cross-check: each layer's
    // inferred type must match, or this would not compile.
    let cn: &ChangeNotifier<_> = &wrapped; // outermost: change-notifier
    let traced_outer: &TracedVm<_> = cn.inner(); // tracedvm("proposervm")
    let metered: &MeterVm<_> = traced_outer.inner(); // metervm
    let proposer: &ProposerVm<_, FixedState> = metered.inner(); // proposervm
    let _traced_inner: &TracedVm<TestVm> = proposer.inner(); // tracedvm(primaryAlias) -> inner

    // ---- initialize the full stack ⇒ genesis is last-accepted.
    let mut wrapped = wrapped;
    let db: Arc<dyn DynDatabase> = Arc::clone(&stack.vm_db);
    let app_sender: Arc<dyn AppSender> = Arc::new(NoopAppSender);
    wrapped
        .initialize(
            &token,
            Arc::clone(&ctx),
            db,
            b"genesis",
            b"",
            b"",
            Vec::new(),
            app_sender,
        )
        .await
        .expect("initialize wrapped vm");
    let last = wrapped.last_accepted(&token).await.expect("last accepted");
    let genesis = wrapped
        .get_block_id_at_height(&token, 0)
        .await
        .expect("genesis at height 0");
    assert_eq!(last, genesis, "wrapped VM initializes to genesis");

    // ---- create_snowman_chain registers the handler with the router. Use a
    // fresh registry: the standalone `wrap_snowman_vm`/`build_db_stack` above
    // already registered metervm/meterdb collectors under `reg`.
    let chain_reg = Registry::new();
    let router = RecordingRouter::default();
    let sender = RecordingSender::new();
    let (validators, _ids) = build_validators(node_id);

    // The frontier-agreement beacon set (synthetic: this single node, weight 1).
    let beacons: BTreeMap<NodeId, u64> = {
        let mut m = BTreeMap::new();
        m.insert(node_id, 1u64);
        m
    };

    let chain = create_snowman_chain(
        &token,
        chain_id,
        Id::EMPTY,
        DEFAULT_PARAMETERS,
        MemDb::new(),
        "P",
        Arc::clone(&ctx),
        Arc::clone(&clock),
        validator_state,
        Some(identity),
        TestVm::new(),
        Vec::new(),
        b"genesis",
        Arc::clone(&sender),
        Arc::new(NoopAppSender),
        validators,
        beacons,
        &router,
        &chain_reg,
    )
    .await
    .expect("create snowman chain");

    assert_eq!(chain.chain_id, chain_id);
    assert_eq!(
        *router.registered.lock().unwrap_or_else(|e| e.into_inner()),
        vec![chain_id],
        "the chain's handler sink is registered with the router"
    );

    // The chain starts in `Bootstrapping`: its observability handle (the
    // ConsensusContext.state) is set when the handler activates the bootstrapper.
    use ava_snow::EngineState;
    assert_eq!(
        **chain.ctx.state.load(),
        EngineState::Initializing,
        "consensus context starts in Initializing before the handler starts"
    );

    // Start the handler: it activates the initial (Bootstrapping) engine, which
    // begins frontier discovery (`SendGetAcceptedFrontier` to the beacons) and
    // flips the ConsensusContext state to `Bootstrapping`.
    let join = chain.handler.start();

    // Wait until the bootstrapper has run `start` (state flipped) — virtual time;
    // we poll the observability handle rather than sleep on a wall clock.
    let observed_bootstrapping =
        wait_for(|| matches!(**chain.ctx.state.load(), EngineState::Bootstrapping)).await;
    assert!(
        observed_bootstrapping,
        "handler.start activates the bootstrapper (ConsensusContext.state -> Bootstrapping)"
    );

    // The bootstrapper emitted a `SendGetAcceptedFrontier` to the beacon set.
    let saw_frontier = sender
        .log
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|s| {
            matches!(
                s,
                Sent::GetAcceptedFrontier { nodes, .. } if nodes == &vec![node_id]
            )
        });
    assert!(
        saw_frontier,
        "bootstrapper emitted SendGetAcceptedFrontier to the beacons"
    );

    // Halt + join cleanly (no leaked task).
    token.cancel();
    join.await.expect("handler joins after halt");
}

/// Polls `cond` on each tokio scheduler turn (no wall-clock sleep — virtual,
/// deterministic), giving the spawned handler task room to run, up to a bounded
/// number of yields. Returns whether `cond` became true.
async fn wait_for<F: Fn() -> bool>(cond: F) -> bool {
    for _ in 0..1024 {
        if cond() {
            return true;
        }
        tokio::task::yield_now().await;
    }
    cond()
}

/// Builds a `DefaultManager` with one validator (this node) on the empty subnet.
fn build_validators(node: NodeId) -> (Arc<DefaultManager>, Vec<NodeId>) {
    let mgr = Arc::new(DefaultManager::new());
    mgr.add_staker(Id::EMPTY, node, None, Id::EMPTY, 1)
        .expect("add staker");
    (mgr, vec![node])
}
