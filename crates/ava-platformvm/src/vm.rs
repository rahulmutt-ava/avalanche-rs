// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PlatformVm` — the P-Chain `block.ChainVM` (`vms/platformvm/vm.go`, specs 08
//! §1; bootstrap is linear-only, NO state sync, 19 §5).
//!
//! [`PlatformVm`] wires the M4.20 [`BlockManager`] (verify/accept/reject/options
//! over the persisted [`State`]) and the M4.21 [`PChainValidatorManager`]
//! (the chain's [`ValidatorState`]) behind the [`Vm`]/[`ChainVm`] traits, and
//! drives the M4.24 genesis seeding on [`Vm::initialize`].
//!
//! ## Shared block-manager + the block wrapper
//!
//! The engine-facing [`ava_snow::Block`] returned by `get_block`/`parse_block`/
//! `build_block` carries `verify`/`accept`/`reject`, which mutate the
//! [`BlockManager`]. The VM therefore holds the manager (and the validator
//! manager) behind an `Arc<Mutex<…>>` ([`Shared`]); each returned [`PChainBlock`]
//! holds a clone so its decision methods can drive the shared manager. On accept
//! the VM re-`refresh`es the validator manager from the just-flushed state — the
//! production wiring point M4.21 deferred (the manager is *also* injected as the
//! [`BlockAcceptanceNotifier`] so the recently-accepted window updates inside
//! `accept`).
//!
//! ## NO state sync
//!
//! [`ChainVm::as_state_syncable`] returns `None` (19 §5): the P-Chain bootstraps
//! linearly only.
//!
//! ## Scope (M4.25, read-only sync)
//!
//! The M4.26 mempool ([`crate::txs::mempool::Mempool`]) is wired in (the builder
//! drains it in FIFO order), but it stays empty during read-only sync (no txs are
//! issued); the p2p gossip transport that would fill it is the deferred seam (see
//! [`crate::network`]). No JSON-RPC service (M4.28 — `create_handlers`
//! returns empty), no bootstrap-engine wiring beyond the VM hooks (M4.27).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_database::{Database, DynDatabase};
use ava_snow::{ChainContext, EngineState};
use ava_types::id::Id;
use ava_utils::clock::{Clock, RealClock};
use ava_validators::state::ValidatorState;
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::batched::{BatchedChainVm, INT_LEN};
use ava_vm::block::{Block as VmBlock, ChainVm, StateSyncableVm};
use ava_vm::connector::Connector;
use ava_vm::error::{Error as VmError, Result as VmResult};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{HttpHandler, LockOptions, Vm, VmEvent};

use crate::block::Block;
use crate::block::builder;
use crate::block::executor::BlockManager;
use crate::error::{Error, Result};
use crate::jsonrpc::registry_service;
use crate::state::chain::Chain;
use crate::state::state::State;
use crate::txs::codec;
use crate::txs::executor::{Backend, StakingConfig, UpgradeSchedule};
use crate::txs::fee::simple_calculator::StaticFeeConfig;
use crate::txs::mempool::Mempool;
use crate::validators::manager::PChainValidatorManager;

/// `SyncBound` — a built block's timestamp may be at most this far ahead of the
/// local clock (Go `vms/platformvm/block/builder.SyncBound`, 10s).
const SYNC_BOUND: Duration = Duration::from_secs(10);

mod dyndb;
pub use dyndb::DynDb;

/// The mutable core shared between the [`PlatformVm`] and every [`PChainBlock`]
/// it hands out: the block manager (which owns the persisted [`State`]).
/// Guarded by a single [`Mutex`] — the engine drives the VM as one actor, so
/// contention is structural, not concurrent.
struct Shared<D: Database> {
    manager: Mutex<BlockManager<D>>,
    /// The validator manager, `refresh`ed from state after each accept.
    validators: Arc<PChainValidatorManager<D>>,
}

impl<D: Database + 'static> Shared<D> {
    /// Re-captures the validator manager's read snapshot from the (just-flushed)
    /// state — the M4.21 production wiring point. Called after every accept.
    fn refresh_validators(&self) {
        let mgr = self.manager.lock();
        self.validators.refresh(mgr.state());
    }
}

/// The [`crate::service::ServiceState`] view over the VM's live state: every
/// read locks the block manager (the moral equivalent of Go's per-request
/// `vm.ctx.Lock` in `service.go`) and forwards to the persisted [`State`].
struct VmServiceState<D: Database + 'static> {
    shared: Arc<Shared<D>>,
}

impl<D: Database + 'static> crate::service::ServiceState for VmServiceState<D> {
    fn timestamp(&self) -> SystemTime {
        Chain::timestamp(self.shared.manager.lock().state())
    }
    fn current_supply(&self, subnet: Id) -> Result<u64> {
        Chain::current_supply(self.shared.manager.lock().state(), subnet)
    }
    fn fee_state(&self) -> crate::txs::fee::gas::GasState {
        Chain::fee_state(self.shared.manager.lock().state())
    }
    fn l1_validator_excess(&self) -> u64 {
        Chain::l1_validator_excess(self.shared.manager.lock().state())
    }
    fn get_l1_validator(
        &self,
        validation_id: Id,
    ) -> Result<crate::state::l1_validator::L1Validator> {
        Chain::get_l1_validator(self.shared.manager.lock().state(), validation_id)
    }
    fn chains(&self, subnet: Id) -> Vec<Id> {
        Chain::chains(self.shared.manager.lock().state(), subnet)
    }
    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>> {
        Chain::get_tx(self.shared.manager.lock().state(), tx_id)
    }
    fn get_block(&self, id: Id) -> Result<Vec<u8>> {
        self.shared.manager.lock().state().get_block(id)
    }
    fn get_block_id_at_height(&self, height: u64) -> Option<Id> {
        self.shared
            .manager
            .lock()
            .state()
            .get_block_id_at_height(height)
    }
    fn utxo_ids(&self, addr: &ava_types::short_id::ShortId, previous: Id, limit: usize) -> Vec<Id> {
        crate::state::State::utxo_ids(self.shared.manager.lock().state(), addr, previous, limit)
    }
    fn get_utxo(&self, id: Id) -> Result<crate::state::chain::UtxoBytes> {
        Chain::get_utxo(self.shared.manager.lock().state(), id)
    }
    fn subnets(&self) -> Vec<Id> {
        Chain::subnets(self.shared.manager.lock().state())
    }
    fn get_subnet_owner(&self, subnet: Id) -> Result<Vec<u8>> {
        Chain::get_subnet_owner(self.shared.manager.lock().state(), subnet)
    }
    fn get_subnet_manager(&self, subnet: Id) -> Result<Vec<u8>> {
        Chain::get_subnet_manager(self.shared.manager.lock().state(), subnet)
    }
    fn get_reward_utxos(&self, tx_id: Id) -> Vec<crate::state::chain::UtxoBytes> {
        Chain::get_reward_utxos(self.shared.manager.lock().state(), tx_id)
    }
    fn current_stakers(&self) -> Vec<crate::state::staker::Staker> {
        Chain::current_stakers(self.shared.manager.lock().state())
    }
    fn pending_stakers(&self) -> Vec<crate::state::staker::Staker> {
        Chain::pending_stakers(self.shared.manager.lock().state())
    }
}

