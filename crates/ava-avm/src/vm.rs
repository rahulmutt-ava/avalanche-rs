// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `AvmVm` — the X-Chain (AVM) `block.ChainVM` (`vms/avm/vm.go`, specs 09 §1/§7;
//! 07 §2 VM framework).
//!
//! [`AvmVm`] is the integration capstone of M5: it wires the M5.16
//! [`BlockManager`] (verify/accept/reject over the persisted [`State`]), the
//! M5.17 [`build_block`] builder, the M5.17 [`Mempool`], and the M5.18 gossip
//! handler + [`AtomicAppHandler`] behind the engine-facing [`Vm`] / [`ChainVm`]
//! traits, returning Snowman [`Block`](VmBlock)s.
//!
//! ## Shared block-manager + the block wrapper
//!
//! The engine-facing [`VmBlock`] returned by `get_block`/`parse_block`/
//! `build_block` carries `verify`/`accept`/`reject`, which mutate the
//! [`BlockManager`]. The VM holds the manager behind an `Arc<Shared>` whose
//! single [`Mutex`] guards it (the engine drives the VM as one actor, so
//! contention is structural, not concurrent). Each returned [`AvmBlock`] holds a
//! clone of the `Arc<Shared>` so its decision methods drive the shared manager.
//!
//! ## `build_block` auto-verifies into the manager cache
//!
//! Go's engine verifies a block before it becomes the preferred parent. The
//! generic VM-conformance battery (07 §10) sets preference to a freshly built,
//! *not-yet-verified* block and then builds a child on it. To support that — and
//! to keep `build_block` total — [`ChainVm::build_block`] verifies the block it
//! builds into the manager's diff cache before returning, so the block is
//! immediately resolvable as a parent state view. A subsequent
//! `verify`/`accept` from the engine simply re-runs / consumes that cache.
//!
//! ## `build_block` cache lifetime and remove-on-build
//!
//! `build_block` does two things beyond Go's `BuildBlock` signature:
//!
//! 1. **Auto-verify into the diff cache.** The built block is immediately
//!    inserted into the [`BlockManager`]'s processing-block cache
//!    (`blk_id_to_state`) so it is resolvable as a parent state view before the
//!    engine explicitly calls `verify` on it (the generic VM-conformance battery
//!    sets preference to an unaccepted built block, then builds a child — Go
//!    parity via `builder.go`).  A subsequent engine-driven `verify`/`accept`
//!    re-runs / consumes the same cache entry.  This contrasts with the P-Chain
//!    VM, which does NOT verify-on-build and does NOT remove packed txs on build.
//!
//! 2. **Remove-on-build.** The txs the builder actually packed are removed from
//!    the mempool immediately after `build_block` returns, so a subsequent build
//!    over the same (unaccepted) parent produces a fresh block rather than
//!    re-packing the same txs.  Packed txs do **not** re-enter the mempool until
//!    the block is **rejected** (see [`AvmBlock::reject`]); an engine that neither
//!    accepts nor rejects strands them — benign at shutdown because the pool is
//!    discarded.  Because AVM relies on the engine deciding every built block
//!    (the Snowman `ava_snow` processing-set contract), an unbounded-build stress
//!    test should land with real engine wiring (M5.20+).
//!
//! ## X-Chain-specific seams (each documented inline)
//!
//! * **`shared_memory`** is **not** on [`ChainContext`] yet (like the P-Chain's
//!   deferred validator-state field), so `initialize` installs an in-memory
//!   no-op [`SharedMemory`] ([`NoopSharedMemory`]). The conformance battery uses
//!   only `BaseTx` (no atomic), so it never touches it. Wiring the real
//!   cross-chain shared memory is **M5.20**.
//! * **`verify.SameSubnet`** validator-state stays SKIPPED (no `validator_state`
//!   on [`ChainContext`] yet) — the same documented seam as M5.13. M5.20+ wires
//!   it.
//! * **X-Chain genesis.** There is no `ava-genesis` crate (M8) and no avm
//!   genesis-asset format yet. `initialize` derives the genesis seed
//!   (stop-vertex id + Unix timestamp) from a **minimal synthetic genesis**: the
//!   32-byte stop-vertex id followed by the 8-byte big-endian Unix timestamp.
//!   Full Go-format X-Chain genesis (the `CreateAssetTx` list + alloc) is
//!   deferred to **M8/`ava-genesis`**.
//! * **No-tx ⇒ no block.** The X-Chain has only `StandardBlock`; the builder
//!   returns [`Error::NoPendingBlocks`] when nothing packs. Callers must keep the
//!   mempool fed (via gossip / the issue path); the genesis-asset UTXO seeding a
//!   real node performs is the M8 follow-up.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_database::{BatchOps, DynDatabase};
use ava_snow::{ChainContext, EngineState};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, RealClock};
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{Block as VmBlock, ChainVm};
use ava_vm::components::avax::shared_memory::{IndexedResult, Requests, SharedMemory};
use ava_vm::connector::Connector;
use ava_vm::error::{Error as VmError, Result as VmResult};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{HttpHandler, LockOptions, Vm, VmEvent};

use crate::block::builder::BuildBlockParams;
use crate::block::executor::{BlockManager, BlockManagerConfig};
use crate::block::{Block, build_block};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::fx::dispatch::Dispatch;
use crate::jsonrpc::registry_service;
use crate::mempool::Mempool;
use crate::network::atomic::{AppGossipHandler, AtomicAppHandler};
use crate::network::gossip::{DropReason, HandleOutcome, TxGossipHandler, TxMarshaller};
use crate::network::tx_verifier::SyntacticTxVerifier;
use crate::state::State;
use crate::state::chain::ReadOnlyChain;
use crate::state::versions::Versions;
use crate::txs::Tx;
use crate::txs::codec::{Codec, codec};
use crate::txs::executor::{Backend, Config as FeeConfig};

