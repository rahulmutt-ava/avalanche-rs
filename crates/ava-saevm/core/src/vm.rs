// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The SAE core VM lifecycle: `build_block` / `verify_block` / `accept_block` /
//! `set_preference` and the block-store reads (specs/11 §1.2/§1.3, §5; specs/27
//! §2.4/§5.4).
//!
//! Faithful port of `vms/saevm/sae/{blocks,consensus}.go`
//! (`BuildBlock`, `VerifyBlock`, `verifyWhenBootstrapping`, `AcceptBlock`,
//! `SetPreference`, `GetBlock`, `GetBlockIDAtHeight`, `LastAccepted`).
//!
//! # What this delivers vs. what is deferred
//!
//! `Initialize` on a [`Vm::new`]-constructed VM is a **no-op success** — the VM
//! is already genesis-rooted at construction, so the standard consensus boot
//! pipeline (`ava_chains::create_snowman_chain`) can drive it. The genesis-*bytes*
//! → VM materialization path (parsing `genesis_bytes`/`db`/`config` into a fresh
//! VM via the production seams) is still **deferred to the cchain harness**
//! (M7.21/M7.26/M7.23 — specs/11 §5); everything else in the consensus lifecycle
//! is implemented. The C-Chain hook *bodies*
//! (M7.21) and the executor reactor loop (M7.26) are reached through two
//! object-safe seams ([`BlockBuilderSeam`], [`ExecutorSeam`]) so the real wiring
//! lands later without touching this lifecycle. The VM uses interior mutability
//! (`&self` on every lifecycle method, `ArcSwap`/`RwLock` fields) so it composes
//! with the [`adaptor`](ava_saevm_adaptor) wrapper.
//!
//! # The lifecycle (Go parity)
//!
//! * **`build_block`** — builds on the current preference via [`BlockBuilderSeam::build_on`]
//!   (which internally runs the txgossip-priority → worst-case-predict → hook
//!   build pipeline; M7.21). The builder populates `GasLimit`/`BaseFee`/`GasUsed`
//!   from the worst-case prediction and `Root` from the settled ancestor's
//!   post-exec state root (specs/11 §1.3). Cited: `sae/blocks.go::BuildBlock`.
//! * **`verify_block`** — fetches the parent, rejects already-accepted heights,
//!   then (`NormalOp`) **rebuilds** the block from `parent` + the builder and
//!   compares hashes (cheap, **no execution**); on match, populates ancestry and
//!   stores the block consensus-critical. During **bootstrapping** the rebuild is
//!   skipped (peers verify by hash) — `verify_when_bootstrapping`. Cited:
//!   `sae/blocks.go::{VerifyBlock,verifyWhenBootstrapping}`.
//! * **`accept_block`** — in strict **D→M→I→X** order (specs/27 §2.4): persist
//!   (modelled by the store insert here), mark the settlement set `Σ` settled in
//!   increasing height via [`settle`](crate::settle()), advance `LastAccepted`,
//!   enqueue to the executor; and, during **bootstrapping only**, block on
//!   `wait_until_executed` so the engine's accept-in-a-loop cannot outrun the
//!   executor and FATAL. Cited: `sae/consensus.go::AcceptBlock`.
//! * **`set_preference`** — store the preferred (leaf) block. Cited:
//!   `sae/consensus.go::SetPreference`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_evm_reth::B256;
use ava_saevm_adaptor::ChainVm;
use ava_saevm_blocks::Block;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;
use ava_vm::{
    AppError, AppHandler, AppSender, BlockContext, ChainContext, Connector, EngineState, Fx,
    HealthCheck, HttpHandler, Result as VmResult, Vm as BaseVm, VmEvent,
};

use crate::block_handle::{SaeBlock, id_from_hash};
use crate::frontier::Frontier;
use crate::settle::{SettleError, settle};

// ---------------------------------------------------------------------------
// Seam errors + traits (the hook builder + executor seams; M7.21/M7.26)
// ---------------------------------------------------------------------------

/// A failure of a VM seam ([`BlockBuilderSeam`] / [`ExecutorSeam`]).
///
/// Kept deliberately small: the real hook/executor errors carry richer context
/// (M7.21/M7.26); at this layer the lifecycle only needs to map them onto
/// [`ava_vm::Error`].
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The block builder (hook) failed to build/rebuild a block.
    #[error("building block: {0}")]
    Builder(String),
    /// The executor seam failed to enqueue a block.
    #[error("enqueuing block: {0}")]
    Executor(String),
}