/// The deferred `issueTx` admission seam: the P-Chain mempool is un-shared on
/// [`PlatformVm`] (not in `Shared`), so an RPC-issued tx cannot yet be admitted
/// from the per-request handler. The shared-mempool + gossip wiring is the M8
/// node-assembly concern (`tests/PORTING.md`). Returns the Go-byte-equal
/// rejection prefix wrapped with the deferral reason.
struct DeferredIssuer;

impl crate::service::TxIssuer for DeferredIssuer {
    fn issue_tx(&self, _tx: crate::txs::Tx) -> std::result::Result<(), String> {
        Err(
            "RPC issuance not yet wired (deferred: the P-Chain mempool is \
             un-shared on PlatformVm; shared-mempool + gossip admission is M8 \
             node assembly)"
                .to_owned(),
        )
    }
}

/// `platformvm.VM` — the P-Chain Snowman VM over the [`DynDb`]-adapted engine
/// database (specs 08 §1).
pub struct PlatformVm {
    /// `None` until [`initialize`](Vm::initialize) builds the shared core.
    shared: Option<Arc<Shared<DynDb>>>,
    /// The immutable chain identity/handles received at `initialize`.
    ctx: Option<Arc<ChainContext>>,
    /// The current engine phase (Go `vm.bootstrapped`/`vm.state`).
    state: EngineState,
    /// The currently preferred (leaf) block id (Go `vm.preferred`).
    preferred: Id,
    /// The genesis block id (the initial last-accepted / preference).
    genesis_id: Id,
    /// The decision-tx mempool (M4.26). Inbound gossip is admitted here via
    /// [`crate::network::TxGossipHandler`]; the builder drains it in FIFO order.
    /// Empty during read-only sync (no txs are issued).
    mempool: Mutex<Mempool>,
    /// The injectable clock — the ONLY wall-clock source (specs 24 hazard #5).
    /// Threaded into `build_block` (the new block's timestamp) AND the executor
    /// [`Backend`]'s `Fx` (locktime/credential checks), so both consensus-state
    /// time reads go through one clock. [`PlatformVm::new`] installs a
    /// [`RealClock`]; tests inject a `MockClock` via [`PlatformVm::with_clock`].
    clock: Arc<dyn Clock>,
}

impl Default for PlatformVm {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformVm {
    /// Builds an uninitialized `PlatformVm` reading time through the production
    /// [`RealClock`]. Call [`Vm::initialize`] before use.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(RealClock))
    }

    /// Builds an uninitialized `PlatformVm` reading time through `clock` — the
    /// determinism injection seam (specs 24 hazard #5). The clock backs both the
    /// `build_block` block-time read AND the executor [`Backend`]'s `Fx`
    /// locktime/credential checks, so the whole VM observes one clock. Used by the
    /// reexecute harness (M9.19 `replay_pchain`) to drive deterministic, pinned
    /// block times via a `MockClock` without depending on the wall clock. Call
    /// [`Vm::initialize`] before use.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: None,
            ctx: None,
            state: EngineState::Initializing,
            preferred: Id::EMPTY,
            genesis_id: Id::EMPTY,
            mempool: Mutex::new(Mempool::new()),
            clock,
        }
    }

    /// The chain's [`ValidatorState`](ava_validators::state::ValidatorState)
    /// (the M4.21 manager), exposed to the snow context / proposervm windower /
    /// Warp signer (Go `vm.State`). `None` before [`initialize`](Vm::initialize).
    #[must_use]
    pub fn validator_state(&self) -> Option<Arc<PChainValidatorManager<DynDb>>> {
        self.shared.as_ref().map(|s| Arc::clone(&s.validators))
    }

    /// The shared core, or [`Error::NotInitialized`] if `initialize` has not run.
    fn shared(&self) -> Result<&Arc<Shared<DynDb>>> {
        self.shared.as_ref().ok_or(Error::NotInitialized)
    }

    /// **Test helper** — run `read` against the persisted [`State`] read surface.
    ///
    /// The reexecute harness (M9.19 `replay_pchain`) computes a deterministic
    /// post-state digest over the live state after replaying a synthetic case; the
    /// [`Chain`] trait exposes per-id/address reads but the [`State`] is held behind
    /// the private [`Shared`], so the harness reads it back via this read-only seam
    /// (the P-Chain mirror of `ava_avm::vm::AvmVm::with_state`). Acquires the block
    /// manager lock for the duration of `read`.
    ///
    /// # Errors
    /// Returns [`Error::NotInitialized`] before [`initialize`](Vm::initialize).
    #[doc(hidden)]
    pub fn with_state<R>(&self, read: impl FnOnce(&State<DynDb>) -> R) -> Result<R> {
        let shared = self.shared()?;
        let mgr = shared.manager.lock();
        Ok(read(mgr.state()))
    }

    /// **Test helper** — admit `tx` to the (un-shared) decision-tx mempool.
    ///
    /// Production admission flows through the gossip handler
    /// ([`crate::network::TxGossipHandler`]) or the not-yet-wired `issueTx` RPC
    /// (see [`DeferredIssuer`]); this is the direct seam the reexecute harness
    /// (M9.19 `replay_pchain`) uses to drive a funded, signed `CreateSubnetTx`
    /// into a height-1 standard block. The P-Chain mirror of
    /// [`ava_avm::vm::AvmVm::mempool_add`] — but the mempool is a field on
    /// [`PlatformVm`] itself (not in [`Shared`]), so this locks `self.mempool`.
    ///
    /// # Errors
    /// Maps a mempool rejection (duplicate / full / conflict) to a descriptive
    /// [`Error::Service`] — callers in tests treat any error as fatal.
    #[doc(hidden)]
    pub fn mempool_add(&self, tx: crate::txs::Tx) -> Result<()> {
        self.mempool
            .lock()
            .add(tx)
            .map_err(|e| Error::Service(format!("mempool add: {e}")))
    }

    /// Builds the executor [`Backend`] from the chain context (read-only-sync
    /// subset; the full per-network staking/fee config is M8/`ava-genesis`).
    ///
    /// The executor `Fx` reads time through the SAME injected `clock` as
    /// `build_block` (specs 24 hazard #5), so locktime/credential checks and the
    /// proposed block time observe one clock.
    fn backend(ctx: &ChainContext, clock: &Arc<dyn Clock>) -> Backend {
        Backend {
            upgrades: upgrade_schedule(ctx),
            staking: StakingConfig::mainnet(),
            static_fee_config: StaticFeeConfig::MAINNET,
            network_id: ctx.network_id,
            chain_id: ctx.chain_id,
            avax_asset_id: ctx.avax_asset_id,
            node_id: ctx.node_id,
            fx: ava_secp256k1fx::Fx::new(Arc::clone(clock)),
            // Set true once the engine transitions us to NormalOp; during
            // bootstrap the heavier semantic checks are skipped (Go `Bootstrapped`).
            bootstrapped: false,
        }
    }

    /// Wraps a P-Chain [`Block`] as the engine-facing [`VmBlock`].
    ///
    /// The P-Chain [`Block`] is `!Sync` (its `Tx` carries a `!Sync` `OnceCell`),
    /// so the wrapper cannot hold it across an `await`. Instead it stores the
    /// `Send + Sync` projection (id/parent/height/timestamp/bytes) and re-parses
    /// the block from `bytes` on demand inside the (locked) decision methods.
    fn wrap(&self, block: &Block) -> Result<Arc<dyn VmBlock>> {
        let shared = self.shared()?;
        let timestamp_secs = block.banff_timestamp();
        Ok(Arc::new(PChainBlock {
            id: block.id(),
            parent: block.parent_id(),
            height: block.height(),
            timestamp_secs,
            bytes: block.bytes().to_vec(),
            shared: Arc::clone(shared),
        }))
    }
}

