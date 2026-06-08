// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `EvmVm` — the C-Chain Snowman [`ChainVm`] adapter (spec 10 §3; 07 §2
//! ChainVm/Block), reth-as-a-library (NOT the Engine API): Snowman owns fork
//! choice, acceptance is **linear**, and there are **no reorgs** (G6).
//!
//! ## What this adapter wires
//!
//! [`EvmVm`] bundles the M6.3/M6.4 [`FirewoodStateProvider`] (the EVM
//! state-of-record), the M6.6 [`AvaEvmConfig`] (the bare reth `BlockExecutor`
//! driver), the M6.9 [`CanonicalStore`] (the non-state block-metadata "blocks
//! db", G6), and the M6.16 [`AtomicMempool`] behind the engine-facing [`Vm`] /
//! [`ChainVm`] traits, handing the engine [`ava_snow::Block`]s.
//!
//! ## The engine-facing block wrapper ([`VerifiedEvmBlock`])
//!
//! [`ava_snow::Block`] is the async decidable trait (`id`/`parent`/`height`/
//! `timestamp`/`bytes` + `verify`/`accept`/`reject`) with **no VM handle** on the
//! decision methods. The M6.9 [`EvmBlock`] lifecycle, by contrast, takes an
//! [`EvmBlockContext`] (provider + config + canonical store) and a parent root
//! explicitly. M6.9 deliberately left the trait `impl` to this task because the
//! lifecycle needs those collaborators: [`VerifiedEvmBlock`] bundles an
//! [`EvmBlock`] with a shared [`EvmBlockContext`] (an `Arc` clone of the VM's
//! collaborators) plus a back-reference to the VM's shared core so `verify`
//! resolves the parent state root, stashes the pre-commit root, and on
//! `accept`/`reject` advances/discards — promoting the block into / evicting it
//! from the shared `verified` tree and advancing `last_accepted`.
//!
//! ## Linear-accept block resolution (G6)
//!
//! `verify` inserts the block into `verified` (a [`DashMap`], the processing-block
//! tree); `accept` advances the committed Firewood tip + the [`CanonicalStore`]
//! tip + the `last_accepted` pointer, leaving the block in `verified` so it stays
//! resolvable by `get_block` after acceptance. `get_block` resolves the
//! `verified` tree first, else confirms the id is a known accepted block via the
//! [`CanonicalStore`] index. **Full-byte reconstruction of an accepted block from
//! the store** (rebuilding the `EvmBlock` from persisted header+body bytes) needs
//! the reth-db history schema and is deferred to the RPC/history task
//! (M6.23/M6.24); until then `get_block` for an accepted-but-evicted id returns
//! [`Error::NotFound`] — see the inline note.
//!
//! ## `build_block` (M6.20)
//!
//! [`ChainVm::build_block`] drives the on-demand [`BlockBuilderDriver`] (§4/§17.6,
//! G5) on the preferred leaf: it resolves the preferred block's coreth header +
//! committed Firewood state root from the processing tree, builds the next-block
//! [`AvaNextBlockCtx`], and calls [`BlockBuilderDriver::build_on`], which pulls
//! one atomic batch + EVM txs under the gas / `blockGasCost` budget, computes the
//! Firewood pre-commit root (stashed for commit-on-accept, the G5/G1 precomputed-
//! root trick), and assembles the byte-exact coreth block whose `header.state_root`
//! is that root — so the self-built block re-verifies to the identical root.
//! When there is nothing to issue (no work, or the min-retry-delay guard), it
//! returns [`Error::NotFound`] — coreth's `ErrNoPendingBlock` shape.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_evm_reth::B256;
use ava_snow::{ChainContext, EngineState};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{Block as VmBlock, ChainVm};
use ava_vm::connector::Connector;
use ava_vm::error::{Error as VmError, Result as VmResult};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{HttpHandler, Vm, VmEvent};

use crate::atomic::mempool::AtomicMempool;
use crate::block::{EvmBlock, EvmBlockContext, decode_ava_evm_block};
use crate::builder::BlockBuilderDriver;
use crate::canonical::CanonicalStore;
use crate::error::Error;
use crate::evmconfig::{AvaEvmConfig, AvaNextBlockCtx};
use crate::state::FirewoodStateProvider;

