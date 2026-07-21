// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `create_snowman_chain` pipeline (specs 07 §8.2, 00 §11.1.2).
//!
//! Reproduces `chains/manager.go::createSnowmanChain` exactly:
//!
//! 1. **DB stack:** `base → meterdb → prefixdb(chainID) → {prefix(VM),
//!    prefix(bootstrapping)}`.
//! 2. **VM wrapping order (00 §11.1.2, ratified):**
//!    `inner → tracedvm(primaryAlias)? → proposervm → metervm? →
//!     tracedvm("proposervm")? → ChangeNotifier`, then `initialize`.
//! 3. Build the `Topological` consensus core + the `SnowmanEngine`, the per-chain
//!    `ChainHandler` actor, and register the handler's sink with the
//!    `ChainRouter` (which owns the `AdaptiveTimeoutManager`).
//!
//! [`ChangeNotifier`] is the outermost wrapper: a `ChainVm` that fires an
//! `on_change` callback on `build_block` / `set_state` / a *changed*
//! `set_preference`, waking the engine's notification forwarder (Go
//! `block.ChangeNotifier`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use ava_database::{Database, DynDatabase, MeterDb, PrefixDb};
use ava_engine::common::sender::Sender;
use ava_engine::networking::handler::{ChainHandler, ChainHandlerSink, EngineManager};
use ava_engine::networking::router::Router;
use ava_engine::networking::{BootstrapperEngineAdapter, SnowmanEngineAdapter, transition_channel};
use ava_engine::snowman::Bootstrapper;
use ava_engine::snowman::bootstrap::Config as BootstrapConfig;
use ava_engine::snowman::engine::{Config as SnowmanConfig, SnowmanEngine};
use ava_proposervm::{ProposerVm, StakingIdentity};
use ava_snow::acceptor::NoOpAcceptor;
use ava_snow::snowball::{Parameters, SnowballFactory};
use ava_snow::snowman::Topological;
use ava_snow::{ChainContext, ConsensusContext, EngineState, EngineType};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::Clock;
use ava_validators::ValidatorManager;
use ava_validators::state::ValidatorState;
use ava_version::application::Application;
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{
    BatchedChainVm, Block, BlockContext, BuildBlockWithContext, ChainVm, SetPreferenceWithContext,
    StateSummary, StateSyncableVm,
};
use ava_vm::connector::Connector;
use ava_vm::error::{Error as VmError, Result as VmResult};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::middleware::{MeterVm, TracedVm};
use ava_vm::vm::{HttpHandler, PendingWorkWaiter, Vm, VmEvent};
use prometheus::Registry;

use crate::error::Result;

/// `VMDBPrefix` — the VM's DB namespace under the chain prefix.
pub const VM_DB_PREFIX: &[u8] = b"vm";
/// `ChainBootstrappingDBPrefix` — the bootstrapper's DB namespace.
pub const BOOTSTRAPPING_DB_PREFIX: &[u8] = b"bs";

/// The `OnChange` callback fired by [`ChangeNotifier`] (Go
/// `block.ChangeNotifier.OnChange`).
pub type OnChange = Arc<dyn Fn() + Send + Sync>;

/// `block.ChangeNotifier` — the outermost VM wrapper. Forwards every `ChainVm`
/// call to the inner VM and fires `on_change` on `build_block`, `set_state`, and
/// a *changed* `set_preference` (so the engine re-subscribes to the VM's
/// build-readiness — Go `notifier.go`).
pub struct ChangeNotifier<V: ChainVm> {
    inner: V,
    on_change: OnChange,
    last_pref: Mutex<Option<Id>>,
    supports_build_with_context: bool,
    supports_set_preference_with_context: bool,
    supports_batched: bool,
    supports_state_sync: bool,
}

impl<V: ChainVm> ChangeNotifier<V> {
    /// Wraps `inner`, firing `on_change` on the change events above.
    pub fn new(inner: V, on_change: OnChange) -> Self {
        let supports_build_with_context = inner.as_build_with_context().is_some();
        let supports_set_preference_with_context = inner.as_set_preference_with_context().is_some();
        let supports_batched = inner.as_batched().is_some();
        let supports_state_sync = inner.as_state_syncable().is_some();
        Self {
            inner,
            on_change,
            last_pref: Mutex::new(None),
            supports_build_with_context,
            supports_set_preference_with_context,
            supports_batched,
            supports_state_sync,
        }
    }

    /// Borrows the inner VM (introspection helper for the pipeline test).
    #[must_use]
    pub fn inner(&self) -> &V {
        &self.inner
    }
}

#[async_trait]
impl<V: ChainVm> AppHandler for ChangeNotifier<V> {
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> VmResult<()> {
        self.inner
            .app_request(token, node, request_id, deadline, request)
            .await
    }