/// Builds the executor [`UpgradeSchedule`] from the chain context's upgrade
/// config (Durango / Etna activation times → [`SystemTime`]).
fn upgrade_schedule(ctx: &ChainContext) -> UpgradeSchedule {
    UpgradeSchedule {
        durango_time: datetime_to_system(ctx.network_upgrades.durango_time),
        etna_time: datetime_to_system(ctx.network_upgrades.etna_time),
    }
}

/// `chrono::DateTime<Utc>` → [`SystemTime`] (saturating; the epoch for pre-epoch
/// / out-of-range times).
fn datetime_to_system(dt: chrono::DateTime<chrono::Utc>) -> SystemTime {
    let secs = dt.timestamp();
    u64::try_from(secs)
        .ok()
        .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)))
        .unwrap_or(UNIX_EPOCH)
}

// ---------------------------------------------------------------------------
// The engine-facing block wrapper.
// ---------------------------------------------------------------------------

/// A P-Chain block presented to the consensus engine: a `Send + Sync` projection
/// of a [`Block`] that drives the shared [`BlockManager`] on
/// `verify`/`accept`/`reject` by re-parsing its bytes (Go the per-block
/// `Verify`/`Accept`/`Reject` delegating to `block/executor`).
struct PChainBlock {
    id: Id,
    parent: Id,
    height: u64,
    /// The Banff timestamp (seconds), or `None` for an Apricot block (whose
    /// timestamp is the parent's resolved chain time).
    timestamp_secs: Option<u64>,
    bytes: Vec<u8>,
    shared: Arc<Shared<DynDb>>,
}

impl PChainBlock {
    /// Re-parses the full P-Chain [`Block`] from the stored bytes (the `!Sync`
    /// block cannot be held across an `await`).
    fn parse(&self) -> ava_snow::Result<Block> {
        Block::parse(crate::txs::Codec(), &self.bytes)
            .map_err(|e| ava_snow::Error::from(Error::from(e)))
    }
}

#[async_trait]
impl VmBlock for PChainBlock {
    fn id(&self) -> Id {
        self.id
    }

    fn parent(&self) -> Id {
        self.parent
    }

    fn height(&self) -> u64 {
        self.height
    }