/// Maps a `B256` block hash to a consensus [`Id`]. The C-Chain block ID is
/// `keccak256(header RLP)` (spec 10 §9.3); the 32 bytes are identical, so the
/// mapping is a pure reinterpretation.
fn id_of(hash: B256) -> Id {
    Id::from(<[u8; 32]>::from(hash))
}

/// Maps a consensus [`Id`] back to a `B256` block hash (inverse of [`id_of`]).
fn hash_of(id: Id) -> B256 {
    B256::from(id.to_bytes())
}

/// A C-Chain block presented to the consensus engine: an [`EvmBlock`] bundled
/// with the shared lifecycle [`EvmBlockContext`] and a back-reference to the VM's
/// shared core, so the `&self`-only [`ava_snow::Block`] decision methods can drive
/// `verify`/`accept`/`reject` with no extra arguments (spec 10 §3.1).
struct VerifiedEvmBlock {
    /// The decoded / built block (the M6.9 lifecycle subject).
    block: EvmBlock,
    /// `keccak256(header RLP)` -> consensus id (cached).
    id: Id,
    /// The parent's consensus id (cached).
    parent: Id,
    /// The shared lifecycle collaborators (provider + config + canonical store).
    ctx: Arc<EvmBlockContext>,
    /// The shared VM core (the processing-block tree + `last_accepted` pointer).
    shared: Arc<Shared>,
}

#[async_trait]
impl VmBlock for VerifiedEvmBlock {
    fn id(&self) -> Id {
        self.id
    }

    fn parent(&self) -> Id {
        self.parent
    }

    fn height(&self) -> u64 {
        self.block.number()
    }

    fn timestamp(&self) -> SystemTime {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(self.block.header().time))
            .unwrap_or(UNIX_EPOCH)
    }

    fn bytes(&self) -> &[u8] {
        self.block.encoded_bytes()
    }

    async fn verify(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        // The parent must be the committed tip (linear acceptance, G6): resolve
        // its state root from the Firewood provider's current tip.
        let parent_root = self.shared.parent_state_root(self.parent)?;
        let precommit = self
            .block
            .verify(&self.ctx, parent_root)
            .map_err(ava_snow::Error::from)?;
        // Promote into the processing-block tree, recording the pre-commit root
        // so accept/reject can commit/discard exactly it.
        self.shared.verified.insert(
            self.id,
            ProcessingBlock {
                block: self.block.clone(),
                precommit_root: precommit,
            },
        );
        Ok(())
    }

    async fn accept(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        let precommit = self
            .shared
            .verified
            .get(&self.id)
            .map(|e| e.precommit_root)
            .ok_or_else(|| ava_snow::Error::from(Error::MissingProposal(hash_of(self.id))))?;
        self.block
            .accept(&self.ctx, precommit)
            .map_err(ava_snow::Error::from)?;
        // Advance the committed tip pointer. The block stays in `verified` so it
        // remains resolvable by `get_block` after acceptance (full-byte
        // reconstruction from the canonical store is M6.23/M6.24).
        self.shared
            .last_accepted
            .store(Arc::new((self.id, self.block.number())));
        Ok(())
    }

    async fn reject(&self, _token: &CancellationToken) -> ava_snow::Result<()> {
        // Drop the stashed proposal (if any) and evict from the processing tree.
        if let Some((_, pb)) = self.shared.verified.remove(&self.id) {
            self.block
                .reject(&self.ctx, pb.precommit_root)
                .map_err(ava_snow::Error::from)?;
        }
        Ok(())
    }
}

/// A processing (verified-but-not-yet-decided) block held in the [`Shared`]
/// `verified` tree: the decoded block (so `get_block` can re-wrap it) plus the
/// stashed pre-commit root `accept` commits / `reject` discards.
struct ProcessingBlock {
    block: EvmBlock,
    /// The Firewood pre-commit root `verify` stashed (== `header.state_root`).
    precommit_root: B256,
}