    async fn app_request_failed(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> VmResult<()> {
        self.inner
            .app_request_failed(token, node, request_id, err)
            .await
    }

    async fn app_response(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> VmResult<()> {
        self.inner
            .app_response(token, node, request_id, response)
            .await
    }

    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> VmResult<()> {
        self.inner.app_gossip(token, node, msg).await
    }
}

#[async_trait]
impl<V: ChainVm> HealthCheck for ChangeNotifier<V> {
    async fn health_check(&self, token: &CancellationToken) -> VmResult<serde_json::Value> {
        self.inner.health_check(token).await
    }
}

#[async_trait]
impl<V: ChainVm> Connector for ChangeNotifier<V> {
    async fn connected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> VmResult<()> {
        self.inner.connected(token, node, version).await
    }

    async fn disconnected(&mut self, token: &CancellationToken, node: NodeId) -> VmResult<()> {
        self.inner.disconnected(token, node).await
    }
}

#[async_trait]
impl<V: ChainVm> Vm for ChangeNotifier<V> {
    #[allow(clippy::too_many_arguments)]
    async fn initialize(
        &mut self,
        token: &CancellationToken,
        chain_ctx: Arc<ChainContext>,
        db: Arc<dyn DynDatabase>,
        genesis_bytes: &[u8],
        upgrade_bytes: &[u8],
        config_bytes: &[u8],
        fxs: Vec<Fx>,
        app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        self.inner
            .initialize(
                token,
                chain_ctx,
                db,
                genesis_bytes,
                upgrade_bytes,
                config_bytes,
                fxs,
                app_sender,
            )
            .await
    }

    async fn set_state(&mut self, token: &CancellationToken, state: EngineState) -> VmResult<()> {
        let res = self.inner.set_state(token, state).await;
        (self.on_change)();
        res
    }

    async fn shutdown(&mut self, token: &CancellationToken) -> VmResult<()> {
        self.inner.shutdown(token).await
    }

    async fn version(&self, token: &CancellationToken) -> VmResult<String> {
        self.inner.version(token).await
    }

    async fn create_handlers(
        &mut self,
        token: &CancellationToken,
    ) -> VmResult<HashMap<String, HttpHandler>> {
        self.inner.create_handlers(token).await
    }

    async fn new_http_handler(
        &mut self,
        token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        self.inner.new_http_handler(token).await
    }

    async fn wait_for_event(&self, token: &CancellationToken) -> VmResult<VmEvent> {
        self.inner.wait_for_event(token).await
    }
}

#[async_trait]
impl<V: ChainVm> ChainVm for ChangeNotifier<V> {
    async fn build_block(&mut self, token: &CancellationToken) -> VmResult<Arc<dyn Block>> {
        let res = self.inner.build_block(token).await;
        (self.on_change)();
        res
    }

    async fn get_block(&self, token: &CancellationToken, id: Id) -> VmResult<Arc<dyn Block>> {
        self.inner.get_block(token, id).await
    }

    async fn parse_block(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn Block>> {
        self.inner.parse_block(token, bytes).await
    }

    async fn set_preference(&mut self, token: &CancellationToken, id: Id) -> VmResult<()> {
        // Only fire OnChange when the preference actually changes (Go).
        let changed = {
            let mut last = self.last_pref.lock().unwrap_or_else(|e| e.into_inner());
            let changed = *last != Some(id);
            *last = Some(id);
            changed
        };
        let res = self.inner.set_preference(token, id).await;
        if changed {
            (self.on_change)();
        }
        res
    }

    async fn last_accepted(&self, token: &CancellationToken) -> VmResult<Id> {
        self.inner.last_accepted(token).await
    }

    async fn get_block_id_at_height(&self, token: &CancellationToken, height: u64) -> VmResult<Id> {
        self.inner.get_block_id_at_height(token, height).await
    }

    fn as_build_with_context(&self) -> Option<&dyn BuildBlockWithContext> {
        if self.supports_build_with_context {
            Some(self)
        } else {
            None
        }
    }

    fn as_set_preference_with_context(&self) -> Option<&dyn SetPreferenceWithContext> {
        if self.supports_set_preference_with_context {
            Some(self)
        } else {
            None
        }
    }

    fn as_batched(&self) -> Option<&dyn BatchedChainVm> {
        if self.supports_batched {
            Some(self)
        } else {
            None
        }
    }

    fn as_state_syncable(&self) -> Option<&dyn StateSyncableVm> {
        if self.supports_state_sync {
            Some(self)
        } else {
            None
        }
    }
}

#[async_trait]
impl<V: ChainVm> BuildBlockWithContext for ChangeNotifier<V> {
    async fn build_block_with_context(
        &self,
        token: &CancellationToken,
        ctx: &BlockContext,
    ) -> VmResult<Arc<dyn Block>> {
        let inner = self
            .inner
            .as_build_with_context()
            .ok_or(VmError::RemoteVmNotImplemented)?;
        inner.build_block_with_context(token, ctx).await
    }
}

#[async_trait]
impl<V: ChainVm> SetPreferenceWithContext for ChangeNotifier<V> {
    async fn set_preference_with_context(
        &self,
        token: &CancellationToken,
        id: Id,
        ctx: &BlockContext,
    ) -> VmResult<()> {
        let inner = self
            .inner
            .as_set_preference_with_context()
            .ok_or(VmError::RemoteVmNotImplemented)?;
        inner.set_preference_with_context(token, id, ctx).await
    }
}

#[async_trait]
impl<V: ChainVm> BatchedChainVm for ChangeNotifier<V> {
    async fn get_ancestors(
        &self,
        token: &CancellationToken,
        blk_id: Id,
        max_blocks_num: usize,
        max_blocks_size: usize,
        max_retrieval_time: Duration,
    ) -> VmResult<Vec<Vec<u8>>> {
        let inner = self
            .inner
            .as_batched()
            .ok_or(VmError::RemoteVmNotImplemented)?;
        inner
            .get_ancestors(
                token,
                blk_id,
                max_blocks_num,
                max_blocks_size,
                max_retrieval_time,
            )
            .await
    }

    async fn batched_parse_block(
        &self,
        token: &CancellationToken,
        blks: &[Vec<u8>],
    ) -> VmResult<Vec<Arc<dyn Block>>> {
        let inner = self
            .inner
            .as_batched()
            .ok_or(VmError::RemoteVmNotImplemented)?;
        inner.batched_parse_block(token, blks).await
    }
}

#[async_trait]
impl<V: ChainVm> StateSyncableVm for ChangeNotifier<V> {
    async fn state_sync_enabled(&self, token: &CancellationToken) -> VmResult<bool> {
        match self.inner.as_state_syncable() {
            Some(inner) => inner.state_sync_enabled(token).await,
            None => Ok(false),
        }
    }

    async fn get_ongoing_sync_state_summary(
        &self,
        token: &CancellationToken,
    ) -> VmResult<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(VmError::StateSyncableVmNotImplemented)?;
        inner.get_ongoing_sync_state_summary(token).await
    }

    async fn get_last_state_summary(
        &self,
        token: &CancellationToken,
    ) -> VmResult<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(VmError::StateSyncableVmNotImplemented)?;
        inner.get_last_state_summary(token).await
    }

    async fn parse_state_summary(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(VmError::StateSyncableVmNotImplemented)?;
        inner.parse_state_summary(token, bytes).await
    }

    async fn get_state_summary(
        &self,
        token: &CancellationToken,
        height: u64,
    ) -> VmResult<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(VmError::StateSyncableVmNotImplemented)?;
        inner.get_state_summary(token, height).await
    }
}