/// The block-builder seam: the hook-driven block construction pipeline (specs/11
/// §1.3, Go `block_builder.go::{build,rebuild}`).
///
/// The real impl (M7.21) runs txgossip-priority selection → worst-case
/// prediction (`ava-saevm-worstcase`) → the C-Chain hook `BlockBuilder::build_block`,
/// populating `GasLimit`/`BaseFee`/`GasUsed` from the prediction and `Root` from
/// the settled ancestor's post-exec state root, attaching the [`WorstCaseBounds`]
/// via [`Block::set_worst_case_bounds`]. Both `build_on` and `rebuild` MUST be
/// deterministic so a faithfully re-broadcast block rebuilds to an identical hash
/// (the `verify_block` invariant).
///
/// [`WorstCaseBounds`]: ava_saevm_blocks::WorstCaseBounds
/// [`Block::set_worst_case_bounds`]: ava_saevm_blocks::Block::set_worst_case_bounds
pub trait BlockBuilderSeam: Send + Sync + 'static {
    /// Builds a new block on top of `parent` (the current preference).
    ///
    /// # Errors
    /// [`BuildError::Builder`] if the hook builder cannot construct the block.
    fn build_on(&self, parent: &Arc<Block>) -> std::result::Result<Arc<Block>, BuildError>;

    /// Rebuilds a block from `parent` (Go `BlockRebuilderFrom`), reconstructing
    /// the candidate `b` for the verify-by-rebuild + hash-compare check. The
    /// returned block MUST be byte-identical to `b` when `b` is valid.
    ///
    /// # Errors
    /// [`BuildError::Builder`] if the hook rebuilder cannot reconstruct the block.
    fn rebuild(
        &self,
        parent: &Arc<Block>,
        b: &Arc<Block>,
    ) -> std::result::Result<Arc<Block>, BuildError>;
}

/// The executor seam: accept-time enqueue into the single-task streaming
/// executor (specs/11 §6.1, Go `saexec.Executor.Enqueue`).
///
/// The real impl (M7.26) wraps `ava-saevm-exec`'s `Executor` and its bounded
/// `mpsc` `processQueue` loop; the block becomes executed asynchronously and any
/// [`Block::wait_until_executed`](ava_saevm_blocks::Block::wait_until_executed)
/// waiter wakes once it commits.
pub trait ExecutorSeam: Send + Sync + 'static {
    /// Enqueues `block` for execution. Returns once the block is queued (not
    /// once it has executed).
    ///
    /// # Errors
    /// [`BuildError::Executor`] if the block cannot be enqueued (e.g. the queue
    /// is full / the executor is shutting down).
    fn enqueue(&self, block: &Arc<Block>) -> std::result::Result<(), BuildError>;
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

/// A lifecycle error surfaced to the consensus engine. Carries a human-readable
/// message that the [`adaptor`](ava_saevm_adaptor) maps onto the `ava_snow`
/// boundary; `ava_vm::Error` has no free-form variant, so verification failures
/// (hash mismatch, unknown parent, …) are signalled via the message-carrying
/// [`Error`] returned by the inherent methods and mapped to
/// [`ava_vm::Error::NotFound`] only for genuine lookup misses.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A block/height lookup missed (maps to `ava_vm::Error::NotFound`).
    #[error("not found")]
    NotFound,
    /// The block's declared parent is not known to the VM.
    #[error("unknown parent {0:#x}")]
    UnknownParent(B256),
    /// The block's height is at or below the last-accepted height.
    #[error("block height {height} too low (<= last accepted {accepted})")]
    HeightTooLow {
        /// The candidate block's height.
        height: u64,
        /// The current last-accepted height.
        accepted: u64,
    },
    /// The rebuilt block's hash did not match the candidate's.
    #[error("hash mismatch; rebuilt as {rebuilt:#x} when verifying {verifying:#x}")]
    HashMismatch {
        /// The hash of the locally-rebuilt block.
        rebuilt: B256,
        /// The hash of the candidate block under verification.
        verifying: B256,
    },
    /// A seam (builder / executor) failed.
    #[error(transparent)]
    Seam(#[from] BuildError),
    /// Settlement failed (e.g. an ancestor could not be marked settled).
    #[error("settling: {0}")]
    Settle(String),
    /// Reserved for the deferred genesis-*bytes* → VM materialization path
    /// (parsing `genesis_bytes`/`db`/`config` into a fresh VM via the production
    /// builder/executor seams), owned by the cchain harness (M7.21/M7.26/M7.23,
    /// specs/11 §5). `initialize` on a [`Vm::new`]-constructed VM is a no-op
    /// success (it is already genesis-rooted), so this is not currently returned.
    #[error("initialize from genesis bytes is deferred to the cchain harness (M7.23)")]
    InitializeDeferred,
    /// A lifecycle invariant in `ava-saevm-blocks` was violated.
    #[error("block lifecycle: {0}")]
    Lifecycle(String),
}