/// The mutable core shared between the [`EvmVm`] and every [`VerifiedEvmBlock`]
/// it hands out (spec 10 §3 / §17.2.2).
struct Shared {
    /// The Firewood state-of-record provider (the committed EVM tip).
    state: Arc<FirewoodStateProvider>,
    /// The non-state block-metadata "blocks db" (G6).
    blocks: Arc<CanonicalStore>,
    /// The processing-block tree: `id -> verified-but-undecided block`
    /// (Go `chain.State` / coreth `blockChain` processing set).
    verified: DashMap<Id, ProcessingBlock>,
    /// The committed last-accepted `(id, height)` (Go `vm.lastAcceptedBlock`),
    /// seeded from the canonical tip on init.
    last_accepted: ArcSwap<(Id, u64)>,
}

impl Shared {
    /// The committed Firewood state root for `parent` (the parent of the block
    /// being verified). Under linear acceptance the parent IS the committed tip,
    /// so this returns the provider's current committed root regardless of which
    /// id is asked (a sibling-on-tip verify resolves the same parent state, 04
    /// §4.2). Errors only if the parent is neither the committed tip nor a known
    /// processing block.
    fn parent_state_root(&self, parent: Id) -> ava_snow::Result<B256> {
        // Parent is the committed tip (the common linear case).
        if self.is_committed_tip(parent) {
            return Ok(self.state.root());
        }
        // Parent is a processing block (verify-then-build-child before accept):
        // its pre-commit root is the state the child executes against.
        if let Some(pb) = self.verified.get(&parent) {
            return Ok(pb.precommit_root);
        }
        Err(ava_snow::Error::from(Error::MissingProposal(hash_of(
            parent,
        ))))
    }

    /// Whether `id` is the committed last-accepted tip.
    fn is_committed_tip(&self, id: Id) -> bool {
        self.last_accepted.load().0 == id
    }
}

/// `evm.VM` — the C-Chain Snowman VM (spec 10 §3). Drives reth/revm as a library
/// executor over Firewood, with Snowman owning fork choice (G6).
pub struct EvmVm {
    /// The shared mutable core (state + blocks db + processing tree + tip).
    shared: Arc<Shared>,
    /// The EVM config (the bare reth `BlockExecutor` driver), cloned into each
    /// block's [`EvmBlockContext`].
    evm_config: AvaEvmConfig,
    /// The atomic X<->C mempool (M6.16); the builder driver (M6.20) drains it.
    /// Held behind a [`parking_lot::Mutex`] (the engine drives the VM as one
    /// actor, so contention is structural, not concurrent).
    txpool: Arc<parking_lot::Mutex<AtomicMempool>>,
    /// The on-demand block builder (M6.20, §4/§17.6). Drives the same executor
    /// over the preferred-block parent view + the atomic mempool batch, and
    /// produces a block whose `header.state_root` is the precomputed Firewood
    /// root (G5/G1). `build_block` resolves the preferred parent and calls it.
    builder: BlockBuilderDriver,
    /// The currently preferred (leaf) block id (Go `vm.preferred`). Record-only:
    /// Snowman owns fork choice, so `set_preference` does no reorg work (G6).
    preferred: ArcSwap<Id>,
    /// The immutable chain identity/handles received at `initialize`.
    ctx: Option<Arc<ChainContext>>,
    /// The current engine phase (Go `vm.bootstrapped`).
    engine_state: EngineState,
}

impl EvmVm {
    /// Builds an `EvmVm` over its pre-built collaborators, seeding the
    /// last-accepted / preferred pointers from the committed tip.
    ///
    /// This is the seam the (M6.8-completing) [`Vm::initialize`] uses once genesis
    /// parsing wires the provider/config/store; until then it is the test +
    /// node-bootstrap constructor. `genesis_id` identifies the committed genesis
    /// block (height 0); it seeds the tip when the canonical store is empty (a
    /// fresh chain), and is superseded by the persisted canonical tip on re-open.
    #[must_use]
    pub fn new(
        state: Arc<FirewoodStateProvider>,
        evm_config: AvaEvmConfig,
        blocks: Arc<CanonicalStore>,
        genesis_id: Id,
    ) -> Self {
        // Seed the committed tip from the canonical store if it has accepted
        // blocks (re-open), else from the supplied genesis (height 0).
        let tip = match blocks.last_canonical() {
            Ok(Some(height)) => match blocks.canonical_hash(height) {
                Ok(Some(hash)) => (id_of(hash), height),
                _ => (genesis_id, 0),
            },
            _ => (genesis_id, 0),
        };
        let avax_asset_id = Id::EMPTY;
        let shared = Arc::new(Shared {
            state: Arc::clone(&state),
            blocks,
            verified: DashMap::new(),
            last_accepted: ArcSwap::from_pointee(tip),
        });
        let txpool = Arc::new(parking_lot::Mutex::new(AtomicMempool::new(
            4096,
            avax_asset_id,
        )));
        let builder = BlockBuilderDriver::new(evm_config.clone(), state, Arc::clone(&txpool));
        Self {
            shared,
            evm_config,
            txpool,
            builder,
            preferred: ArcSwap::from_pointee(tip.0),
            ctx: None,
            engine_state: EngineState::Initializing,
        }
    }