// ---------------------------------------------------------------------------
// DB stack + VM wrapping
// ---------------------------------------------------------------------------

/// The per-chain DB stack (specs 07 §8.2 step 1):
/// `base → meterdb → prefixdb(chainID) → {prefix(VM), prefix(bootstrapping)}`.
///
/// The VM DB is returned type-erased as `Arc<dyn DynDatabase>` (what
/// `Vm::initialize` takes); the bootstrapping DB is likewise erased for the
/// bootstrapper.
pub struct DbStack {
    /// `prefixdb(VMDBPrefix, prefixdb(chainID, meterdb(base)))`.
    pub vm_db: Arc<dyn DynDatabase>,
    /// `prefixdb(BootstrappingDBPrefix, prefixdb(chainID, meterdb(base)))`.
    pub bootstrapping_db: Arc<dyn DynDatabase>,
}

/// Builds the DB stack for `chain_id` over the base `db`, registering the
/// meterdb metrics into `reg` (Go `createSnowmanChain` step 1).
///
/// # Errors
/// Propagates a meterdb metric-registration failure.
pub fn build_db_stack<D: Database + 'static>(
    chain_id: Id,
    db: D,
    reg: &Registry,
) -> Result<DbStack> {
    let meter_db = MeterDb::new(reg, db)?;
    // prefixdb(chainID) shared by the VM + bootstrapping sub-databases.
    let prefix_db = Arc::new(PrefixDb::new(&chain_id.to_bytes(), meter_db));
    let vm_db = PrefixDb::new_arc(VM_DB_PREFIX, Arc::clone(&prefix_db));
    let bootstrapping_db = PrefixDb::new_arc(BOOTSTRAPPING_DB_PREFIX, prefix_db);
    Ok(DbStack {
        vm_db: Arc::new(vm_db),
        bootstrapping_db: Arc::new(bootstrapping_db),
    })
}