impl From<Error> for ava_vm::Error {
    fn from(_e: Error) -> Self {
        // `ava_vm::Error` is a fixed sentinel set with no free-form variant, so
        // every lifecycle failure (lookup miss, hash mismatch, settle error, …)
        // degrades to `NotFound` when surfaced through the base `ava_vm::Result`.
        // The [`adaptor`](ava_saevm_adaptor) is the parity-preserving path: it
        // maps the message-carrying `ava_snow` error at the block boundary, so
        // verify/accept failures keep their human-readable form there.
        ava_vm::Error::NotFound
    }
}

/// The lifecycle result alias.
pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// VM
// ---------------------------------------------------------------------------

/// The SAE core VM (specs/11 §5).
///
/// Generic over the block-builder seam `B` (the hook pipeline, M7.21) and the
/// executor seam `E` (M7.26). Holds the three-frontier consensus state
/// ([`Frontier`]), the in-memory block store (hash → handle, the verified /
/// accepted blocks consensus MAY request), the canonical height → hash index,
/// the engine-phase pointer, and the preference pointer — all behind interior
/// mutability so every lifecycle method takes `&self`.
pub struct Vm<B: BlockBuilderSeam, E: ExecutorSeam> {
    /// The three monotonic frontiers (S/E/A) + the consensus-critical `[S,A]` map.
    frontier: Frontier,
    /// Every block consensus may request by id (verified-but-not-accepted +
    /// accepted), keyed by hash. The accepted subset is also indexed by height.
    blocks: RwLock<HashMap<B256, SaeBlock>>,
    /// Canonical (accepted) height → block hash.
    height_index: RwLock<HashMap<u64, B256>>,
    /// The engine phase (`Bootstrapping` gates verify-skip + accept-blocking).
    consensus_state: ArcSwap<EngineState>,
    /// The current preferred (leaf) block — the parent for `build_block`.
    preference: ArcSwap<Block>,
    /// The hook-driven block builder seam (M7.21).
    builder: B,
    /// The executor seam (M7.26).
    executor: Arc<E>,
    /// Injected wall-clock for the future-block bound at parse time (specs/24).
    now: fn() -> SystemTime,
}

impl<B: BlockBuilderSeam, E: ExecutorSeam> Vm<B, E> {
    /// SAE always verifies with the proposervm context (specs/11 §5;
    /// `ShouldVerifyWithContext == true`).
    pub const SHOULD_VERIFY_WITH_CONTEXT: bool = true;

    /// Constructs a VM rooted at `genesis` (the synchronous / last pre-SAE
    /// block), which starts as the sole accepted/executed/settled block and the
    /// initial preference.
    #[must_use]
    pub fn new(
        genesis: &Arc<Block>,
        builder: B,
        executor: Arc<E>,
        now: fn() -> SystemTime,
    ) -> Self {
        let genesis_handle = SaeBlock::new(Arc::clone(genesis));
        let mut blocks = HashMap::new();
        blocks.insert(genesis.hash(), genesis_handle);
        let mut height_index = HashMap::new();
        height_index.insert(genesis.height(), genesis.hash());

        let frontier = Frontier::new(Arc::clone(genesis));
        let preference = ArcSwap::from(Arc::clone(genesis));

        Self {
            frontier,
            blocks: RwLock::new(blocks),
            height_index: RwLock::new(height_index),
            consensus_state: ArcSwap::from_pointee(EngineState::Initializing),
            preference,
            builder,
            executor,
            now,
        }
    }

    /// Sets the engine phase (the harness calls this via `set_state`; exposed
    /// directly for tests / the M7.23 harness).
    pub fn set_state(&self, state: EngineState) {
        self.consensus_state.store(Arc::new(state));
    }

    /// The current engine phase.
    #[must_use]
    pub fn consensus_state(&self) -> EngineState {
        *self.consensus_state.load_full()
    }