mod dyndb;
pub use dyndb::DynDb;

/// `numFxs` — the X-Chain registers exactly three feature extensions
/// (secp256k1fx, nftfx, propertyfx; specs 09 §2.2).
const NUM_FXS: usize = 3;

/// The mutable core shared between the [`AvmVm`] and every [`AvmBlock`] it hands
/// out: the block manager (which owns the persisted [`State`]) plus the shared
/// mempool. Guarded by a single [`Mutex`] — the engine drives the VM as one
/// actor, so contention is structural, not concurrent.
struct Shared {
    /// The block manager (verify/accept/reject over the persisted state).
    manager: Mutex<BlockManager<DynDb>>,
    /// The decision-tx mempool, shared with the gossip handler ([`AvmGossipHandler`]).
    mempool: Arc<Mutex<Mempool>>,
    /// The cross-chain shared-memory handle (also held by the block manager);
    /// retained so `create_handlers` can wire the `getUTXOs` `sourceChain`
    /// atomic path (M8.23b). The live node-wide implementation replaces the
    /// `NoopSharedMemory` at the M8 `ava-chains` wiring.
    shared_memory: Arc<dyn SharedMemory>,
}

/// The [`ReadOnlyChain`] view over the VM's live state: every read locks the
/// block manager (the moral equivalent of Go's per-request `vm.ctx.Lock` in
/// `service.go`) and forwards to the persisted [`State`]. Backs the M8.22
/// `avm.*` API service.
struct VmChainReader {
    shared: Arc<Shared>,
}

impl ReadOnlyChain for VmChainReader {
    fn get_utxo(&self, utxo_id: Id) -> Result<Vec<u8>> {
        self.shared.manager.lock().state().get_utxo(utxo_id)
    }
    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>> {
        self.shared.manager.lock().state().get_tx(tx_id)
    }
    fn get_block_id_at_height(&self, height: u64) -> Option<Id> {
        self.shared
            .manager
            .lock()
            .state()
            .get_block_id_at_height(height)
    }
    fn get_block(&self, blk_id: Id) -> Result<Vec<u8>> {
        self.shared.manager.lock().state().get_block(blk_id)
    }
    fn get_last_accepted(&self) -> Id {
        self.shared.manager.lock().state().get_last_accepted()
    }
    fn get_timestamp(&self) -> SystemTime {
        self.shared.manager.lock().state().get_timestamp()
    }
    fn utxo_ids(
        &self,
        addr: &ava_types::short_id::ShortId,
        previous: Id,
        limit: usize,
    ) -> Result<Vec<Id>> {
        self.shared
            .manager
            .lock()
            .state()
            .utxo_ids(addr, previous, limit)
    }
}

/// The [`crate::service::TxIssuer`] seam over the shared mempool: `issueTx`
/// admits through the SAME dedupe → verify → add path inbound gossip uses
/// ([`TxGossipHandler::handle_gossiped_tx`] with the [`SyntacticTxVerifier`]),
/// so RPC submission and gossip admission cannot diverge. Outbound re-gossip
/// of the admitted tx is a recorded deferral (live `Network::gossip`, M8).
struct VmTxIssuer {
    shared: Arc<Shared>,
}

impl crate::service::TxIssuer for VmTxIssuer {
    fn issue_tx(&self, tx: Tx) -> std::result::Result<(), String> {
        let mut pool = self.shared.mempool.lock();
        match TxGossipHandler::new().handle_gossiped_tx(&mut pool, &SyntacticTxVerifier, tx) {
            HandleOutcome::Added => Ok(()),
            HandleOutcome::Dropped(DropReason::Duplicate) => Err("duplicate tx".to_string()),
            HandleOutcome::Dropped(DropReason::Verification(reason)) => Err(reason),
            HandleOutcome::Dropped(DropReason::Mempool(e)) => Err(e.to_string()),
        }
    }
}

/// `avm.VM` — the X-Chain Snowman VM over the [`DynDb`]-adapted engine database
/// (specs 09 §1).
pub struct AvmVm {
    /// `None` until [`initialize`](Vm::initialize) builds the shared core.
    shared: Option<Arc<Shared>>,
    /// The immutable chain identity/handles received at `initialize`.
    ctx: Option<Arc<ChainContext>>,
    /// The current engine phase (Go `vm.bootstrapped`).
    state: EngineState,
    /// The currently preferred (leaf) block id (Go `vm.preferred`).
    preferred: Id,
    /// The genesis block id (the initial last-accepted / preference).
    genesis_id: Id,
    /// The atomic gossip-handler switch (M5.18); `app_gossip` delegates here.
    /// `None` until `initialize` installs the live handler.
    gossip_handler: Option<Arc<AtomicAppHandler>>,
    /// The parsed VM [`Config`] (fee schedule: `tx_fee` / `create_asset_tx_fee`).
    /// Retained from `initialize` so `set_state(NormalOp)` can rebuild the
    /// manager's executor [`Backend`] with the same fees and `bootstrapped=true`
    /// (Go `vm.onBootstrapped`).
    fee_config: Config,
    /// The injectable clock — the ONLY wall-clock source (specs 24 hazard #5).
    ///
    /// Backs both the proposed block time (`build_block`'s `now`) and the fx
    /// [`Dispatch`] locktime/credential checks, so the whole VM observes one
    /// clock. [`AvmVm::new`] installs a [`RealClock`]; tests inject a `MockClock`
    /// via [`AvmVm::with_clock`].
    clock: Arc<dyn Clock>,
}