/// The fully-wrapped Snowman VM type produced by [`wrap_snowman_vm`] with both
/// tracing and metering enabled — the **maximal** stack. The type *is* the proof
/// of the exact wrapping order (00 §11.1.2):
///
/// `ChangeNotifier< TracedVm< MeterVm< ProposerVm< TracedVm<V>, S > > > >`
///
/// reading outermost→innermost: change-notifier, tracedvm("proposervm"),
/// metervm, proposervm, tracedvm(primaryAlias), inner VM.
pub type WrappedVm<V, S> = ChangeNotifier<TracedVm<MeterVm<ProposerVm<TracedVm<V>, S>>>>;

/// Wraps the inner VM in the exact ratified order (00 §11.1.2), with tracing and
/// metering **both enabled** (the maximal stack):
/// `inner → tracedvm(primaryAlias) → proposervm → metervm → tracedvm("proposervm")
///  → change-notifier`.
///
/// The proposervm runs in its pre-fork (passthrough) regime for genesis-era
/// timestamps, so a test VM's blocks flow through unchanged.
///
/// # Errors
/// Propagates a metervm metric-registration failure.
#[allow(clippy::too_many_arguments)]
pub fn wrap_snowman_vm<V: ChainVm, S: ValidatorState + 'static>(
    inner: V,
    primary_alias: &str,
    ctx: Arc<ChainContext>,
    clock: Arc<dyn Clock>,
    validator_state: S,
    proposervm_db: Arc<dyn DynDatabase>,
    identity: Option<StakingIdentity>,
    reg: &Registry,
    on_change: OnChange,
) -> Result<WrappedVm<V, S>> {
    // inner -> tracedvm(primaryAlias)
    let traced_inner = TracedVm::new(inner, primary_alias.to_string());
    // -> proposervm
    let proposer = ProposerVm::new(
        traced_inner,
        ctx,
        clock,
        validator_state,
        proposervm_db,
        identity,
    );
    // -> metervm
    let metered = MeterVm::new(proposer, reg)?;
    // -> tracedvm("proposervm")
    let traced_outer = TracedVm::new(metered, "proposervm".to_string());
    // -> change-notifier
    Ok(ChangeNotifier::new(traced_outer, on_change))
}

// ---------------------------------------------------------------------------
// create_snowman_chain
// ---------------------------------------------------------------------------

/// The handles a created Snowman chain exposes (specs 07 §8.2): a fully-wired,
/// startable chain. The `Bootstrapper` and `SnowmanEngine` are both wrapped in
/// the M4.30a [`ChainEngine`](ava_engine::networking::handler::ChainEngine)
/// adapters and registered on the handler's `EngineManager`
/// (`Bootstrapping`→bootstrapper, `NormalOp`→snowman); the handler owns the
/// engine-transition channel. Starting the [`handler`](Self::handler) activates
/// the bootstrapper (frontier discovery), and on bootstrap completion the
/// adapter requests the `NormalOp` transition.
///
/// The engines have moved into the handler's `EngineManager`, so the
/// observability handle is the shared [`ConsensusContext`]: its
/// `state: ArcSwap<EngineState>` reflects the live engine phase
/// (`Initializing → Bootstrapping → NormalOp`).
pub struct SnowmanChain<V: ChainVm, S, M> {
    /// The chain id.
    pub chain_id: Id,
    /// The shared consensus context — the observability handle for the live
    /// engine phase (`ctx.state`) and the acceptor callbacks. Shared (via `Arc`)
    /// between the bootstrapper and the snowman engine.
    pub ctx: Arc<ConsensusContext>,
    /// The handler sink registered with the router (kept alive by the caller).
    pub sink: ChainHandlerSink,
    /// The handler actor (the caller spawns it via `ChainHandler::start`). Owns
    /// both engine adapters + the transition channel; starting it activates the
    /// initial (`Bootstrapping`) engine.
    pub handler: ChainHandler,
    /// The height the `Topological` consensus core was rooted at — the height of
    /// the VM's last-accepted block at chain creation (`0` for a fresh genesis
    /// tip, the persisted height for a node that recovered an advanced tip from
    /// disk; M9.15 STEP (b)).
    pub last_accepted_height: u64,
    /// The VM→engine notification channel (`common.Message`). Sending
    /// [`VmEvent::PendingTxs`](ava_vm::vm::VmEvent::PendingTxs) here drives the
    /// running [`SnowmanEngine`](ava_engine::snowman::SnowmanEngine) to build,
    /// issue, and (given votes) accept a block — the in-process equivalent of a
    /// VM's `toEngine` channel. The caller keeps this to issue blocks through the
    /// genuine engine path (M9.15 STEP (m)); the handler owns the receiver.
    pub vm_tx: mpsc::Sender<VmEvent>,
    /// The **shared** fully-wrapped VM (the same `Arc<Mutex<..>>` the engines
    /// hold), type-erased to the [`Vm`] object the API server's chain
    /// registration takes. The chain creator passes this to
    /// `ApiServer::register_chain` so the VM's `create_handlers` /
    /// `new_http_handler` mount at `/ext/bc/<chainID>/<ext>` (Go
    /// `chains/manager.go` → `server.RegisterChain`; M9.15 rung 2).
    pub vm: Arc<AsyncMutex<dyn Vm>>,
    /// Carries the generic `V`/`S`/`M` parameters (the concrete engines moved
    /// into the type-erased `EngineManager` inside the handler).
    _vm: std::marker::PhantomData<(V, S, M)>,
}