    /// The current wall-clock instant (the injected clock).
    #[must_use]
    pub fn now(&self) -> SystemTime {
        (self.now)()
    }

    /// The three frontiers (read-only handle for the RPC label mapping / tests).
    #[must_use]
    pub fn frontier(&self) -> &Frontier {
        &self.frontier
    }

    // -- block store -------------------------------------------------------

    /// Looks up a stored block handle by hash.
    fn lookup(&self, hash: B256) -> Option<SaeBlock> {
        self.blocks.read().get(&hash).cloned()
    }

    /// Inserts (or replaces) a block handle in the store.
    fn store(&self, handle: SaeBlock) {
        self.blocks.write().insert(handle.block().hash(), handle);
    }

    // -- inherent lifecycle (the adaptor::ChainVm impl forwards here) -------

    /// Builds a new block on the current preference (Go `BuildBlock`).
    ///
    /// `_ctx` is the proposervm context when present; the worst-case prediction
    /// and hook build run inside [`BlockBuilderSeam::build_on`] (M7.21). The
    /// freshly built block is stored so a subsequent `verify`/`block_by_id` can
    /// find it.
    ///
    /// # Errors
    /// [`Error::Seam`] if the hook builder fails.
    pub fn build(&self, _ctx: Option<&BlockContext>) -> Result<SaeBlock> {
        let parent = self.preference.load_full();
        let block = self.builder.build_on(&parent)?;
        let handle = SaeBlock::new(block);
        self.store(handle.clone());
        Ok(handle)
    }

    /// Verifies a block: cheap rebuild-and-compare (no execution) in `NormalOp`,
    /// skip-and-by-hash during bootstrapping (Go `VerifyBlock` /
    /// `verifyWhenBootstrapping`).
    ///
    /// # Errors
    /// [`Error::UnknownParent`] if the parent is not stored;
    /// [`Error::HeightTooLow`] if the block is at/below the accepted height;
    /// [`Error::HashMismatch`] if the local rebuild disagrees; [`Error::Seam`] if
    /// the rebuilder fails.
    pub fn verify(&self, _ctx: Option<&BlockContext>, b: &SaeBlock) -> Result<()> {
        let block = b.block();
        let parent_hash = block.parent_hash();
        let parent = self
            .lookup(parent_hash)
            .ok_or(Error::UnknownParent(parent_hash))?;

        // Sanity: never verify an already-accepted block.
        let accepted_height = self.frontier.last_accepted().height();
        if block.height() <= accepted_height {
            return Err(Error::HeightTooLow {
                height: block.height(),
                accepted: accepted_height,
            });
        }

        if self.consensus_state() == EngineState::Bootstrapping {
            // Bootstrapping: peers verify by hash; skip the rebuild entirely and
            // just populate ancestry from the (already-trusted) parent +
            // last-settled, then store consensus-critical. (Go
            // `verifyWhenBootstrapping`'s settled-root/height sanity checks need
            // the hook `SettledBy` + `lastToSettle`, wired in M7.21; the ancestry
            // population + consensus-critical store are the load-bearing steps
            // the bootstrap-accept loop depends on.)
            let last_settled = parent.block().last_settled();
            block
                .set_ancestors(Some(Arc::clone(parent.block())), last_settled)
                .map_err(|e| Error::Lifecycle(e.to_string()))?;
            self.store(b.clone());
            return Ok(());
        }

        // NormalOp: rebuild from parent + builder and compare hashes (cheap, NO
        // execution). On match, populate ancestry and store consensus-critical.
        let rebuilt = self.builder.rebuild(parent.block(), block)?;
        if rebuilt.hash() != block.hash() {
            return Err(Error::HashMismatch {
                rebuilt: rebuilt.hash(),
                verifying: block.hash(),
            });
        }
        // CopyAncestorsFrom: adopt the rebuilt block's ancestry + worst-case
        // bounds. The rebuilt block carries parent + last_settled; copy them onto
        // the verified handle's block.
        let last_settled = rebuilt.last_settled();
        block
            .set_ancestors(Some(Arc::clone(parent.block())), last_settled)
            .map_err(|e| Error::Lifecycle(e.to_string()))?;
        self.store(b.clone());
        Ok(())
    }