    fn timestamp(&self) -> SystemTime {
        // Banff blocks carry their own timestamp; Apricot blocks inherit the
        // parent's chain time (resolved by the manager).
        let secs = match self.timestamp_secs {
            Some(secs) => secs,
            None => {
                let mgr = self.shared.manager.lock();
                mgr.timestamp(self.parent)
            }
        };
        UNIX_EPOCH
            .checked_add(Duration::from_secs(secs))
            .unwrap_or(UNIX_EPOCH)
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    async fn verify(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        let block = self.parse()?;
        let mut mgr = self.shared.manager.lock();
        mgr.verify(&block).map_err(ava_snow::Error::from)
    }

    async fn accept(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        let block = self.parse()?;
        {
            let mut mgr = self.shared.manager.lock();
            mgr.accept(&block).map_err(ava_snow::Error::from)?;
        }
        // Re-capture the validator manager from the flushed state (the M4.21
        // production wiring point). The recently-accepted window is updated
        // inside `accept` via the injected notifier.
        self.shared.refresh_validators();
        Ok(())
    }

    async fn reject(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        let block = self.parse()?;
        let mut mgr = self.shared.manager.lock();
        mgr.reject(&block);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Vm supertraits (app / health / connector) — no P-Chain behaviour here yet.
// ---------------------------------------------------------------------------

#[async_trait]
impl AppHandler for PlatformVm {
    async fn app_request(
        &mut self,
        _token: &CancellationToken,
        _node: ava_types::node_id::NodeId,
        _request_id: u32,
        _deadline: std::time::Instant,
        _request: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_request_failed(
        &mut self,
        _token: &CancellationToken,
        _node: ava_types::node_id::NodeId,
        _request_id: u32,
        _err: AppError,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_response(
        &mut self,
        _token: &CancellationToken,
        _node: ava_types::node_id::NodeId,
        _request_id: u32,
        _response: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_gossip(
        &mut self,
        _token: &CancellationToken,
        _node: ava_types::node_id::NodeId,
        _msg: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }
}

#[async_trait]
impl HealthCheck for PlatformVm {
    async fn health_check(&self, _token: &CancellationToken) -> VmResult<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl Connector for PlatformVm {
    async fn connected(
        &mut self,
        _token: &CancellationToken,
        _node: ava_types::node_id::NodeId,
        _version: ava_version::application::Application,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn disconnected(
        &mut self,
        _token: &CancellationToken,
        _node: ava_types::node_id::NodeId,
    ) -> VmResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Vm.
// ---------------------------------------------------------------------------

#[async_trait]
impl Vm for PlatformVm {
    async fn initialize(
        &mut self,
        _token: &CancellationToken,
        chain_ctx: Arc<ChainContext>,
        db: Arc<dyn DynDatabase>,
        genesis_bytes: &[u8],
        _upgrade_bytes: &[u8],
        _config_bytes: &[u8],
        _fxs: Vec<Fx>,
        _app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        // Open State over the engine-provided DB (adapted to the typed surface).
        let mut state = State::new(DynDb::new(db)).map_err(VmError::from)?;

        // Seed genesis (M4.24): timestamp, supply, UTXOs, current validators,
        // chains — and derive the genesis ApricotCommit block id (height 0).
        let genesis = crate::genesis::parse(genesis_bytes).map_err(VmError::from)?;
        let genesis_id = crate::genesis::seed_state(&mut state, &genesis, genesis_bytes)
            .map_err(VmError::from)?;

        // Record the genesis block as last-accepted at height 0 WITHOUT Accept
        // (the BlockManager seeds its last-accepted from `state.last_accepted()`).
        let genesis_block = crate::genesis::genesis_block(genesis_bytes).map_err(VmError::from)?;
        state.add_block(genesis_id, 0, genesis_block.bytes());
        state.set_last_accepted(genesis_id);
        state.set_height(0);

        // Build the M4.21 validator manager from the seeded state, then the
        // M4.20 block manager with the validator manager wired as the acceptance
        // notifier (so the recently-accepted window updates inside `accept`).
        let validators = Arc::new(PChainValidatorManager::from_state(&state, false));
        let codec = codec::codec().map_err(|e| VmError::from(Error::from(e)))?;
        let manager = BlockManager::new(
            state,
            Self::backend(&chain_ctx, &self.clock),
            codec,
            Arc::clone(&validators) as Arc<dyn crate::block::executor::BlockAcceptanceNotifier>,
        );

        self.shared = Some(Arc::new(Shared {
            manager: Mutex::new(manager),
            validators,
        }));
        self.ctx = Some(chain_ctx);
        self.genesis_id = genesis_id;
        self.preferred = genesis_id;
        self.state = EngineState::Initializing;
        Ok(())
    }

    async fn set_state(&mut self, _token: &CancellationToken, state: EngineState) -> VmResult<()> {
        // Bootstrapping → NormalOp is the only transition that changes VM
        // behaviour here (Go `onNormalOperationsStarted` flips `Bootstrapped`).
        // The accept path's bootstrapping-vs-normal distinction (verify+accept
        // vs accept-non-verifying) is the engine's choice via the block methods;
        // the VM only records the phase for now.
        self.state = state;
        Ok(())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> VmResult<()> {
        Ok(())
    }

    async fn version(&self, _token: &CancellationToken) -> VmResult<String> {
        Ok("platformvm/0.0.0".to_string())
    }

    /// Go `CreateHandlers` (`vms/platformvm/vm.go:451-466`): one gorilla
    /// JSON-RPC server at extension `""` with the service registered as
    /// `"platform"` (the `vm.metrics` request-interceptor wrap is a recorded
    /// deferral, consistent with the proposervm M8.22 precedent). The bridged
    /// method set vs Go's 31 is inventoried in `tests/PORTING.md` (M8.23 owns
    /// full parity).
    async fn create_handlers(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<HashMap<String, HttpHandler>> {
        let shared = self.shared().map_err(VmError::from)?;
        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| VmError::from(Error::NotInitialized))?;
        let service = Arc::new(crate::service::Service::new(
            Arc::new(VmServiceState {
                shared: Arc::clone(shared),
            }),
            Arc::clone(&shared.validators) as Arc<dyn ValidatorState>,
            ctx.network_id,
            ctx.avax_asset_id,
        ));
        // The `issueTx` mempool-admission seam. The P-Chain mempool currently
        // lives un-shared on `PlatformVm` (not in `Shared`), so it cannot be
        // reached from the per-request handler; admission therefore surfaces a
        // clear deferral (the shared-mempool + gossip wiring is the M8 node
        // assembly concern — see `tests/PORTING.md`). Decode/parse + the wire
        // contract are fully exercised before this point.
        let issuer: Arc<dyn crate::service::TxIssuer> = Arc::new(DeferredIssuer);
        let registry = Arc::new(crate::service::registry(service, ctx.avax_asset_id, issuer));
        let mut handlers = HashMap::new();
        handlers.insert(
            String::new(),
            // Go's modern CreateHandlers map carries no lock semantics; the
            // service locks the block manager per read (VmServiceState).
            HttpHandler::in_process(LockOptions::NoLock, registry_service(registry)),
        );
        Ok(handlers)
    }

    async fn new_http_handler(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        Ok(None)
    }

    async fn wait_for_event(&self, _token: &CancellationToken) -> VmResult<VmEvent> {
        // Read-only sync issues no txs, so the mempool stays empty (the p2p
        // gossip transport that would fill it is the deferred seam).
        let pending = !self.mempool.lock().is_empty();
        if pending {
            Ok(VmEvent::PendingTxs)
        } else {
            // Block until cancellation (no event to report). The engine cancels
            // the token on shutdown.
            _token.cancelled().await;
            Ok(VmEvent::PendingTxs)
        }
    }
}

// ---------------------------------------------------------------------------
// ChainVm.
// ---------------------------------------------------------------------------

#[async_trait]
impl ChainVm for PlatformVm {
    async fn build_block(&mut self, _token: &CancellationToken) -> VmResult<Arc<dyn VmBlock>> {
        let block = {
            let shared = self.shared().map_err(VmError::from)?;
            let mgr = shared.manager.lock();

            // Resolve the preferred block's parent state view + height.
            let parent_id = self.preferred;
            let parent_state = mgr
                .get_state_for_verify(parent_id)
                .ok_or(VmError::NotFound)?;
            let parent_height = mgr.parent_height(parent_id).map_err(VmError::from)?;
            let height = parent_height.saturating_add(1);

            // Resolve the new block time: min(max(now, parent_ts), next staker
            // change), clamped by SyncBound. `now` reads the injected clock —
            // NOT the wall clock directly (specs 24 hazard #5).
            let now = self.clock.now();
            let parent_ts = parent_state.timestamp();
            let next_change = next_staker_change_time(parent_state.as_ref());
            let (timestamp, time_was_capped) =
                builder::next_block_time(now, parent_ts, next_change, SYNC_BOUND);

            // Decision txs from the mempool, in FIFO order (empty in read-only
            // sync). The builder caps them by size; accepted txs are removed on
            // accept (a follow-up wires the accept-side drain).
            let decision_txs = self.mempool.lock().snapshot();

            builder::build_block(
                crate::txs::Codec(),
                parent_id,
                height,
                timestamp,
                time_was_capped,
                parent_state.as_ref(),
                decision_txs,
            )
            .map_err(VmError::from)?
        };
        self.wrap(&block).map_err(VmError::from)
    }

    async fn get_block(&self, _token: &CancellationToken, id: Id) -> VmResult<Arc<dyn VmBlock>> {
        let shared = self.shared().map_err(VmError::from)?;
        let bytes = {
            let mgr = shared.manager.lock();
            mgr.state().get_block(id).map_err(VmError::from)?
        };
        let block =
            Block::parse(crate::txs::Codec(), &bytes).map_err(|e| VmError::from(Error::from(e)))?;
        self.wrap(&block).map_err(VmError::from)
    }

    async fn parse_block(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn VmBlock>> {
        self.shared().map_err(VmError::from)?;
        let block =
            Block::parse(crate::txs::Codec(), bytes).map_err(|e| VmError::from(Error::from(e)))?;
        self.wrap(&block).map_err(VmError::from)
    }

    async fn set_preference(&mut self, _token: &CancellationToken, id: Id) -> VmResult<()> {
        self.preferred = id;
        Ok(())
    }

    async fn last_accepted(&self, _token: &CancellationToken) -> VmResult<Id> {
        let shared = self.shared().map_err(VmError::from)?;
        let mgr = shared.manager.lock();
        Ok(mgr.last_accepted())
    }

    async fn get_block_id_at_height(
        &self,
        _token: &CancellationToken,
        height: u64,
    ) -> VmResult<Id> {
        let shared = self.shared().map_err(VmError::from)?;
        let mgr = shared.manager.lock();
        mgr.state()
            .get_block_id_at_height(height)
            .ok_or(VmError::NotFound)
    }

    /// P-Chain has NO state sync (19 §5): bootstraps linearly only.
    fn as_state_syncable(&self) -> Option<&dyn StateSyncableVm> {
        None
    }

    /// The P-Chain implements [`BatchedChainVm`] so the bootstrapper (M4.27) can
    /// bulk-fetch ancestry via `GetAncestors` and bulk-parse a peer's `Ancestors`
    /// reply (Go `vms/platformvm/vm.go` embeds `block.BatchedChainVM`).
    fn as_batched(&self) -> Option<&dyn BatchedChainVm> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// BatchedChainVm (the linear-bootstrap fetch/parse capability, M4.27).
// ---------------------------------------------------------------------------

#[async_trait]
impl BatchedChainVm for PlatformVm {
    /// `GetAncestors` — return the byte representations of `[blk_id]` and its
    /// ancestors over the accepted block store, newest first, bounded by
    /// `max_blocks_num` / `max_blocks_size` / `max_retrieval_time` (Go
    /// `vms/platformvm/vm.go::GetAncestors`, the byte-accounting mirrors the
    /// engine fallback in `ava_vm::block::batched`).
    async fn get_ancestors(
        &self,
        _token: &CancellationToken,
        blk_id: Id,
        max_blocks_num: usize,
        max_blocks_size: usize,
        max_retrieval_time: Duration,
    ) -> VmResult<Vec<Vec<u8>>> {
        let shared = self.shared().map_err(VmError::from)?;
        let start = std::time::Instant::now();
        let mgr = shared.manager.lock();
        let state = mgr.state();

        // Fetch the requested block; a missing block yields an empty response
        // (signals the peer to stop asking this node).
        let first = match state.get_block(blk_id) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(Vec::new()),
        };

        let mut ancestors: Vec<Vec<u8>> = Vec::with_capacity(max_blocks_num.min(1024));
        let mut total_len = first.len().saturating_add(INT_LEN);
        let mut current =
            Block::parse(crate::txs::Codec(), &first).map_err(|e| VmError::from(Error::from(e)))?;
        ancestors.push(first);

        let mut num_fetched = 1usize;
        while num_fetched < max_blocks_num && start.elapsed() < max_retrieval_time {
            let parent_id = current.parent_id();
            let parent_bytes = match state.get_block(parent_id) {
                Ok(bytes) => bytes,
                // Missing parent stops the walk (e.g. below the local root).
                Err(_) => break,
            };
            let new_len = total_len
                .saturating_add(parent_bytes.len())
                .saturating_add(INT_LEN);
            if new_len > max_blocks_size {
                break;
            }
            current = Block::parse(crate::txs::Codec(), &parent_bytes)
                .map_err(|e| VmError::from(Error::from(e)))?;
            ancestors.push(parent_bytes);
            total_len = new_len;
            num_fetched = num_fetched.saturating_add(1);
        }

        Ok(ancestors)
    }

    /// `BatchedParseBlock` — parse a batch of block byte representations into the
    /// engine-facing [`VmBlock`]s (Go `vms/platformvm/vm.go::BatchedParseBlock`).
    async fn batched_parse_block(
        &self,
        _token: &CancellationToken,
        blks: &[Vec<u8>],
    ) -> VmResult<Vec<Arc<dyn VmBlock>>> {
        self.shared().map_err(VmError::from)?;
        let mut blocks: Vec<Arc<dyn VmBlock>> = Vec::with_capacity(blks.len());
        for bytes in blks {
            let block = Block::parse(crate::txs::Codec(), bytes)
                .map_err(|e| VmError::from(Error::from(e)))?;
            blocks.push(self.wrap(&block).map_err(VmError::from)?);
        }
        Ok(blocks)
    }
}

/// The next staker change time (Go `state.GetNextStakerChangeTime`): the
/// earliest `next_time` across the current + pending stakers, or [`SystemTime`]'s
/// far future when there are none (no change pending).
fn next_staker_change_time(parent_state: &dyn Chain) -> SystemTime {
    let far = UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::from(u32::MAX).saturating_mul(64)))
        .unwrap_or(UNIX_EPOCH);
    // Both staker iterators are in `(next_time, …)` order, so the head of each
    // is its minimum next-change time; the overall next change is the earlier of
    // the two heads (or `far` when both sets are empty).
    let mut earliest = far;
    if let Some(s) = parent_state.current_stakers().first() {
        earliest = earliest.min(s.next_time);
    }
    if let Some(s) = parent_state.pending_stakers().first() {
        earliest = earliest.min(s.next_time);
    }
    earliest
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod conformance {
    //! `conformance::vm_initialize_and_last_accepted` (M4.25 TDD entry point):
    //! `initialize` from genesis sets `last_accepted == genesis_id`,
    //! `get_block(genesis_id)` returns the ApricotCommit block, `parse_block` /
    //! `build_block` round-trip, and the VM does NOT implement `StateSyncableVm`
    //! (19 §5).

    use std::collections::HashSet;
    use std::sync::Arc;

    use async_trait::async_trait;
    use ava_database::{DynDatabase, MemDb};
    use ava_snow::ChainContext;
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_vm::app_sender::{AppSender, SendConfig};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::block::BlockBody;

    /// A no-op [`AppSender`] for the `initialize` call.
    #[derive(Debug, Default)]
    struct NoopAppSender;

    #[async_trait]
    impl AppSender for NoopAppSender {
        async fn send_app_request(
            &self,
            _token: &CancellationToken,
            _nodes: &HashSet<NodeId>,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_response(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_error(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _code: i32,
            _message: &str,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_gossip(
            &self,
            _token: &CancellationToken,
            _config: SendConfig,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
    }

    fn chain_ctx() -> Arc<ChainContext> {
        Arc::new(ChainContext {
            network_id: 1,
            subnet_id: Id::EMPTY,
            chain_id: Id::EMPTY,
            node_id: NodeId::default(),
            public_key: None,
            network_upgrades: ava_version::upgrade::get_config(1),
            x_chain_id: Id::EMPTY,
            c_chain_id: Id::EMPTY,
            avax_asset_id: Id::EMPTY,
            chain_data_dir: std::path::PathBuf::new(),
        })
    }

    async fn init_vm() -> (PlatformVm, Id, Vec<u8>) {
        let genesis = crate::genesis::test_synthetic_genesis();
        let genesis_bytes = crate::genesis::marshal(&genesis).expect("marshal genesis");
        let expected_genesis_id = crate::genesis::genesis_block(&genesis_bytes)
            .expect("genesis block")
            .id();

        let mut vm = PlatformVm::new();
        let token = CancellationToken::new();
        let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        vm.initialize(
            &token,
            chain_ctx(),
            db,
            &genesis_bytes,
            b"",
            b"",
            Vec::new(),
            Arc::new(NoopAppSender),
        )
        .await
        .expect("initialize");
        (vm, expected_genesis_id, genesis_bytes)
    }

    #[tokio::test]
    async fn vm_initialize_and_last_accepted() {
        let token = CancellationToken::new();
        let (vm, genesis_id, _genesis_bytes) = init_vm().await;

        // last_accepted == genesis_id.
        let last = vm.last_accepted(&token).await.expect("last accepted");
        assert_eq!(last, genesis_id, "last_accepted should be the genesis id");

        // get_block(genesis_id) returns the genesis ApricotCommit block (height 0).
        let blk = vm.get_block(&token, genesis_id).await.expect("get genesis");
        assert_eq!(blk.id(), genesis_id);
        assert_eq!(blk.height(), 0);
        // Down-cast to inspect the concrete body is not needed: re-parse the
        // bytes and assert the variant.
        let reparsed = Block::parse(crate::txs::Codec(), blk.bytes()).expect("reparse");
        assert!(
            matches!(reparsed.body(), BlockBody::ApricotCommit(_)),
            "genesis block must be an ApricotCommit block"
        );

        // get_block_id_at_height(0) == genesis_id.
        let at0 = vm
            .get_block_id_at_height(&token, 0)
            .await
            .expect("height 0");
        assert_eq!(at0, genesis_id);

        // parse_block round-trips the genesis bytes to the same id.
        let parsed = vm.parse_block(&token, blk.bytes()).await.expect("parse");
        assert_eq!(parsed.id(), genesis_id);
        assert_eq!(parsed.bytes(), blk.bytes());

        // build_block over genesis: the genesis chain time is `5` (the synthetic
        // genesis timestamp), `now` is well past it, and the single genesis
        // staker's end time is far in the future, so the builder advances the
        // time → a BanffStandardBlock at height 1 (force_advance_time = true via
        // the staker-change cap). It must round-trip through parse_block.
        let mut vm = vm;
        let built = vm.build_block(&token).await.expect("build block");
        assert_eq!(built.parent(), genesis_id);
        assert_eq!(built.height(), 1);
        let round = vm
            .parse_block(&token, built.bytes())
            .await
            .expect("round-trip built block");
        assert_eq!(round.id(), built.id());
        assert_eq!(round.bytes(), built.bytes());

        // NO state sync (19 §5): the VM does not implement StateSyncableVm.
        assert!(
            ChainVm::as_state_syncable(&vm).is_none(),
            "P-Chain must NOT implement StateSyncableVm"
        );

        // The ValidatorState is exposed to the snow context.
        assert!(vm.validator_state().is_some());
    }

    /// M8.22 end-to-end: `create_handlers` mounts the gorilla `platform.*`
    /// service at extension `""` (Go `vm.go:463-465`) and serves
    /// `platform.getHeight` / `platform.getTimestamp` through the in-process
    /// `HttpHandler` seam over the live (genesis) state.
    #[tokio::test]
    #[allow(clippy::indexing_slicing)] // Value indexing yields Null, not a panic
    async fn create_handlers_serves_platform_service() {
        use ava_vm::vm::VmRequest;

        let token = CancellationToken::new();
        let (mut vm, ..) = init_vm().await;
        let handlers = vm.create_handlers(&token).await.expect("create_handlers");
        assert_eq!(handlers.len(), 1, "exactly the Go extension set");
        let handler = handlers.get("").expect("root extension (Go key \"\")");
        let service = handler
            .service
            .as_ref()
            .expect("in-process VmHttpService handler");

        let post = |body: serde_json::Value| {
            let service = Arc::clone(service);
            async move {
                let resp = service
                    .serve_http(VmRequest {
                        method: "POST".to_string(),
                        uri: String::new(),
                        headers: vec![("content-type".to_string(), "application/json".to_string())],
                        body: serde_json::to_vec(&body).expect("serialize"),
                    })
                    .await;
                assert_eq!(resp.status, 200, "JSON-RPC always answers HTTP 200");
                serde_json::from_slice::<serde_json::Value>(&resp.body).expect("json body")
            }
        };

        // getHeight over the genesis state: height 0, json.Uint64 string.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "platform.getHeight",
            "params": [{}],
            "id": 1,
        }))
        .await;
        assert_eq!(
            body["result"]["height"], "0",
            "platform.getHeight serves the genesis height"
        );

        // getTimestamp reads the live chain time (the synthetic genesis time).
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "platform.getTimestamp",
            "params": [{}],
            "id": 2,
        }))
        .await;
        assert!(
            body["result"]["timestamp"].is_string(),
            "platform.getTimestamp serves an RFC3339 timestamp: {body}"
        );

        // issueTx is now bridged (M8.23a): an empty payload fails to decode and
        // surfaces a -32000 handler error (not -32601 method-not-found).
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "platform.issueTx",
            "params": [{}],
            "id": 3,
        }))
        .await;
        assert_eq!(
            body["error"]["code"], -32000,
            "bridged issueTx surfaces a -32000 handler error on a bad payload"
        );

        // A genuinely-unknown method still dispatches to -32601.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "platform.notAMethod",
            "params": [{}],
            "id": 4,
        }))
        .await;
        assert_eq!(body["error"]["code"], -32601, "unknown method is -32601");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod clock_injection {
    //! `clock_injection::build_block_reads_injected_clock` (specs 24 hazard #5):
    //! a `PlatformVm` constructed via [`PlatformVm::with_clock`] with a
    //! `MockClock` pinned to a fixed instant builds a height-1 block whose
    //! timestamp equals the pinned clock time (clamped by `next_block_time`),
    //! proving `build_block` reads the injected clock — NOT the wall clock and
    //! NOT the parent timestamp. The clock is pinned strictly past the genesis
    //! time (5) and well before the staker-change cap, so the resolved time is
    //! exactly the pinned `now`.

    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use async_trait::async_trait;
    use ava_database::{DynDatabase, MemDb};
    use ava_snow::ChainContext;
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_utils::clock::MockClock;
    use ava_vm::app_sender::{AppSender, SendConfig};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::txs::base_tx::BaseTx;
    use crate::txs::components::{BaseTx as AvaxBaseTx, Owner};
    use crate::txs::{Codec, CreateSubnetTx, Tx, UnsignedTx};

    /// A no-op [`AppSender`] for the `initialize` call.
    #[derive(Debug, Default)]
    struct NoopAppSender;

    #[async_trait]
    impl AppSender for NoopAppSender {
        async fn send_app_request(
            &self,
            _token: &CancellationToken,
            _nodes: &HashSet<NodeId>,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_response(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_error(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _code: i32,
            _message: &str,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_gossip(
            &self,
            _token: &CancellationToken,
            _config: SendConfig,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
    }

    fn chain_ctx() -> Arc<ChainContext> {
        Arc::new(ChainContext {
            network_id: 1,
            subnet_id: Id::EMPTY,
            chain_id: Id::EMPTY,
            node_id: NodeId::default(),
            public_key: None,
            network_upgrades: ava_version::upgrade::get_config(1),
            x_chain_id: Id::EMPTY,
            c_chain_id: Id::EMPTY,
            avax_asset_id: Id::EMPTY,
            chain_data_dir: std::path::PathBuf::new(),
        })
    }

    #[tokio::test]
    async fn build_block_reads_injected_clock() {
        // Pin the clock to unix 1_000: strictly past the genesis/parent time (5)
        // and far below the genesis staker's end time (≈31.5M, the next-change
        // cap), so `next_block_time` resolves the block time to exactly `now`.
        const PINNED: u64 = 1_000;
        let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(PINNED));

        let genesis = crate::genesis::test_synthetic_genesis();
        let genesis_bytes = crate::genesis::marshal(&genesis).expect("marshal genesis");

        let mut vm = PlatformVm::with_clock(Arc::new(clock));
        let token = CancellationToken::new();
        let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        vm.initialize(
            &token,
            chain_ctx(),
            db,
            &genesis_bytes,
            b"",
            b"",
            Vec::new(),
            Arc::new(NoopAppSender),
        )
        .await
        .expect("initialize");

        // Admit a minimal (well-formed) decision tx so the builder packs a
        // standard block at `now`. The pinned clock (1_000) is below the genesis
        // staker-change cap (≈31.5M), so `next_block_time` does NOT force-advance
        // the time; without a tx the builder would decline (`NoPendingBlocks`).
        // build_block only PACKS the tx (verification is a later step), so an
        // unfunded CreateSubnetTx is sufficient to drive the build path here.
        let mut tx = Tx::new(UnsignedTx::CreateSubnet(CreateSubnetTx {
            base: BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![],
                memo: vec![],
            }),
            owner: Owner::default(),
        }));
        tx.initialize(Codec()).expect("initialize create-subnet tx");
        vm.mempool_add(tx).expect("mempool add");

        // Build over genesis with the admitted tx (a standard block stamping the
        // chain time). The built block's timestamp must equal the pinned clock.
        let built = vm.build_block(&token).await.expect("build block");
        assert_eq!(built.height(), 1, "build advances to height 1");

        let ts = built
            .timestamp()
            .duration_since(UNIX_EPOCH)
            .expect("post-epoch timestamp")
            .as_secs();
        assert_eq!(
            ts, PINNED,
            "build_block must stamp the INJECTED clock time, not the wall clock or parent ts (5)"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod differential {
    //! `differential::pchain_sync_to_tip` (M4.27 — TDD ENTRY POINT #2, height-0
    //! subset): drive the M3 Snowman [`Bootstrapper`] end-to-end against a
    //! recorded single-block frontier (= genesis), proving the linear-bootstrap
    //! fetch→execute-forward loop accepts the genesis block and stops at height 0
    //! (specs 19 §1–§2, §5; 08 §4.2).
    //!
    //! The height-0 case is special and simple: the recorded frontier IS the
    //! genesis block, which the VM already holds as last-accepted after
    //! `initialize`. The bootstrapper fetches the frontier via `GetAncestors`
    //! (answered by the M4.27 [`BatchedChainVm`] impl over the block store),
    //! recognizes it is at the local last-accepted height (the interval tree's
    //! `add_block` declines it), executes the empty range, and hands off to
    //! NormalOp. `last_accepted` remains the genesis id throughout.
    //!
    //! The full multi-block Fuji sync (chasing the tip) is M4.29; the recorded
    //! Go state-hash oracle at height 0 is deferred (see `tests/PORTING.md`).

    use std::collections::{BTreeMap, HashSet};
    use std::sync::Arc;

    use async_trait::async_trait;
    use ava_database::{DynDatabase, MemDb};
    use ava_snow::acceptor::NoOpAcceptor;
    use ava_snow::{ChainContext, ConsensusContext};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_vm::app_sender::{AppSender, SendConfig};
    use tokio::sync::Mutex as AsyncMutex;
    use tokio_util::sync::CancellationToken;

    use ava_engine::common::sender::{SendConfig as EngineSendConfig, Sender};
    use ava_engine::snowman::bootstrap::{Bootstrapper, Config, Phase};

    use super::*;

    /// A no-op [`AppSender`] for the `initialize` call.
    #[derive(Debug, Default)]
    struct NoopAppSender;

    #[async_trait]
    impl AppSender for NoopAppSender {
        async fn send_app_request(
            &self,
            _token: &CancellationToken,
            _nodes: &HashSet<NodeId>,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_response(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_error(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _code: i32,
            _message: &str,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
        async fn send_app_gossip(
            &self,
            _token: &CancellationToken,
            _config: SendConfig,
            _bytes: Vec<u8>,
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
    }

    /// A minimal [`Sender`] that records the outbound `GetAncestors` so the test
    /// can answer it; every other bootstrap send is a no-op (height-0 needs only
    /// the fetch round-trip).
    #[derive(Default)]
    struct FetchSender {
        get_ancestors: parking_lot::Mutex<Vec<(NodeId, u32, Id)>>,
    }

    impl FetchSender {
        fn take_get_ancestors(&self) -> Vec<(NodeId, u32, Id)> {
            std::mem::take(&mut self.get_ancestors.lock())
        }
    }

    #[async_trait]
    impl Sender for FetchSender {
        fn send_get_state_summary_frontier(&self, _nodes: &HashSet<NodeId>, _req: u32) {}
        fn send_state_summary_frontier(&self, _node: NodeId, _req: u32, _summary: Vec<u8>) {}
        fn send_get_accepted_state_summary(
            &self,
            _nodes: &HashSet<NodeId>,
            _req: u32,
            _heights: &[u64],
        ) {
        }
        fn send_accepted_state_summary(&self, _node: NodeId, _req: u32, _summary_ids: &[Id]) {}
        fn send_get_accepted_frontier(&self, _nodes: &HashSet<NodeId>, _req: u32) {}
        fn send_accepted_frontier(&self, _node: NodeId, _req: u32, _container_id: Id) {}
        fn send_get_accepted(&self, _nodes: &HashSet<NodeId>, _req: u32, _ids: &[Id]) {}
        fn send_accepted(&self, _node: NodeId, _req: u32, _ids: &[Id]) {}
        fn send_get(&self, _node: NodeId, _req: u32, _container_id: Id) {}
        fn send_get_ancestors(&self, node: NodeId, req: u32, container_id: Id) {
            self.get_ancestors.lock().push((node, req, container_id));
        }
        fn send_put(&self, _node: NodeId, _req: u32, _container: Vec<u8>) {}
        fn send_ancestors(&self, _node: NodeId, _req: u32, _containers: Vec<Vec<u8>>) {}
        fn send_push_query(
            &self,
            _nodes: &HashSet<NodeId>,
            _req: u32,
            _container: Vec<u8>,
            _requested_height: u64,
        ) {
        }
        fn send_pull_query(
            &self,
            _nodes: &HashSet<NodeId>,
            _req: u32,
            _container_id: Id,
            _requested_height: u64,
        ) {
        }
        fn send_chits(
            &self,
            _node: NodeId,
            _req: u32,
            _preferred: Id,
            _preferred_at_height: Id,
            _accepted: Id,
            _accepted_height: u64,
        ) {
        }
        async fn send_app_request(
            &self,
            _nodes: &HashSet<NodeId>,
            _req: u32,
            _bytes: Vec<u8>,
        ) -> ava_engine::error::Result<()> {
            Ok(())
        }
        async fn send_app_response(
            &self,
            _node: NodeId,
            _req: u32,
            _bytes: Vec<u8>,
        ) -> ava_engine::error::Result<()> {
            Ok(())
        }
        async fn send_app_error(
            &self,
            _node: NodeId,
            _req: u32,
            _code: i32,
            _msg: &str,
        ) -> ava_engine::error::Result<()> {
            Ok(())
        }
        async fn send_app_gossip(
            &self,
            _cfg: EngineSendConfig,
            _bytes: Vec<u8>,
        ) -> ava_engine::error::Result<()> {
            Ok(())
        }
    }

    fn chain_ctx() -> Arc<ChainContext> {
        Arc::new(ChainContext {
            network_id: 1,
            subnet_id: Id::EMPTY,
            chain_id: Id::EMPTY,
            node_id: NodeId::default(),
            public_key: None,
            network_upgrades: ava_version::upgrade::get_config(1),
            x_chain_id: Id::EMPTY,
            c_chain_id: Id::EMPTY,
            avax_asset_id: Id::EMPTY,
            chain_data_dir: std::path::PathBuf::new(),
        })
    }

    /// Initializes a `PlatformVm` from the synthetic genesis, returning the VM,
    /// the genesis block id, and the genesis block bytes (what a peer would serve
    /// as the recorded height-0 frontier).
    async fn init_vm() -> (PlatformVm, Id, Vec<u8>) {
        let genesis = crate::genesis::test_synthetic_genesis();
        let genesis_bytes = crate::genesis::marshal(&genesis).expect("marshal genesis");
        let genesis_block = crate::genesis::genesis_block(&genesis_bytes).expect("genesis block");
        let genesis_id = genesis_block.id();
        let genesis_block_bytes = genesis_block.bytes().to_vec();

        let mut vm = PlatformVm::new();
        let token = CancellationToken::new();
        let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        vm.initialize(
            &token,
            chain_ctx(),
            db,
            &genesis_bytes,
            b"",
            b"",
            Vec::new(),
            Arc::new(NoopAppSender),
        )
        .await
        .expect("initialize");
        (vm, genesis_id, genesis_block_bytes)
    }

    /// `pchain_sync_to_tip` (height-0 subset): the bootstrapper drives the VM
    /// from a recorded single-block frontier (= genesis) through frontier
    /// discovery → agreement → fetch → execute → handoff, ending at height 0 with
    /// `last_accepted == genesis_id`.
    #[tokio::test]
    async fn pchain_sync_to_tip() {
        let token = CancellationToken::new();
        let (vm, genesis_id, genesis_block_bytes) = init_vm().await;

        // Sanity: the recorded frontier IS the VM's last-accepted genesis block,
        // and the M4.27 BatchedChainVm capability is exposed.
        assert_eq!(
            vm.last_accepted(&token).await.expect("last accepted"),
            genesis_id
        );
        assert!(
            ChainVm::as_batched(&vm).is_some(),
            "P-Chain must implement BatchedChainVm for linear bootstrap"
        );

        // A single beacon reporting genesis as its accepted frontier.
        let acceptor = Arc::new(NoOpAcceptor);
        let ctx = Arc::new(ConsensusContext::new(
            chain_ctx(),
            "P".to_string(),
            acceptor,
            Arc::new(NoOpAcceptor),
        ));
        let sender = Arc::new(FetchSender::default());
        let beacon = NodeId::from([10u8; 20]);
        let mut beacons = BTreeMap::new();
        beacons.insert(beacon, 1u64);

        let vm = Arc::new(AsyncMutex::new(vm));
        let cfg = Config {
            subnet_id: Id::EMPTY,
            ctx: ctx.clone(),
            vm: Arc::clone(&vm),
            sender: sender.clone(),
            beacons,
            token: token.clone(),
        };
        let mut boot = Bootstrapper::new(cfg);

        // Start: enters Bootstrapping + asks the beacon for its frontier.
        boot.start(0).await.expect("start");
        assert_eq!(boot.phase(), Phase::DiscoveringFrontier);
        assert_eq!(**ctx.state.load(), EngineState::Bootstrapping);

        // The beacon reports genesis as both its frontier and its accepted set →
        // weight threshold met → fetch the genesis ancestry.
        boot.accepted_frontier(beacon, 1, genesis_id)
            .await
            .expect("accepted_frontier");
        assert_eq!(boot.phase(), Phase::AgreeingFrontier);
        boot.accepted(beacon, 2, &[genesis_id])
            .await
            .expect("accepted");

        // The bootstrapper requested the genesis ancestry; serve the genesis
        // block bytes (the recorded height-0 frontier).
        let ga = sender.take_get_ancestors();
        let (node, req, wanted) = ga
            .into_iter()
            .find(|(_, _, id)| *id == genesis_id)
            .expect("GetAncestors for the genesis frontier");
        assert_eq!(wanted, genesis_id);
        boot.ancestors(node, req, std::slice::from_ref(&genesis_block_bytes))
            .await
            .expect("ancestors");

        // The range (empty: genesis is at the local last-accepted height) executed
        // and the node handed off to normal operation.
        assert!(boot.is_finished(), "bootstrapper must hand off at height 0");
        assert_eq!(boot.phase(), Phase::Finished);
        assert_eq!(**ctx.state.load(), EngineState::NormalOp);

        // last_accepted is still the genesis id, and the genesis block round-trips
        // through the (post-bootstrap) VM.
        let vm = vm.lock().await;
        let last = vm.last_accepted(&token).await.expect("last accepted");
        assert_eq!(
            last, genesis_id,
            "P-Chain bootstrap stops at the genesis id"
        );
        let blk = vm.get_block(&token, genesis_id).await.expect("get genesis");
        assert_eq!(blk.id(), genesis_id);
        assert_eq!(blk.height(), 0);
        assert_eq!(blk.bytes(), genesis_block_bytes.as_slice());
    }
}