/// The fully-wrapped + engine-mounted chain for the maximal stack, generic over
/// the inner VM `V`, the validator state `S`, the sender `Snd`, and the
/// validator manager `M`.
pub type WrappedSnowmanChain<V, S, Snd, M> = SnowmanChain<WrappedVm<V, S>, Snd, M>;

/// Reproduces `chains/manager.go::createSnowmanChain` (specs 07 §8.2): builds the
/// DB stack, wraps the VM in the exact ratified order, `initialize`s it, builds
/// the `Topological` consensus + `SnowmanEngine`, creates the per-chain
/// `ChainHandler`, and registers the handler's sink with the `router` (which
/// owns the timeout manager).
///
/// The genesis is seeded by the inner VM at `initialize`; the engine's consensus
/// is rooted at the VM's last-accepted block.
///
/// # Errors
/// Propagates DB / VM-init / consensus-construction failures.
#[allow(clippy::too_many_arguments)]
pub async fn create_snowman_chain<D, V, S, Snd, M, R>(
    token: &CancellationToken,
    chain_id: Id,
    subnet_id: Id,
    params: Parameters,
    base_db: D,
    primary_alias: &str,
    chain_ctx: Arc<ChainContext>,
    clock: Arc<dyn Clock>,
    validator_state: S,
    identity: Option<StakingIdentity>,
    inner_vm: V,
    fxs: Vec<Fx>,
    genesis_bytes: &[u8],
    sender: Arc<Snd>,
    app_sender: Arc<dyn AppSender>,
    validators: Arc<M>,
    beacons: BTreeMap<NodeId, u64>,
    router: &R,
    reg: &Registry,
) -> Result<WrappedSnowmanChain<V, S, Snd, M>>
where
    D: Database + 'static,
    V: ChainVm + 'static,
    S: ValidatorState + 'static,
    Snd: Sender + 'static,
    M: ValidatorManager + 'static,
    R: Router,
{
    // 1. DB stack: base → meterdb → prefixdb(chainID) → {prefix(VM), prefix(bs)}.
    let db_stack = build_db_stack(chain_id, base_db, reg)?;

    // 2. VM wrapping order (00 §11.1.2). The proposervm gets its own VM-DB-rooted
    //    state DB; in Go the proposervm shares the chain prefix DB (it persists
    //    under the VM DB). We pass the VM DB so the wrapped state is namespaced
    //    under the chain.
    let on_change: OnChange = Arc::new(|| {});

    // Capture the lock-free proposal waiter from the inner VM BEFORE it is
    // wrapped and moved behind the consensus-shared mutex. Go:
    // snow/engine/common/notifier.go (NotificationForwarder) polls
    // WaitForEvent off the engine lock; the shared `Arc<Mutex<dyn Vm>>` here
    // forbids that, so a per-chain forwarder (spawned below) drives a lock-free
    // waiter instead. `None` (P/X/SAE today) means no forwarder is spawned.
    let pending_waiter: Option<Arc<dyn PendingWorkWaiter>> = inner_vm.pending_work_waiter();

    let mut vm = wrap_snowman_vm(
        inner_vm,
        primary_alias,
        Arc::clone(&chain_ctx),
        clock,
        validator_state,
        Arc::clone(&db_stack.vm_db),
        identity,
        reg,
        on_change,
    )?;

    // 3. Initialize the fully-wrapped VM with the per-chain ChainContext, the VM
    //    DB, genesis/upgrade/config bytes, fxs, and the AppSender.
    vm.initialize(
        token,
        Arc::clone(&chain_ctx),
        Arc::clone(&db_stack.vm_db),
        genesis_bytes,
        b"",
        b"",
        fxs,
        app_sender,
    )
    .await?;

    // 4. Wire the proposervm wrapper's preferred id from the inner last-accepted.
    //    The consensus core must be rooted at that block's *height* (Go
    //    `vm.GetBlock(vm.LastAccepted()).Height()`), so a node that recovered an
    //    advanced tip from disk roots consensus at the persisted height, not `0`
    //    (M9.15 STEP (b)). On a fresh genesis tip this is `0` — unchanged.
    let last_accepted = vm.last_accepted(token).await?;
    let last_accepted_height = vm.get_block(token, last_accepted).await?.height();

    // 5. Build the Topological consensus core rooted at the VM's last-accepted.
    let consensus =
        Topological::new_default(SnowballFactory, params, last_accepted, last_accepted_height)
            .map_err(|e| crate::error::Error::Other(format!("topological: {e}")))?;

    // 6. Share ONE wrapped-VM mutex between the Snowman engine and the
    //    bootstrapper. Only one engine is active at a time, so the shared mutex
    //    is correct: the bootstrapper accepts blocks forward, then consensus
    //    continues from the same last-accepted.
    let vm = Arc::new(AsyncMutex::new(vm));

    // 6a. Build the shared ConsensusContext — the observability handle whose
    //     `state: ArcSwap<EngineState>` the bootstrapper flips
    //     (Initializing → Bootstrapping → NormalOp). Read-only sync uses NoOp
    //     acceptors; registrant/indexer acceptors are a later concern (M4.x).
    let ctx = Arc::new(ConsensusContext::new(
        Arc::clone(&chain_ctx),
        primary_alias.to_string(),
        Arc::new(NoOpAcceptor),
        Arc::new(NoOpAcceptor),
    ));

    // 6b. Build the Snowman engine over the shared VM, sender, validators.
    let engine_cfg = SnowmanConfig {
        subnet_id,
        params,
        vm: Arc::clone(&vm),
        sender: Arc::clone(&sender),
        validators,
        token: token.clone(),
    };
    let engine = SnowmanEngine::new(engine_cfg, Box::new(consensus));

    // 6c. Build the Bootstrapper over the shared VM, ConsensusContext, sender,
    //     and the beacon set.
    let boot_cfg = BootstrapConfig {
        subnet_id,
        ctx: Arc::clone(&ctx),
        vm: Arc::clone(&vm),
        sender: Arc::clone(&sender),
        beacons,
        token: token.clone(),
    };
    let bootstrapper = Bootstrapper::new(boot_cfg);

    // 7. Wrap both engines in the M4.30a ChainEngine adapters and register them
    //    on the EngineManager (Bootstrapping → bootstrapper, NormalOp → snowman).
    //    The handler owns the receiver end of the transition channel; the
    //    bootstrapper adapter holds the sender and requests `NormalOp` once the
    //    bootstrapper hands off.
    //    The Getter is shared between both adapters (same Arc<Mutex<V>> + Arc<S>)
    //    and answers inbound Get* requests regardless of engine phase.
    let getter = Arc::new(ava_engine::snowman::Getter::new(
        Arc::clone(&vm),
        Arc::clone(&sender),
        token.clone(),
    ));
    let (transition_tx, transition_rx) = transition_channel(8);
    let boot_adapter = BootstrapperEngineAdapter::new(
        bootstrapper,
        transition_tx,
        0,
        Arc::clone(&getter),
        Arc::clone(&vm),
        token.clone(),
    );
    let snowman_adapter = SnowmanEngineAdapter::new(engine, getter, Arc::clone(&vm), token.clone());

    let mut engines = EngineManager::new(EngineType::Snowman);
    engines.register(EngineState::Bootstrapping, Box::new(boot_adapter));
    engines.register(EngineState::NormalOp, Box::new(snowman_adapter));

    // 7a. Per-chain ChainHandler actor, starting in Bootstrapping so that
    //     handler.start() immediately activates the bootstrapper
    //     (→ SendGetAcceptedFrontier to the beacons). Register its sink with the
    //     router (which owns the AdaptiveTimeoutManager).
    let (handler, sink, vm_tx) = ChainHandler::new(
        engines,
        EngineState::Bootstrapping,
        1024,
        Duration::from_secs(1),
        token.clone(),
        transition_rx,
    );
    router.add_chain(chain_id, Arc::new(sink.clone()));

    // 7b. Production NotificationForwarder (Go snow/engine/common/notifier.go:31-134,
    //     started at handler start per handler.go:254-255): a VM pending-work
    //     signal becomes an engine `PendingTxs` build trigger. Spawned ONLY for a
    //     VM that hands out a lock-free waiter (`Some` — the EVM VM today; P/X/SAE
    //     return `None`, so their chains spawn no task and see no extra sends).
    //     See `forward_pending_work` for the loop's invariants.
    if let Some(waiter) = pending_waiter {
        tokio::spawn(forward_pending_work(waiter, vm_tx.clone(), token.clone()));
    }

    Ok(SnowmanChain {
        chain_id,
        ctx,
        sink,
        handler,
        last_accepted_height,
        vm_tx,
        // The same shared mutex the engines hold, unsized to the `dyn Vm` the
        // API server's `register_chain` takes (M9.15 rung 2).
        vm,
        _vm: std::marker::PhantomData,
    })
}