impl Default for AvmVm {
    fn default() -> Self {
        Self::new()
    }
}

impl AvmVm {
    /// Builds an uninitialized `AvmVm` reading time through a [`RealClock`].
    /// Call [`Vm::initialize`] before use.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(RealClock))
    }

    /// Builds an uninitialized `AvmVm` reading time through `clock` — the
    /// determinism injection seam (specs 24 hazard #5). The clock backs both the
    /// proposed block time (`build_block`'s `now`) and the fx [`Dispatch`]
    /// locktime/credential checks, so the whole VM observes one clock. Used by
    /// tests (mirroring [`with_state`](Self::with_state) /
    /// [`mempool_add`](Self::mempool_add)) to pin block times via a `MockClock`
    /// without depending on the wall clock. Call [`Vm::initialize`] before use.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: None,
            ctx: None,
            state: EngineState::Initializing,
            preferred: Id::EMPTY,
            genesis_id: Id::EMPTY,
            gossip_handler: None,
            fee_config: Config::default(),
            clock,
        }
    }

    /// The shared core, or [`Error::NotInitialized`] if `initialize` has not run.
    fn shared(&self) -> Result<&Arc<Shared>> {
        self.shared.as_ref().ok_or(Error::NotInitialized)
    }

    /// Wraps an X-Chain [`Block`] as the engine-facing [`VmBlock`].
    ///
    /// The X-Chain [`Block`] / [`Tx`] are `Send + Sync` (they hold only
    /// `Bytes`/`Id` caches, with no `!Sync` interior cells), so the wrapper holds
    /// the parsed block directly rather than re-parsing on demand (unlike the
    /// P-Chain, whose `!Sync` tx forces a re-parse in its block wrapper).
    fn wrap(&self, block: Block) -> Result<Arc<dyn VmBlock>> {
        let shared = self.shared()?;
        Ok(Arc::new(AvmBlock {
            id: block.id(),
            parent: block.parent_id(),
            height: block.height(),
            timestamp_secs: block.timestamp(),
            bytes: block.bytes().to_vec(),
            block,
            shared: Arc::clone(shared),
        }))
    }

    /// **Test helper** — seed the genesis state's UTXO / tx stores directly.
    ///
    /// The full Go-format X-Chain genesis-asset alloc is the M8/`ava-genesis`
    /// follow-up; until then a caller (the conformance battery / a node
    /// bootstrap shim) seeds the spendable UTXO set this way. Commits the seed.
    ///
    /// # Errors
    /// Returns [`Error::NotInitialized`] before `initialize`, or an
    /// [`Error::Database`] if the commit fails.
    #[doc(hidden)]
    pub fn seed_genesis_state(&self, seed: impl FnOnce(&mut State<DynDb>)) -> Result<()> {
        let shared = self.shared()?;
        let mut mgr = shared.manager.lock();
        mgr.seed_state(seed)
    }

    /// **Test helper** — run `read` against the persisted [`State`] read surface.
    ///
    /// The differential harness (M5.22 `differential::xchain_issue_tx`) collects
    /// a normalized observation over the post-accept UTXO set; the `Chain` trait
    /// exposes no enumeration, so the harness reads back the UTXO ids it knows it
    /// touched via this read-only seam (the read-side mirror of
    /// [`seed_genesis_state`](Self::seed_genesis_state)).
    ///
    /// # Errors
    /// Returns [`Error::NotInitialized`] before `initialize`.
    #[doc(hidden)]
    pub fn with_state<R>(&self, read: impl FnOnce(&State<DynDb>) -> R) -> Result<R> {
        let shared = self.shared()?;
        let mgr = shared.manager.lock();
        Ok(read(mgr.state()))
    }

    /// **Test helper** — admit `tx` to the shared mempool.
    ///
    /// Production admission flows through the gossip handler ([`Self::app_gossip`])
    /// or the (not-yet-ported) issue RPC; this is the direct seam the conformance
    /// battery + unit tests use.
    ///
    /// # Errors
    /// Returns [`Error::NotInitialized`] before `initialize`, or maps a mempool
    /// rejection (duplicate / full / conflict) to an [`Error::Database`]-free
    /// descriptive [`Error::Config`] — callers in tests treat any error as fatal.
    #[doc(hidden)]
    pub fn mempool_add(&self, tx: Tx) -> Result<()> {
        let shared = self.shared()?;
        shared
            .mempool
            .lock()
            .add(tx)
            .map_err(|e| Error::Config(format!("mempool add: {e}")))
    }
}

/// Builds the executor [`Backend`] from the chain context + fee config
/// (read-only-sync subset; the full per-network config is M8/`ava-genesis`).
fn backend(ctx: &ChainContext, fees: Config, bootstrapped: bool) -> Backend {
    Backend::new(
        ctx.network_id,
        ctx.chain_id,
        FeeConfig::new(fees.tx_fee, fees.create_asset_tx_fee),
        ctx.avax_asset_id,
        NUM_FXS,
        bootstrapped,
    )
}