    /// The current committed Firewood state root (test/inspection helper).
    #[must_use]
    pub fn state_root(&self) -> B256 {
        self.shared.state.root()
    }

    /// The currently preferred (leaf) block id (test/inspection helper).
    #[must_use]
    pub fn preferred(&self) -> Id {
        *self.preferred.load_full()
    }

    /// Wraps an [`EvmBlock`] as the engine-facing [`ava_snow::Block`], cloning the
    /// shared collaborators into the block's lifecycle context.
    fn wrap(&self, block: EvmBlock) -> Arc<dyn VmBlock> {
        let id = id_of(block.hash());
        let parent = id_of(*block.parent_hash());
        let ctx = Arc::new(EvmBlockContext::new(
            Arc::clone(&self.shared.state),
            self.evm_config.clone(),
            Arc::clone(&self.shared.blocks),
        ));
        Arc::new(VerifiedEvmBlock {
            block,
            id,
            parent,
            ctx,
            shared: Arc::clone(&self.shared),
        })
    }
}

// ---------------------------------------------------------------------------
// Vm supertraits (app / health / connector) — minimal, mirroring the avm/pchain
// precedent. Inbound app messages are atomic-tx gossip (M6.16/M6.19 wire the
// live handler); here they are accepted as no-ops until that lands.
// ---------------------------------------------------------------------------

