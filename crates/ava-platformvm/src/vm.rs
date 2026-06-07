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
//! No gossip mempool (M4.26 — `build_block` uses a minimal in-VM decision queue,
//! empty during read-only sync), no JSON-RPC service (M4.28 — `create_handlers`
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
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{Block as VmBlock, ChainVm, StateSyncableVm};
use ava_vm::connector::Connector;
use ava_vm::error::{Error as VmError, Result as VmResult};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{HttpHandler, Vm, VmEvent};

use crate::block::Block;
use crate::block::builder;
use crate::block::executor::BlockManager;
use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::state::State;
use crate::txs::executor::{Backend, StakingConfig, UpgradeSchedule};
use crate::txs::fee::simple_calculator::StaticFeeConfig;
use crate::txs::{Tx, codec};
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
    /// A minimal in-VM decision-tx queue (placeholder for the M4.26 mempool;
    /// empty during read-only sync).
    mempool: Mutex<Vec<Tx>>,
}

impl Default for PlatformVm {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformVm {
    /// Builds an uninitialized `PlatformVm`. Call [`Vm::initialize`] before use.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: None,
            ctx: None,
            state: EngineState::Initializing,
            preferred: Id::EMPTY,
            genesis_id: Id::EMPTY,
            mempool: Mutex::new(Vec::new()),
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

    /// Builds the executor [`Backend`] from the chain context (read-only-sync
    /// subset; the full per-network staking/fee config is M8/`ava-genesis`).
    fn backend(ctx: &ChainContext) -> Backend {
        Backend {
            upgrades: upgrade_schedule(ctx),
            staking: StakingConfig::mainnet(),
            static_fee_config: StaticFeeConfig::MAINNET,
            network_id: ctx.network_id,
            chain_id: ctx.chain_id,
            avax_asset_id: ctx.avax_asset_id,
            node_id: ctx.node_id,
            fx: ava_secp256k1fx::Fx::new(Arc::new(ava_utils::clock::RealClock)),
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
            Self::backend(&chain_ctx),
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

    async fn create_handlers(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<HashMap<String, HttpHandler>> {
        // The JSON-RPC service is M4.28.
        Ok(HashMap::new())
    }

    async fn new_http_handler(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        Ok(None)
    }

    async fn wait_for_event(&self, _token: &CancellationToken) -> VmResult<VmEvent> {
        // No gossip mempool yet (M4.26); read-only sync issues no txs.
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
            // change), clamped by SyncBound.
            let now = SystemTime::now();
            let parent_ts = parent_state.timestamp();
            let next_change = next_staker_change_time(parent_state.as_ref());
            let (timestamp, time_was_capped) =
                builder::next_block_time(now, parent_ts, next_change, SYNC_BOUND);

            // Decision txs from the minimal in-VM queue (empty in read-only sync).
            let decision_txs = self.mempool.lock().clone();

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
}