/// Builds the fx [`Dispatch`] table (secp256k1fx / nftfx / propertyfx).
///
/// The fx ids match the avm registration (secp at the AVAX/empty id; nft /
/// property at their conventional sentinel ids — the real registration ids land
/// with the genesis-asset alloc in M8). All three share the VM's injected
/// `clock` (specs 24 hazard #5), so the fx locktime/credential checks and the
/// proposed block time observe one clock.
fn dispatch(clock: &Arc<dyn Clock>) -> Dispatch {
    Dispatch::new(
        Id::EMPTY,
        Id::from([1u8; 32]),
        Id::from([2u8; 32]),
        Arc::clone(clock),
    )
}

/// The minimal synthetic X-Chain genesis seed (specs 09 §1): the 32-byte
/// stop-vertex id followed by the 8-byte big-endian Unix-second timestamp.
///
/// The full Go genesis-asset format (the `CreateAssetTx` list + alloc) is the
/// M8/`ava-genesis` follow-up; this seed carries exactly what
/// [`State::initialize_chain_state`] consumes.
fn parse_genesis(genesis_bytes: &[u8]) -> Result<(Id, SystemTime)> {
    let stop_slice = genesis_bytes.get(..32).ok_or(Error::InvalidGenesis)?;
    let ts_slice = genesis_bytes.get(32..40).ok_or(Error::InvalidGenesis)?;
    let mut stop = [0u8; 32];
    stop.copy_from_slice(stop_slice);
    let mut ts = [0u8; 8];
    ts.copy_from_slice(ts_slice);
    let secs = u64::from_be_bytes(ts);
    let genesis_ts = UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap_or(UNIX_EPOCH);
    Ok((Id::from(stop), genesis_ts))
}

// ---------------------------------------------------------------------------
// In-memory no-op SharedMemory (M5.19 seam; real cross-chain wiring is M5.20).
// ---------------------------------------------------------------------------

/// An in-memory, no-op [`SharedMemory`] used until the engine threads a real
/// cross-chain shared-memory handle through [`ChainContext`] (M5.20).
///
/// `get`/`indexed` return empty results and `apply` is a no-op. The conformance
/// battery uses only `BaseTx` (empty atomic requests), so block accept goes
/// through the manager's `state.commit()` path and never calls `apply`.
///
/// ## Deferred (M5.20)
///
/// Because this double cannot replay the state-write `batches` against the
/// underlying DB (it holds no DB handle), an `ExportTx`/`ImportTx` accept — whose
/// manager path hands the not-yet-committed state batch here and then `abort`s
/// the versiondb — would currently NOT be functional (the cross-chain put + the
/// co-committed state write are both dropped). Wiring the real `ava-chains`
/// `SharedMemoryView` (which atomically replays the batch) is **M5.20**.
#[derive(Debug, Default)]
struct NoopSharedMemory;

impl SharedMemory for NoopSharedMemory {
    fn get(&self, _peer_chain: Id, keys: &[Vec<u8>]) -> ava_vm::error::Result<Vec<Vec<u8>>> {
        Ok(vec![Vec::new(); keys.len()])
    }

    fn indexed(
        &self,
        _peer_chain: Id,
        _traits: &[Vec<u8>],
        _start_trait: &[u8],
        _start_key: &[u8],
        _limit: usize,
    ) -> ava_vm::error::Result<IndexedResult> {
        Ok((Vec::new(), Vec::new(), Vec::new()))
    }