#[async_trait]
impl AppHandler for EvmVm {
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
impl HealthCheck for EvmVm {
    async fn health_check(&self, _token: &CancellationToken) -> VmResult<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl Connector for EvmVm {
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
impl Vm for EvmVm {
    async fn initialize(
        &mut self,
        _token: &CancellationToken,
        chain_ctx: Arc<ChainContext>,
        _db: Arc<dyn DynDatabase>,
        _genesis_bytes: &[u8],
        _upgrade_bytes: &[u8],
        _config_bytes: &[u8],
        _fxs: Vec<Fx>,
        _app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        // Genesis-JSON parsing (alloc seeding + the upgrade schedule) is M6.8, and
        // it builds the provider/config/store collaborators this VM is constructed
        // over. Until that wiring lands, `EvmVm::new` is the construction seam
        // (the node bootstrap / tests supply the collaborators); `initialize` only
        // records the immutable chain context here.
        self.ctx = Some(chain_ctx);
        self.engine_state = EngineState::Initializing;
        Ok(())
    }

    async fn set_state(&mut self, _token: &CancellationToken, state: EngineState) -> VmResult<()> {
        self.engine_state = state;
        Ok(())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> VmResult<()> {
        // Idempotent: clearing the processing tree releases the held proposals.
        self.shared.verified.clear();
        Ok(())
    }

    async fn version(&self, _token: &CancellationToken) -> VmResult<String> {
        // TODO(M8): source from ava-version instead of the hard-coded string.
        Ok("evm/0.0.0".to_string())
    }

    async fn create_handlers(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<HashMap<String, HttpHandler>> {
        // The eth_*/avax.* JSON-RPC services are M6.23/M6.24.
        Ok(HashMap::new())
    }

    async fn new_http_handler(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        Ok(None)
    }

    async fn wait_for_event(&self, token: &CancellationToken) -> VmResult<VmEvent> {
        // Report PendingTxs when the atomic mempool is non-empty; otherwise block
        // until cancellation. `VmEvent` has no cancellation variant, so on
        // shutdown we return `PendingTxs` (the engine re-checks emptiness and
        // re-parks — harmless when tearing down). Matches the avm precedent.
        let pending = !self.txpool.lock().is_empty();
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
impl ChainVm for EvmVm {
    async fn build_block(&mut self, _token: &CancellationToken) -> VmResult<Arc<dyn VmBlock>> {
        // On-demand build (M6.20, §4/§17.6): build on the preferred leaf. We need
        // the preferred block's coreth header (the build target's parent) and the
        // committed Firewood state root it executes against. The preferred id is a
        // verified-or-accepted block in the processing tree (`set_preference`
        // records it; accepted blocks are retained there for resolvability). When
        // the preferred id is not in the tree (e.g. a freshly-seeded genesis whose
        // `AvaHeader` the VM never decoded), there is nothing to build on yet —
        // return the "no pending block" shape (coreth `ErrNoPendingBlock`).
        let preferred = *self.preferred.load_full();
        let Some(parent) = self.shared.verified.get(&preferred) else {
            return Err(VmError::NotFound);
        };
        let parent_header = parent.block.header().clone();
        // The parent's own post-state root is the Firewood revision the child
        // executes against (== the committed tip once the parent is accepted).
        let parent_state_root = parent.precommit_root;
        drop(parent);

        // The next-block build/fee context (§17.3). The wall-clock timestamp is
        // the build time; the fee state defaults to the genesis/first-AP3 window
        // (the parent-extra fee-state extraction is M6.7's follow-up — see the
        // build report). The atomic gas budget is the post-AP5 limit the mempool
        // packs against.
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .max(parent_header.time.saturating_add(1));
        let ctx = AvaNextBlockCtx {
            timestamp: now_secs,
            ..AvaNextBlockCtx::with_atomic_gas_limit(100_000)
        };

        // The reth-txpool `best_transactions` integration is M6.23; until then the
        // VM contributes no EVM txs (atomic-only blocks build from the mempool).
        match self
            .builder
            .build_on(&parent_header, parent_state_root, &ctx, Vec::new())
        {
            Ok(block) => Ok(self.wrap(block)),
            // "Nothing to build" / min-retry-delay guard -> no pending block.
            Err(Error::MissingProposal(_)) => Err(VmError::NotFound),
            Err(e) => Err(VmError::from(e)),
        }
    }

    async fn get_block(&self, _token: &CancellationToken, id: Id) -> VmResult<Arc<dyn VmBlock>> {
        // 1) The processing / verified tree (verified-but-undecided + accepted,
        //    which we retain after accept for resolvability).
        if let Some(pb) = self.shared.verified.get(&id) {
            return Ok(self.wrap(pb.block.clone()));
        }
        // 2) An accepted block evicted from `verified`. Full EvmBlock byte
        //    reconstruction from the persisted header+body needs the reth-db
        //    history schema (M6.23/M6.24); the canonical store currently keeps only
        //    the header commitment + ext_data, not the full block RLP. Until then,
        //    such an id is reported NotFound rather than fabricating a partial
        //    block. (Accepted blocks are retained in `verified`, so this branch is
        //    only reached for a genuinely unknown id today.)
        Err(VmError::NotFound)
    }

    async fn parse_block(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn VmBlock>> {
        let block =
            decode_ava_evm_block(bytes, self.evm_config.chain_spec()).map_err(VmError::from)?;
        Ok(self.wrap(block))
    }

    async fn set_preference(&mut self, _token: &CancellationToken, id: Id) -> VmResult<()> {
        // Record-only (G6): Snowman owns fork choice, so there is NO reorg work.
        // We record the preferred leaf and wake the txpool's builder subscribers
        // so the next `build_block` targets the new preference (no state mutation).
        self.preferred.store(Arc::new(id));
        self.txpool.lock().subscribe().notify_one();
        Ok(())
    }

    async fn last_accepted(&self, _token: &CancellationToken) -> VmResult<Id> {
        Ok(self.shared.last_accepted.load().0)
    }

    async fn get_block_id_at_height(
        &self,
        _token: &CancellationToken,
        height: u64,
    ) -> VmResult<Id> {
        match self.shared.blocks.canonical_hash(height) {
            Ok(Some(hash)) => Ok(id_of(hash)),
            _ => Err(VmError::NotFound),
        }
    }
}