    /// Accepts a block (Go `AcceptBlock`): in strict **D→M→I→X** order, persist
    /// (store), settle `Σ` in increasing height, advance `LastAccepted`, index
    /// the canonical height, and enqueue to the executor; during bootstrapping
    /// also block on `wait_until_executed`.
    ///
    /// # Errors
    /// [`Error::Settle`] if settlement fails for a reason other than a benign
    /// execution lag; [`Error::Seam`] if the enqueue fails.
    pub async fn accept(&self, b: &SaeBlock) -> Result<()> {
        let block = b.block();

        // D — persist (modelled by the store insert + canonical-height index;
        // the rawdb batch write is M7.21). Done first so a successful return is a
        // durable-write guarantee (specs/27 §2.4 CC-ORDER).
        self.store(b.clone());
        self.height_index
            .write()
            .insert(block.height(), block.hash());

        // M+I — mark the settlement set Σ settled in increasing height (the
        // `settle` driver advances `LastSettled` + evicts below-S blocks from the
        // consensus-critical map). A benign `ExecutionLagging` is not fatal here:
        // the block is accepted regardless; settlement retries as execution
        // catches up (Go retries on the next accept).
        match settle(&self.frontier, block) {
            Ok(_) | Err(SettleError::ExecutionLagging) => {}
            Err(other) => return Err(Error::Settle(other.to_string())),
        }

        // I(b ∈ A) — advance LastAccepted (after the settled pointers, matching
        // Go's `s ∈ S` before `b ∈ A`).
        self.frontier.advance_accepted(block);

        // X — enqueue to the executor (the external signal).
        self.executor.enqueue(block)?;

        // Bootstrapping: block until the executor has executed this block, so the
        // engine's accept-in-a-loop cannot outrun execution and FATAL (specs/27
        // §5.4, Go `AcceptBlock`'s `WaitUntilExecuted`).
        if self.consensus_state() == EngineState::Bootstrapping {
            block.wait_until_executed().await;
        }

        Ok(())
    }

    /// Rejects a block (Go `RejectBlock`): a no-op beyond dropping it from the
    /// consensus-critical map — SAE executes only after acceptance.
    pub fn reject(&self, b: &SaeBlock) {
        // The block was never executed (execution only follows acceptance), so
        // rejection just removes it from the store / consensus-critical window.
        self.blocks.write().remove(&b.block().hash());
    }

    /// Sets the preferred (leaf) block (Go `SetPreference`).
    ///
    /// # Errors
    /// [`Error::NotFound`] if `id` is not a known block.
    pub fn set_preferred(&self, id: Id) -> Result<()> {
        let hash = B256::from(*id.as_bytes());
        let handle = self.lookup(hash).ok_or(Error::NotFound)?;
        self.preference.store(Arc::clone(handle.block()));
        Ok(())
    }

    /// Returns the stored block with `id` (Go `GetBlock`).
    ///
    /// # Errors
    /// [`Error::NotFound`] if no such block is stored.
    pub fn block_by_id(&self, id: Id) -> Result<SaeBlock> {
        let hash = B256::from(*id.as_bytes());
        self.lookup(hash).ok_or(Error::NotFound)
    }

    /// Parses an RLP block, returning a handle that caches the wire bytes (Go
    /// `ParseBlock`). Ancestry is populated on verify, not here.
    ///
    /// # Errors
    /// [`Error::Lifecycle`] on malformed RLP / a future-dated block.
    pub fn parse(&self, bytes: &[u8]) -> Result<SaeBlock> {
        let block = ava_saevm_blocks::parse_block(bytes, self.now())
            .map_err(|e| Error::Lifecycle(e.to_string()))?;
        let handle = SaeBlock::with_bytes(Arc::new(block), Arc::new(bytes.to_vec()));
        Ok(handle)
    }

    /// The id of the last accepted block (Go `LastAccepted`).
    #[must_use]
    pub fn last_accepted_id(&self) -> Id {
        id_from_hash(self.frontier.last_accepted().hash())
    }

    /// The canonical (accepted) block id at `height` (Go `GetBlockIDAtHeight`).
    ///
    /// # Errors
    /// [`Error::NotFound`] if the height is not (yet) indexed.
    pub fn block_id_at_height(&self, height: u64) -> Result<Id> {
        self.height_index
            .read()
            .get(&height)
            .map(|h| id_from_hash(*h))
            .ok_or(Error::NotFound)
    }
}

// ---------------------------------------------------------------------------
// Base ava_vm::Vm (Initialize deferred to M7.23; the rest are minimal)
// ---------------------------------------------------------------------------

#[async_trait]
impl<B: BlockBuilderSeam, E: ExecutorSeam> AppHandler for Vm<B, E> {
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
        _node: NodeId,
        _msg: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }
}