/// Retry floor between consecutive forwarder signals. After a send that
/// produces no block (proposervm windower "not my slot yet"), buildable work
/// still exists and the ACP-226 pacing inside [`PendingWorkWaiter::wait`] has
/// already elapsed, so `wait()` returns instantly — without this floor the
/// loop would busy-spin sends. Coreth's analogue is the 100 ms
/// `RetryDelay`/`lastBuildTime` arm inside `waitForEvent`; we keep the
/// established 2 s cadence (design:
/// `docs/superpowers/specs/2026-07-20-forwarder-rearm-pacing-design.md`).
const FORWARDER_RETRY_FLOOR: Duration = Duration::from_secs(2);

/// The production NotificationForwarder loop: parks on the ACP-226-paced
/// [`PendingWorkWaiter::wait`] and turns each release into ONE engine
/// `PendingTxs` signal, so EVERY send — first and re-arm alike — respects the
/// parent's minimum block delay (coreth parity: its NotificationForwarder
/// re-enters `WaitForEvent` before every signal). `wait()` parking on an
/// empty pool doubles as the idle park, so no separate `has_pending()` guard
/// is needed. Holds NO VM lock (M7.18 lock-parking hazard): the waiter locks
/// only mempool `Arc`s. Exits on chain teardown (`token`) or a closed engine
/// channel; a full channel parks the send un-cancellably, as before.
async fn forward_pending_work(
    waiter: Arc<dyn PendingWorkWaiter>,
    vm_tx: mpsc::Sender<VmEvent>,
    token: CancellationToken,
) {
    loop {
        // Park until the VM has buildable work AND the parent's ACP-226
        // minimum delay has cleared, or the chain is torn down.
        tokio::select! {
            () = waiter.wait() => {}
            () = token.cancelled() => return,
        }
        // Signal once; spurious signals are harmless (the engine build
        // returns NotFound when there is nothing to build — engine.rs).
        if vm_tx.send(VmEvent::PendingTxs).await.is_err() {
            return;
        }
        // Anti-busy-spin retry floor — see [`FORWARDER_RETRY_FLOOR`].
        tokio::select! {
            () = tokio::time::sleep(FORWARDER_RETRY_FLOOR) => {}
            () = token.cancelled() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use ava_vm::vm::{PendingWorkWaiter, VmEvent};
    use tokio::sync::{Semaphore, mpsc};
    use tokio_util::sync::CancellationToken;

    use super::forward_pending_work;

    /// Narrow local mock: `wait()` resolves once per permit the test releases
    /// on `gate`, exactly like the production waiter's ACP-226-paced parking.
    /// `has_pending` is deliberately **decoupled** from `gate` via its own
    /// `pending` flag: `forward_pending_work` never calls `has_pending`
    /// itself, but the flag lets a test express the divergent state the old
    /// (pre-pacing) implementation reacted to — buildable work sitting in the
    /// mempool (`pending == true`) while `wait()` is still gated (pacing has
    /// not elapsed / the pool is otherwise empty of *new* releases).
    struct GatedWaiter {
        gate: Semaphore,
        pending: AtomicBool,
    }

    impl GatedWaiter {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                gate: Semaphore::new(0),
                pending: AtomicBool::new(false),
            })
        }

        fn release(&self, n: usize) {
            self.gate.add_permits(n);
        }

        fn set_pending(&self, pending: bool) {
            self.pending.store(pending, Ordering::Relaxed);
        }
    }

    #[async_trait]
    impl PendingWorkWaiter for GatedWaiter {
        fn has_pending(&self) -> bool {
            // Unused by the forwarder (pacing subsumed the re-arm guard);
            // required by the trait. Independent of `gate` — see the struct
            // doc comment.
            self.pending.load(Ordering::Relaxed)
        }

        async fn wait(&self) {
            self.gate
                .acquire()
                .await
                .expect("test semaphore is never closed")
                .forget();
        }
    }

    // All tests run under paused tokio time: timers auto-advance whenever the
    // runtime is idle, so the 60 s `timeout(..)` guards and the 2 s retry
    // floor elapse instantly and deterministically. (Safe here, unlike the
    // ava-evm pending_work_waiter integration tests, because the gate is a
    // semaphore the test controls — there is no MockClock for auto-advance to
    // race against.)

    #[tokio::test(start_paused = true)]
    async fn no_send_before_wait_releases_then_one_per_release() {
        let waiter = GatedWaiter::new();
        let (vm_tx, mut rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        // The sustained-load regression: while wait() is gated (pacing not yet
        // elapsed), NO signal may reach the engine — the old inner re-arm loop
        // sent unpaced here.
        assert!(
            tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .is_err(),
            "no PendingTxs before wait() releases (paced re-arm)"
        );

        waiter.release(1);
        let evt = tokio::time::timeout(Duration::from_secs(60), rx.recv())
            .await
            .expect("released wait() must produce a signal")
            .expect("engine channel stays open");
        assert!(
            matches!(evt, VmEvent::PendingTxs),
            "forwarder signals PendingTxs"
        );

        // One release => exactly one send: the floor elapses (auto-advance)
        // and wait() re-parks on the drained semaphore.
        assert!(
            tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .is_err(),
            "exactly one PendingTxs per wait() release"
        );

        token.cancel();
        task.await.expect("forwarder exits cleanly on cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn consecutive_sends_spaced_by_retry_floor() {
        let waiter = GatedWaiter::new();
        let (vm_tx, mut rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();
        // Two immediate releases: wait() resolves instantly twice, so ONLY the
        // floor separates the sends — without it they land in the same instant
        // (the busy-spin the floor exists to prevent).
        waiter.release(2);
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        rx.recv().await.expect("first signal");
        let first = tokio::time::Instant::now();
        rx.recv().await.expect("second signal");
        let elapsed = tokio::time::Instant::now().duration_since(first);
        assert!(
            elapsed >= Duration::from_secs(2),
            "consecutive sends must be spaced by the 2s retry floor, got {elapsed:?}"
        );

        token.cancel();
        task.await.expect("forwarder exits cleanly on cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_terminates_while_parked_in_wait() {
        let waiter = GatedWaiter::new();
        let (vm_tx, _rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        token.cancel();
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("cancel while parked in wait() must terminate the forwarder")
            .expect("forwarder must not panic");
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_terminates_during_retry_floor() {
        let waiter = GatedWaiter::new();
        let (vm_tx, mut rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();
        waiter.release(1);
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        rx.recv().await.expect("first signal");
        // The forwarder is now at (or entering) the floor sleep. Time is
        // paused and never advanced, so the cancelled branch is the only arm
        // of its select that can become ready.
        token.cancel();
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("cancel during the retry floor must terminate the forwarder")
            .expect("forwarder must not panic");
    }

    #[tokio::test(start_paused = true)]
    async fn rearm_stays_paced_while_work_pending() {
        let waiter = GatedWaiter::new();
        let (vm_tx, mut rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();

        // Mark work pending BEFORE the first send: on the single-threaded
        // paused-time runtime, the forwarder runs un-interrupted from
        // `wait()` through the send and (in the old shape) its
        // `has_pending()` re-arm check — the test task is not rescheduled
        // in between. So `has_pending()` must already read `true` at spawn
        // time for the divergent state to be observed at all.
        waiter.set_pending(true);
        waiter.release(1);
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        rx.recv().await.expect("first signal");

        // Buildable work remains pending but the gate has no more permits —
        // pacing has not elapsed. The old inner re-arm loop
        // (`while waiter.has_pending() { sleep(2s); send }`) fires an
        // unpaced second send here once the 2s floor auto-advances; the
        // paced implementation must stay parked in `wait()` regardless of
        // `has_pending()`.
        assert!(
            tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .is_err(),
            "re-arm must stay paced: no unpaced send while work is pending but wait() is gated"
        );

        // Releasing the gate is what unblocks the next signal — proving the
        // loop is still alive (parked in wait()), not wedged.
        waiter.release(1);
        let evt = tokio::time::timeout(Duration::from_secs(60), rx.recv())
            .await
            .expect("releasing the gate must produce a signal")
            .expect("engine channel stays open");
        assert!(
            matches!(evt, VmEvent::PendingTxs),
            "forwarder signals PendingTxs"
        );

        token.cancel();
        task.await.expect("forwarder exits cleanly on cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn closed_channel_terminates_forwarder() {
        let waiter = GatedWaiter::new();
        let (vm_tx, rx) = mpsc::channel::<VmEvent>(8);
        drop(rx);
        waiter.release(1);

        tokio::time::timeout(
            Duration::from_secs(60),
            forward_pending_work(
                Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
                vm_tx,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("a closed engine channel must terminate the forwarder");
    }
}
