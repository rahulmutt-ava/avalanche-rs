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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use ava_database::{Database, DynDatabase, MeterDb, PrefixDb};
use ava_engine::common::sender::Sender;
use ava_engine::networking::handler::{ChainHandler, ChainHandlerSink, EngineManager};
use ava_engine::networking::router::Router;
use ava_engine::snowman::engine::{Config as SnowmanConfig, SnowmanEngine};
use ava_proposervm::{ProposerVm, StakingIdentity};
use ava_snow::snowball::{Parameters, SnowballFactory};
use ava_snow::snowman::Topological;
use ava_snow::{ChainContext, EngineState, EngineType};
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
use ava_vm::vm::{HttpHandler, Vm, VmEvent};
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

/// The handles a created Snowman chain exposes (specs 07 §8.2): the wrapped VM
/// (behind the engine's mutex), the per-chain handler sink registered with the
/// router, and the chain's consensus parameters.
pub struct SnowmanChain<V: ChainVm, S, M> {
    /// The chain id.
    pub chain_id: Id,
    /// The Snowman engine, ready to be driven (or spawned in a `ChainHandler`).
    pub engine: SnowmanEngine<V, S, M>,
    /// The handler sink registered with the router (kept alive by the caller).
    pub sink: ChainHandlerSink,
    /// The handler actor (the caller spawns it via `ChainHandler::start`).
    pub handler: ChainHandler,
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
    router: &R,
    reg: &Registry,
) -> Result<WrappedSnowmanChain<V, S, Snd, M>>
where
    D: Database + 'static,
    V: ChainVm,
    S: ValidatorState + 'static,
    Snd: Sender,
    M: ValidatorManager,
    R: Router,
{
    // 1. DB stack: base → meterdb → prefixdb(chainID) → {prefix(VM), prefix(bs)}.
    let db_stack = build_db_stack(chain_id, base_db, reg)?;

    // 2. VM wrapping order (00 §11.1.2). The proposervm gets its own VM-DB-rooted
    //    state DB; in Go the proposervm shares the chain prefix DB (it persists
    //    under the VM DB). We pass the VM DB so the wrapped state is namespaced
    //    under the chain.
    let on_change: OnChange = Arc::new(|| {});
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
    let last_accepted = vm.last_accepted(token).await?;

    // 5. Build the Topological consensus core rooted at the VM's last-accepted.
    let consensus = Topological::new_default(SnowballFactory, params, last_accepted, 0)
        .map_err(|e| crate::error::Error::Other(format!("topological: {e}")))?;

    // 6. Build the Snowman engine over the wrapped VM, sender, validators.
    let engine_cfg = SnowmanConfig {
        subnet_id,
        params,
        vm: Arc::new(AsyncMutex::new(vm)),
        sender,
        validators,
        token: token.clone(),
    };
    let engine = SnowmanEngine::new(engine_cfg, Box::new(consensus));

    // 7. Per-chain ChainHandler actor + register its sink with the router (which
    //    owns the AdaptiveTimeoutManager).
    let engines = EngineManager::new(EngineType::Snowman);
    // M4.30a: the handler now takes the receiver end of an engine-transition
    // channel. This chain wires no handler-driven engines yet (the engine is
    // returned separately and driven directly), so the `tx` is dropped; M4.30b
    // will register the engine adapters and hold the `tx`.
    let (_transition_tx, transition_rx) = ava_engine::networking::transition_channel(8);
    let (handler, sink, _vm_tx) = ChainHandler::new(
        engines,
        EngineState::Initializing,
        1024,
        Duration::from_secs(1),
        token.clone(),
        transition_rx,
    );
    router.add_chain(chain_id, Arc::new(sink.clone()));

    Ok(SnowmanChain {
        chain_id,
        engine,
        sink,
        handler,
    })
}