#[async_trait]
impl<B: BlockBuilderSeam, E: ExecutorSeam> HealthCheck for Vm<B, E> {
    async fn health_check(&self, _token: &CancellationToken) -> VmResult<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl<B: BlockBuilderSeam, E: ExecutorSeam> Connector for Vm<B, E> {
    async fn connected(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _version: Application,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn disconnected(&mut self, _token: &CancellationToken, _node: NodeId) -> VmResult<()> {
        Ok(())
    }
}

#[async_trait]
impl<B: BlockBuilderSeam, E: ExecutorSeam> BaseVm for Vm<B, E> {
    #[allow(clippy::too_many_arguments)]
    async fn initialize(
        &mut self,
        _token: &CancellationToken,
        _chain_ctx: Arc<ChainContext>,
        _db: Arc<dyn DynDatabase>,
        _genesis_bytes: &[u8],
        _upgrade_bytes: &[u8],
        _config_bytes: &[u8],
        _fxs: Vec<Fx>,
        _app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        // A `Vm` constructed via [`Vm::new`] is *already* initialized: genesis is
        // seeded into the block store / height index, and the frontier +
        // preference are rooted at it (see `Vm::new`). So when such an
        // in-process VM is driven through the standard consensus boot pipeline
        // (`ava_chains::create_snowman_chain`, which calls `initialize` then
        // reads `last_accepted`/`get_block`), `initialize` is a no-op success —
        // the genesis root the pipeline immediately queries is already present.
        //
        // The genesis-*bytes* → VM materialization path (parsing
        // `genesis_bytes`/`db`/`config` into a fresh VM, i.e. the production
        // builder/executor seams) is the deferred cchain-harness work
        // (M7.21/M7.26/M7.23, specs/11 §5); a from-bytes VM is not constructed
        // here. `_genesis_bytes`/`_db`/`_config_bytes` are therefore ignored:
        // the VM owns its genesis from construction.
        Ok(())
    }

    async fn set_state(&mut self, _token: &CancellationToken, state: EngineState) -> VmResult<()> {
        Vm::set_state(self, state);
        Ok(())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> VmResult<()> {
        Ok(())
    }

    async fn version(&self, _token: &CancellationToken) -> VmResult<String> {
        Ok(concat!("ava-saevm-core/", env!("CARGO_PKG_VERSION")).to_string())
    }

    async fn create_handlers(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<HashMap<String, HttpHandler>> {
        // The sae-rpc handlers land in M7.19.
        Ok(HashMap::new())
    }

    async fn new_http_handler(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        Ok(None)
    }

    async fn wait_for_event(&self, _token: &CancellationToken) -> VmResult<VmEvent> {
        // The txgossip/mempool-driven pending-txs notification is M7.21; the
        // event stream is established at initialize (M7.23).
        Ok(VmEvent::PendingTxs)
    }
}

// ---------------------------------------------------------------------------
// SAE adaptor::ChainVm (forwards to the inherent methods)
// ---------------------------------------------------------------------------

#[async_trait]
impl<B: BlockBuilderSeam, E: ExecutorSeam> ChainVm<SaeBlock> for Vm<B, E> {
    async fn get_block(&self, id: Id) -> VmResult<SaeBlock> {
        Ok(self.block_by_id(id)?)
    }

    async fn parse_block(&self, bytes: &[u8]) -> VmResult<SaeBlock> {
        Ok(self.parse(bytes)?)
    }

    async fn build_block(&self, ctx: Option<&BlockContext>) -> VmResult<SaeBlock> {
        Ok(self.build(ctx)?)
    }

    async fn verify_block(&self, ctx: Option<&BlockContext>, b: &SaeBlock) -> VmResult<()> {
        Ok(self.verify(ctx, b)?)
    }

    async fn accept_block(&self, b: &SaeBlock) -> VmResult<()> {
        Ok(self.accept(b).await?)
    }

    async fn reject_block(&self, b: &SaeBlock) -> VmResult<()> {
        self.reject(b);
        Ok(())
    }

    async fn set_preference(&self, id: Id, _ctx: Option<&BlockContext>) -> VmResult<()> {
        Ok(self.set_preferred(id)?)
    }

    async fn last_accepted(&self) -> VmResult<Id> {
        Ok(self.last_accepted_id())
    }

    async fn get_block_id_at_height(&self, h: u64) -> VmResult<Id> {
        Ok(self.block_id_at_height(h)?)
    }
}