    fn apply(
        &self,
        requests: std::collections::BTreeMap<Id, Requests>,
        _batches: &[BatchOps],
    ) -> ava_vm::error::Result<()> {
        debug_assert!(
            requests.is_empty(),
            "NoopSharedMemory cannot apply atomic requests; \
             real cross-chain SharedMemory wiring is M5.20"
        );
        // No-op until M5.20 (see the type doc): the conformance battery never
        // reaches here (BaseTx blocks have empty atomic requests).
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The engine-facing block wrapper.
// ---------------------------------------------------------------------------

/// An X-Chain block presented to the consensus engine: a `Send + Sync` holder of
/// a parsed [`Block`] that drives the shared [`BlockManager`] on
/// `verify`/`accept`/`reject` (Go the per-block `Verify`/`Accept`/`Reject`
/// delegating to `block/executor`).
struct AvmBlock {
    id: Id,
    parent: Id,
    height: u64,
    /// The block's Unix-second timestamp.
    timestamp_secs: u64,
    bytes: Vec<u8>,
    /// The parsed block driven through the manager (X-Chain `Block` is `Sync`).
    block: Block,
    shared: Arc<Shared>,
}

#[async_trait]
impl VmBlock for AvmBlock {
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
        UNIX_EPOCH
            .checked_add(Duration::from_secs(self.timestamp_secs))
            .unwrap_or(UNIX_EPOCH)
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    async fn verify(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        let mut mgr = self.shared.manager.lock();
        mgr.verify(&self.block).map_err(ava_snow::Error::from)
    }

    async fn accept(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        let mut mgr = self.shared.manager.lock();
        mgr.accept(&self.block).map_err(ava_snow::Error::from)?;
        // Drop the accepted txs from the mempool so they are not re-packed.
        let mut pool = self.shared.mempool.lock();
        for tx in self.block.txs() {
            pool.remove(&tx.id());
        }
        Ok(())
    }

    async fn reject(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        {
            let mut mgr = self.shared.manager.lock();
            mgr.reject(&self.block);
        }
        // Re-admit the rejected block's txs to the mempool so they can be packed
        // into a future block (Go `mempool.Add` on reject). A re-admission that
        // now conflicts/dupes is a benign drop.
        let mut pool = self.shared.mempool.lock();
        for tx in self.block.txs() {
            let _ = pool.add(tx.clone());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The live gossip handler (M5.18 atomic switch payoff).
// ---------------------------------------------------------------------------

/// The live [`AppGossipHandler`] installed into the [`AtomicAppHandler`]: on an
/// inbound gossip message it unmarshals a [`Tx`] and runs it through the
/// [`TxGossipHandler`] admission policy against the shared mempool, using the
/// cheap [`SyntacticTxVerifier`] (matching the P-Chain, which wired only the
/// syntactic verifier; upgrading to semantic-over-preferred-state is a
/// documented follow-up).
struct AvmGossipHandler {
    mempool: Arc<Mutex<Mempool>>,
    marshaller: TxMarshaller,
    handler: TxGossipHandler,
}

impl AppGossipHandler for AvmGossipHandler {
    fn handle_app_gossip(&self, node: NodeId, msg: &[u8]) {
        // Fire-and-forget: a malformed message is logged + dropped, never
        // propagated (the engine ignores gossip errors).
        let tx = match self.marshaller.unmarshal(msg) {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(%node, reason = %e, "avm app_gossip: dropping malformed tx");
                return;
            }
        };
        let mut pool = self.mempool.lock();
        let outcome = self
            .handler
            .handle_gossiped_tx(&mut pool, &SyntacticTxVerifier, tx);
        tracing::trace!(%node, ?outcome, "avm app_gossip: handled gossiped tx");
    }
}

// ---------------------------------------------------------------------------
// Vm supertraits (app / health / connector).
// ---------------------------------------------------------------------------

#[async_trait]
impl AppHandler for AvmVm {
    async fn app_request(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _deadline: std::time::Instant,
        _request: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_request_failed(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _err: AppError,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_response(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _response: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_gossip(
        &mut self,
        _token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> VmResult<()> {
        // Delegate to the M5.18 atomic switch (wired in M5.19). A gossip message
        // before `initialize` is silently ignored (no live handler installed).
        if let Some(handler) = self.gossip_handler.as_ref() {
            handler.handle_app_gossip(node, msg);
        }
        Ok(())
    }
}

#[async_trait]
impl HealthCheck for AvmVm {
    async fn health_check(&self, _token: &CancellationToken) -> VmResult<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl Connector for AvmVm {
    async fn connected(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _version: ava_version::application::Application,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn disconnected(&mut self, _token: &CancellationToken, _node: NodeId) -> VmResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Vm.
// ---------------------------------------------------------------------------

#[async_trait]
impl Vm for AvmVm {
    async fn initialize(
        &mut self,
        _token: &CancellationToken,
        chain_ctx: Arc<ChainContext>,
        db: Arc<dyn DynDatabase>,
        genesis_bytes: &[u8],
        _upgrade_bytes: &[u8],
        config_bytes: &[u8],
        _fxs: Vec<Fx>,
        _app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        // Parse the VM config (mainnet fee defaults when `config_bytes` empty).
        let fee_config = Config::parse(config_bytes).map_err(VmError::from)?;

        // Open State over the engine-provided DB (adapted to the typed surface).
        let mut state = State::new(Arc::new(DynDb::new(db))).map_err(VmError::from)?;

        // Seed the genesis Snowman block (height 0) from the synthetic genesis
        // seed (stop-vertex id + Unix timestamp), idempotent on re-open.
        let (stop_vertex_id, genesis_ts) = parse_genesis(genesis_bytes).map_err(VmError::from)?;
        let c = codec().map_err(|e| VmError::from(Error::from(e)))?;
        state
            .initialize_chain_state(stop_vertex_id, genesis_ts, &c)
            .map_err(VmError::from)?;
        let genesis_id = state.get_last_accepted();

        // Build the M5.16 block manager (bootstrapped=false; flipped at NormalOp).
        // The shared-memory handle is also retained on `Shared` so the API
        // service's `getUTXOs` sourceChain path reads the same store (M8.23b).
        let shared_memory: Arc<dyn SharedMemory> = Arc::new(NoopSharedMemory);
        let mgr_config = BlockManagerConfig {
            backend: backend(&chain_ctx, fee_config, false),
            dispatch: dispatch(&self.clock),
            shared_memory: Arc::clone(&shared_memory),
        };
        let manager = BlockManager::new(state, mgr_config);

        // The shared mempool, shared with the live gossip handler.
        let mempool = Arc::new(Mutex::new(Mempool::new()));

        // Install the M5.18 atomic gossip switch with the live handler.
        let gossip_handler: Arc<dyn AppGossipHandler> = Arc::new(AvmGossipHandler {
            mempool: Arc::clone(&mempool),
            marshaller: TxMarshaller::new(),
            handler: TxGossipHandler::new(),
        });

        self.shared = Some(Arc::new(Shared {
            manager: Mutex::new(manager),
            mempool,
            shared_memory,
        }));
        self.gossip_handler = Some(Arc::new(AtomicAppHandler::new(gossip_handler)));
        self.ctx = Some(chain_ctx);
        self.fee_config = fee_config;
        self.genesis_id = genesis_id;
        self.preferred = genesis_id;
        self.state = EngineState::Initializing;
        Ok(())
    }

    async fn set_state(&mut self, _token: &CancellationToken, state: EngineState) -> VmResult<()> {
        // Bootstrapping → NormalOp is the only transition that changes VM
        // behaviour: it flips the executor backend + fx dispatch to bootstrapped
        // (Go `vm.onBootstrapped` → enables fx signature verification + the
        // bootstrapped semantic checks). Rebuild the manager's backend/dispatch
        // with `bootstrapped=true`.
        if state == EngineState::NormalOp
            && self.state != EngineState::NormalOp
            && let Some(shared) = self.shared.as_ref()
        {
            let ctx = self
                .ctx
                .as_ref()
                .ok_or(VmError::from(Error::NotInitialized))?;
            let mut mgr = shared.manager.lock();
            let mut d = dispatch(&self.clock);
            d.bootstrapped();
            mgr.set_bootstrapped(backend(ctx, self.fee_config, true), d);
        }
        self.state = state;
        Ok(())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> VmResult<()> {
        // Idempotent: dropping the shared core releases the DB handle. A second
        // call is a no-op.
        self.shared = None;
        self.gossip_handler = None;
        Ok(())
    }

    async fn version(&self, _token: &CancellationToken) -> VmResult<String> {
        // TODO(M8): source from ava-version instead of the hard-coded string
        Ok("avm/0.0.0".to_string())
    }

    /// Go `CreateHandlers` (`vms/avm/vm.go:293-318`): the gorilla JSON-RPC
    /// server at extension `""` with the service registered as `"avm"`. Go
    /// also mounts the keystore-backed `"/wallet"` server — out of scope for
    /// the Rust port's key-management boundary (recorded in
    /// `tests/PORTING.md`), as is the `vm.metrics` request-interceptor wrap
    /// (the proposervm M8.22 precedent).
    async fn create_handlers(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<HashMap<String, HttpHandler>> {
        let shared = self.shared().map_err(VmError::from)?;
        let ctx = self
            .ctx
            .as_ref()
            .ok_or(VmError::from(Error::NotInitialized))?;
        // M8.23b: wire the chain id (local-address + sourceChain checks), the
        // fee schedule (`getTxFee`), and the shared-memory handle (`getUTXOs`
        // sourceChain atomic path). The node-level `BCLookup` aliaser
        // (`ChainLookup`: "P"/"C" aliases) is the recorded `ava-node` wiring
        // follow-up — chain-id strings resolve via the built-in fallback.
        let service = Arc::new(
            crate::service::Service::new(
                Arc::new(VmChainReader {
                    shared: Arc::clone(shared),
                }),
                ctx.network_id,
            )
            .with_chain_id(ctx.chain_id)
            .with_fees(self.fee_config.tx_fee, self.fee_config.create_asset_tx_fee)
            .with_shared_memory(Arc::clone(&shared.shared_memory)),
        );
        let issuer = Arc::new(VmTxIssuer {
            shared: Arc::clone(shared),
        });
        let registry = Arc::new(crate::service::registry(service, issuer));
        let mut handlers = HashMap::new();
        handlers.insert(
            String::new(),
            // Go's modern CreateHandlers map carries no lock semantics; the
            // service locks the block manager per read (VmChainReader).
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

    async fn wait_for_event(&self, token: &CancellationToken) -> VmResult<VmEvent> {
        // Report PendingTxs when the mempool is non-empty; otherwise block until
        // cancellation (the engine cancels the token on shutdown).
        //
        // On cancellation we return `Ok(VmEvent::PendingTxs)` because `VmEvent`
        // has no cancellation/shutdown variant (`PendingTxs = 1`,
        // `StateSyncDone = 2` — verified against the `ava-vm` enum definition).
        // The engine loop re-checks mempool emptiness after each wake and re-parks
        // when nothing is pending, so the spurious `PendingTxs` on shutdown is
        // harmless (the engine is already tearing down when it cancels the token).
        let pending = self
            .shared
            .as_ref()
            .is_some_and(|s| !s.mempool.lock().is_empty());
        if pending {
            Ok(VmEvent::PendingTxs)
        } else {
            token.cancelled().await;
            Ok(VmEvent::PendingTxs)
        }
    }
}

// ---------------------------------------------------------------------------
// ChainVm.
// ---------------------------------------------------------------------------

#[async_trait]
impl ChainVm for AvmVm {
    async fn build_block(&mut self, _token: &CancellationToken) -> VmResult<Arc<dyn VmBlock>> {
        let shared = self.shared().map_err(VmError::from)?;

        // Build + verify-into-cache under one lock so the freshly built block is
        // immediately resolvable as a parent state view (the conformance battery
        // sets preference to an unaccepted built block, then builds its child).
        //
        // ## Block packing policy (M5.19, Go-parity)
        //
        // Block packing is FIFO + size-capped (Go `builder.go` /
        // `packDecisionTxs`): the VM hands the M5.17 `build_block` the full FIFO
        // mempool snapshot and the builder packs as many txs as verify against
        // the running diff and fit under `TARGET_BLOCK_SIZE`, dropping the rest.
        // Only the txs the builder actually PACKED are removed from the mempool
        // (remove-on-build); a reject re-admits exactly those (see
        // [`AvmBlock::reject`]), and an accept drops them permanently (see
        // [`AvmBlock::accept`]). Dropped (unpacked) candidates stay in the pool.
        let block = {
            let mut mgr = shared.manager.lock();

            let parent_id = self.preferred;
            let parent_state = mgr.get_state(parent_id).ok_or(VmError::NotFound)?;
            let parent_height = mgr.height_of(parent_id).map_err(VmError::from)?;
            let parent_time = parent_state.get_timestamp();

            // The full FIFO mempool snapshot is the candidate set; the builder
            // packs as many as fit/verify (Go packs multiple txs per block).
            let candidate_txs: Vec<Tx> = shared.mempool.lock().snapshot();

            let out = build_block(BuildBlockParams {
                codec: Codec(),
                parent_id,
                parent_height,
                parent_time,
                // The injected clock — NOT the wall clock directly (specs 24
                // hazard #5). Clamped by the builder to `max(parent_time, now)`.
                now: self.clock.now(),
                parent_state,
                backend: mgr.backend(),
                dispatch: mgr.dispatch(),
                candidate_txs,
            })
            .map_err(VmError::from)?;

            // Verify the block into the manager's diff cache so it is resolvable
            // as a parent before the engine explicitly verifies it.
            mgr.verify(&out.block).map_err(VmError::from)?;
            out.block
        };

        // Remove exactly the txs the builder PACKED into the block from the
        // mempool (remove-on-build, Go-parity), so a subsequent build over an
        // unaccepted child advances past them. Dropped (unpacked) candidates are
        // left in the pool. A reject re-admits the packed set (see
        // [`AvmBlock::reject`]); an accept drops it permanently (see
        // [`AvmBlock::accept`]).
        {
            let mut pool = shared.mempool.lock();
            for tx in block.txs() {
                pool.remove(&tx.id());
            }
        }

        self.wrap(block).map_err(VmError::from)
    }

    async fn get_block(&self, _token: &CancellationToken, id: Id) -> VmResult<Arc<dyn VmBlock>> {
        let shared = self.shared().map_err(VmError::from)?;
        let mgr = shared.manager.lock();

        // A processing (verified-but-unaccepted) block is held in the manager's
        // cache; an accepted block is read back from the persisted block store.
        let bytes = if let Some(b) = mgr.processing_block_bytes(id) {
            b
        } else {
            mgr.state().get_block(id).map_err(VmError::from)?
        };
        drop(mgr);

        let block = Block::parse(Codec(), &bytes).map_err(|e| VmError::from(Error::from(e)))?;
        self.wrap(block).map_err(VmError::from)
    }

    async fn parse_block(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn VmBlock>> {
        self.shared().map_err(VmError::from)?;
        let block = Block::parse(Codec(), bytes).map_err(|e| VmError::from(Error::from(e)))?;
        self.wrap(block).map_err(VmError::from)
    }

    async fn set_preference(&mut self, _token: &CancellationToken, id: Id) -> VmResult<()> {
        self.preferred = id;
        Ok(())
    }

    async fn last_accepted(&self, _token: &CancellationToken) -> VmResult<Id> {
        let shared = self.shared().map_err(VmError::from)?;
        Ok(shared.manager.lock().last_accepted())
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashSet;

    use ava_database::MemDb;
    use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferOutput};
    use ava_types::short_id::ShortId;
    use ava_vm::app_sender::SendConfig;

    use super::*;
    use crate::txs::components::{AvaxBaseTx, Output};
    use crate::txs::{BaseTx, FxCredential, UnsignedTx};

    const NETWORK_ID: u32 = 10;

    fn chain_id() -> Id {
        Id::from([0x05; 32])
    }

    #[derive(Debug, Default)]
    struct NoopAppSender;

    #[async_trait]
    impl AppSender for NoopAppSender {
        async fn send_app_request(
            &self,
            _t: &CancellationToken,
            _n: &HashSet<NodeId>,
            _r: u32,
            _b: Vec<u8>,
        ) -> VmResult<()> {
            Ok(())
        }
        async fn send_app_response(
            &self,
            _t: &CancellationToken,
            _n: NodeId,
            _r: u32,
            _b: Vec<u8>,
        ) -> VmResult<()> {
            Ok(())
        }
        async fn send_app_error(
            &self,
            _t: &CancellationToken,
            _n: NodeId,
            _r: u32,
            _c: i32,
            _m: &str,
        ) -> VmResult<()> {
            Ok(())
        }
        async fn send_app_gossip(
            &self,
            _t: &CancellationToken,
            _c: SendConfig,
            _b: Vec<u8>,
        ) -> VmResult<()> {
            Ok(())
        }
    }

    fn chain_ctx() -> Arc<ChainContext> {
        Arc::new(ChainContext {
            network_id: NETWORK_ID,
            subnet_id: Id::EMPTY,
            chain_id: chain_id(),
            node_id: NodeId::default(),
            public_key: None,
            network_upgrades: ava_version::upgrade::get_config(1),
            x_chain_id: chain_id(),
            c_chain_id: Id::EMPTY,
            avax_asset_id: Id::EMPTY,
            chain_data_dir: std::path::PathBuf::new(),
        })
    }

    fn genesis_bytes() -> Vec<u8> {
        let mut out = vec![0x07; 32];
        out.extend_from_slice(&1_000_000u64.to_be_bytes());
        out
    }

    async fn init_vm() -> AvmVm {
        let mut vm = AvmVm::new();
        let token = CancellationToken::new();
        let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        vm.initialize(
            &token,
            chain_ctx(),
            db,
            &genesis_bytes(),
            b"",
            b"",
            Vec::new(),
            Arc::new(NoopAppSender),
        )
        .await
        .expect("initialize");
        vm
    }

    /// A well-formed, initialized gossip tx (no inputs ⇒ never conflicts).
    fn gossip_tx(tag: u32) -> Tx {
        let base = BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![crate::txs::components::TransferableOutput {
                asset_id: Id::EMPTY,
                out: Output::SecpTransfer(TransferOutput::new(
                    0,
                    OutputOwners::new(0, 1, vec![ShortId::from([0xab; 20])]),
                )),
            }],
            ins: vec![],
            memo: tag.to_be_bytes().to_vec(),
        });
        let mut tx = Tx::new(UnsignedTx::Base(base));
        tx.creds = vec![FxCredential::new(Id::EMPTY, SecpCredential::new(vec![]))];
        tx.initialize(Codec()).expect("initialize gossip tx");
        tx
    }

    #[tokio::test]
    async fn genesis_is_last_accepted_at_height_0() {
        let vm = init_vm().await;
        let token = CancellationToken::new();
        let last = vm.last_accepted(&token).await.expect("last_accepted");
        let at0 = vm
            .get_block_id_at_height(&token, 0)
            .await
            .expect("height 0");
        assert_eq!(last, at0);
        assert_eq!(vm.genesis_id, last);
        let blk = vm.get_block(&token, last).await.expect("get genesis");
        assert_eq!(blk.height(), 0);
    }

    #[tokio::test]
    async fn app_gossip_admits_tx_to_mempool() {
        let mut vm = init_vm().await;
        let token = CancellationToken::new();

        let tx = gossip_tx(42);
        let msg = TxMarshaller::new().marshal(&tx);

        // The mempool starts empty.
        assert!(
            vm.shared().unwrap().mempool.lock().is_empty(),
            "mempool starts empty"
        );

        vm.app_gossip(&token, NodeId::default(), &msg)
            .await
            .expect("app_gossip");

        // The gossiped tx was admitted via the atomic switch → live handler.
        let pool = vm.shared().unwrap().mempool.lock();
        assert_eq!(pool.len(), 1, "gossiped tx admitted to mempool");
        assert!(pool.contains(&tx.id()));
    }

    #[tokio::test]
    async fn app_gossip_drops_malformed() {
        let mut vm = init_vm().await;
        let token = CancellationToken::new();
        // Garbage bytes do not unmarshal to a Tx → dropped, mempool untouched.
        vm.app_gossip(&token, NodeId::default(), &[0xff, 0x00, 0x13])
            .await
            .expect("app_gossip");
        assert!(vm.shared().unwrap().mempool.lock().is_empty());
    }

    #[tokio::test]
    async fn set_state_normal_op_flips_dispatch_bootstrapped() {
        let mut vm = init_vm().await;
        let token = CancellationToken::new();
        // Cycle through the engine phases; NormalOp flips bootstrapped.
        vm.set_state(&token, EngineState::Bootstrapping)
            .await
            .expect("bootstrapping");
        vm.set_state(&token, EngineState::NormalOp)
            .await
            .expect("normal op");
        assert_eq!(vm.state, EngineState::NormalOp);
        // The manager's backend now reports bootstrapped.
        assert!(vm.shared().unwrap().manager.lock().backend().bootstrapped);
    }

    #[tokio::test]
    async fn unknown_block_and_height_not_found() {
        let vm = init_vm().await;
        let token = CancellationToken::new();
        assert!(matches!(
            vm.get_block(&token, Id::from([0xAB; 32])).await,
            Err(VmError::NotFound)
        ));
        assert!(matches!(
            vm.get_block_id_at_height(&token, 99_999).await,
            Err(VmError::NotFound)
        ));
    }

    #[tokio::test]
    async fn uninitialized_vm_errors() {
        let vm = AvmVm::new();
        let token = CancellationToken::new();
        assert!(vm.last_accepted(&token).await.is_err());
    }

    /// M8.22 end-to-end: `create_handlers` mounts the gorilla `avm.*` service
    /// at extension `""` (Go `vm.go:314-317`, minus the out-of-scope
    /// `"/wallet"` keystore mount) and serves `avm.getHeight` /
    /// `avm.issueTx` through the in-process `HttpHandler` seam — issueTx
    /// lands the tx in the live shared mempool via the gossip admission path.
    #[tokio::test]
    #[allow(clippy::indexing_slicing)] // Value indexing yields Null, not a panic
    async fn create_handlers_serves_avm_service() {
        use ava_crypto::hashing::checksum;
        use ava_vm::vm::VmRequest;

        let token = CancellationToken::new();
        let mut vm = init_vm().await;
        let handlers = vm.create_handlers(&token).await.expect("create_handlers");
        assert_eq!(handlers.len(), 1, "the root extension only (no /wallet)");
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
            "method": "avm.getHeight",
            "params": [{}],
            "id": 1,
        }))
        .await;
        assert_eq!(
            body["result"]["height"], "0",
            "avm.getHeight serves the genesis height"
        );

        // issueTx: checksummed-hex wire bytes (Go formatting.Hex) of a
        // well-formed tx land it in the live shared mempool.
        let tx = gossip_tx(7);
        let mut wire = tx.bytes().to_vec();
        let cs = checksum(&wire, 4);
        wire.extend_from_slice(&cs);
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "avm.issueTx",
            "params": [{ "tx": format!("0x{}", hex::encode(&wire)), "encoding": "hex" }],
            "id": 2,
        }))
        .await;
        assert_eq!(
            body["result"]["txID"],
            tx.id().to_string(),
            "avm.issueTx echoes the parsed txID"
        );
        assert!(
            vm.shared().unwrap().mempool.lock().contains(&tx.id()),
            "issueTx admits the tx to the live mempool (gossip admission path)"
        );

        // Re-issuing the same tx is the duplicate drop (Go mempool error).
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "avm.issueTx",
            "params": [{ "tx": format!("0x{}", hex::encode(&wire)), "encoding": "hex" }],
            "id": 3,
        }))
        .await;
        assert_eq!(body["error"]["code"], -32000, "duplicate is a server error");
        assert_eq!(body["error"]["message"], "duplicate tx");
    }
}
