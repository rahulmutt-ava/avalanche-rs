// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The C-Chain VM `Initialize` harness: composes [`ava_saevm_core::Vm`] (the
//! `sae::Vm` analog) with the C-Chain pieces (specs/11 §8 — `cchain/vm.go`;
//! §5 — the harness supplies the `Initialize` that `sae::Vm` omits).
//!
//! Faithful port of `vms/saevm/cchain/vm.go`'s `VM.Initialize`: it
//!
//! 1. sets up the genesis block (Go `core.SetupGenesisBlock` over the eth DB);
//! 2. builds the C-Chain [`hooks`](crate::hooks) ([`CChainHooks`]);
//! 3. composes the SAE core VM (`sae.NewVM` — here [`ava_saevm_core::Vm::new`],
//!    since composing the core VM *is* the `sae::Vm` analog — specs/11 §5);
//! 4. constructs the atomic [`AtomicTxpool`](crate::txpool::AtomicTxpool);
//! 5. reports the genesis block as last-accepted.
//!
//! and [`Vm::create_handlers`] mounts the [`avax`](crate::api) JSON-RPC service
//! at the `/avax` extension path alongside the SAE EVM RPC (Go
//! `vm.go::CreateHandlers`).
//!
//! # AS-BUILT deviations (specs/11 §5 harness precedent)
//!
//! Following the M7.8/M7.31 precedent, the Rust harness composes the pieces that
//! exist rather than replicating Go's `snow.Context`/`AppSender`/metrics/p2p
//! machinery verbatim:
//!
//! * **EVM genesis / eth DB.** Go runs `core.SetupGenesisBlock` over a libevm
//!   `rawdb`; the SAE-Rust C-Chain stores EVM state in Firewood and the eth
//!   genesis-DB setup is owned by `ava-evm` (M6) / the node assembly (M8). The
//!   harness here builds the genesis **SAE block** (the height-0 self-settling
//!   block the core VM is rooted at) directly from an empty eth genesis block;
//!   the EVM-state genesis is a `// TODO(M8)` follow-up.
//! * **Executor.** The streaming executor + its bounded `processQueue` loop is
//!   M7.26; the harness wires a [`NoopExecutor`] seam (enqueue is a no-op) so the
//!   lifecycle composes. `// TODO(M7.26)`.
//! * **Gossip / p2p.** Go's `gossip.NewBloomSet`/`NewSystem` + `AppSender` push
//!   loop have no Rust analog yet (the `txgossip` crate, M7.20, is the seam); the
//!   harness holds the [`AtomicTxpool`] directly and the `/avax` `issueTx`
//!   admits into it. `// TODO(M7.x)` for the bloom-gossip wiring.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use ava_database::DynDatabase;
use ava_evm_reth::{Header, RethBlock, SealedBlock};
use ava_saevm_blocks::Block;
use ava_saevm_core::{BlockBuilderSeam, BuildError, ExecutorSeam, Vm as CoreVm};
use ava_saevm_hook::{BlockBuilder, PointsG, Settled};
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::SharedMemory;
use parking_lot::Mutex;

use crate::api::AvaxService;
use crate::hooks::{AtomicOp, AtomicOpSource, CChainHooks, Error as HooksError};
use crate::state::State;
use crate::txpool::AtomicTxpool;

/// Errors returned by the C-Chain VM harness.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The genesis SAE block could not be constructed.
    #[error("constructing genesis block: {0}")]
    Genesis(String),
    /// The C-Chain state store could not be opened.
    #[error("opening cchain state: {0}")]
    State(#[from] crate::state::Error),
}

/// The lock semantics returned in the `/avax` handler descriptor; the avax
/// service mutates the txpool, so it acquires the write lock (Go mounts gorilla
/// RPC under the VM's default write-lock handler).
const AVAX_LOCK_WRITE: u8 = 0;

// ---------------------------------------------------------------------------
// Builder seam adapter: CChainHooks (PointsG) -> core::BlockBuilderSeam
// ---------------------------------------------------------------------------

/// Adapts the C-Chain [`CChainHooks`] (which implements the SAE
/// [`PointsG`]/[`BlockBuilder`] hook surface) onto the core VM's
/// [`BlockBuilderSeam`] (`build_on`/`rebuild` over [`Block`]).
///
/// `build_on` runs the hook `build_header` → `build_block` pipeline (Go
/// `block_builder.go::build`); `rebuild` runs `block_rebuilder_from` →
/// `build_header`/`build_block` (Go `block_builder.go::rebuild`). The end-of-block
/// atomic ops + EVM txs + worst-case prediction are M7.21/M7.26; here the seam
/// builds the deterministic header-only block so build == rebuild byte-identically
/// (matching the `core` lifecycle's `verify`-by-rebuild invariant).
pub struct HookBuilderSeam<S: AtomicOpSource + Send + Sync + 'static> {
    hooks: CChainHooks<S>,
}

impl<S: AtomicOpSource + Send + Sync + 'static> HookBuilderSeam<S> {
    /// Assembles an `Arc<Block>` on `parent` using the given hook `builder`'s
    /// header + block construction. `parent`'s last-settled block becomes the new
    /// block's last-settled ancestor (the core lifecycle re-derives it on verify).
    fn assemble<B>(builder: &B, parent: &Arc<Block>) -> std::result::Result<Arc<Block>, BuildError>
    where
        B: BlockBuilder<
                AtomicOp,
                Error = HooksError,
                Block = SealedBlock<RethBlock>,
                BlockContext = (),
                EvmTransaction = ava_evm_reth::TransactionSigned,
                Receipt = ava_evm_reth::EthReceipt,
                BlockSource = (),
            >,
    {
        let parent_header = parent.eth_block().clone_sealed_header();
        let header = builder
            .build_header(&parent_header)
            .map_err(|e| BuildError::Builder(e.to_string()))?;
        let settled = Settled {
            height: 0,
            gas_unix: 0,
            gas_numerator: ava_vm::components::gas::Gas(0),
            excess: ava_vm::components::gas::Gas(0),
        };
        let eth = builder
            .build_block(header, &(), &[], &[], &[], settled)
            .map_err(|e| BuildError::Builder(e.to_string()))?;
        let last_settled = parent.last_settled();
        let block = Block::new(eth, Some(Arc::clone(parent)), last_settled)
            .map_err(|e| BuildError::Builder(e.to_string()))?;
        Ok(Arc::new(block))
    }
}

impl<S: AtomicOpSource + Send + Sync + 'static> BlockBuilderSeam for HookBuilderSeam<S> {
    fn build_on(&self, parent: &Arc<Block>) -> std::result::Result<Arc<Block>, BuildError> {
        Self::assemble(&self.hooks, parent)
    }

    fn rebuild(
        &self,
        parent: &Arc<Block>,
        b: &Arc<Block>,
    ) -> std::result::Result<Arc<Block>, BuildError> {
        // BlockRebuilderFrom freezes `b`'s time so the rebuilt header is
        // byte-identical (Go `hooks.BlockRebuilderFrom`).
        let rebuilder = self
            .hooks
            .block_rebuilder_from(b.eth_block())
            .map_err(|e| BuildError::Builder(e.to_string()))?;
        Self::assemble(&rebuilder, parent)
    }
}

// ---------------------------------------------------------------------------
// Executor seam: no-op (M7.26)
// ---------------------------------------------------------------------------

/// A no-op [`ExecutorSeam`]: `enqueue` records nothing. The real streaming
/// executor + bounded `processQueue` loop is M7.26 (`ava-saevm-exec`); the
/// harness composes this placeholder so the core VM lifecycle is wired.
pub struct NoopExecutor;

impl ExecutorSeam for NoopExecutor {
    fn enqueue(&self, _block: &Arc<Block>) -> std::result::Result<(), BuildError> {
        // TODO(M7.26): enqueue into the streaming executor's processQueue.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The atomic-op source: parse the block's atomic txs (M7.22 seam)
// ---------------------------------------------------------------------------

/// An [`AtomicOpSource`] that returns no atomic ops. The real source decodes the
/// block's extData (Go `parseBlockTxs`); the extData codec is wired by the
/// builder's `build_block` in a later task. Empty here keeps build == rebuild.
pub struct EmptyAtomicSource;

impl AtomicOpSource for EmptyAtomicSource {
    fn atomic_ops(&self, _block: &SealedBlock<RethBlock>) -> Vec<AtomicOp> {
        // TODO(M7.22/M7.26): decode the atomic Import/Export txs from extData.
        Vec::new()
    }
}

/// The concrete SAE core VM the C-Chain composes: the [`HookBuilderSeam`] over
/// the C-Chain hooks + the [`NoopExecutor`] placeholder (M7.26).
pub type CChainCoreVm = CoreVm<HookBuilderSeam<EmptyAtomicSource>, NoopExecutor>;

// ---------------------------------------------------------------------------
// The C-Chain VM
// ---------------------------------------------------------------------------

/// The C-Chain VM atop the SAE core VM (specs/11 §8, Go `cchain.VM`).
///
/// Composes [`ava_saevm_core::Vm`] (built by [`Vm::initialize`]) with the
/// cross-chain pieces: the atomic [`State`], the [`AtomicTxpool`], and the
/// [`avax`](crate::api) JSON-RPC service.
pub struct Vm {
    /// The composed SAE core VM (`sae.VM`, created by [`Vm::initialize`]).
    core: Arc<CChainCoreVm>,
    /// The accepted-atomic-tx index + last-applied-height store.
    state: Arc<Mutex<State>>,
    /// The cross-chain (atomic) transaction pool.
    txpool: Arc<AtomicTxpool>,
    /// The `avax` JSON-RPC service mounted at `/avax`.
    avax: AvaxService,
}

impl Vm {
    /// `VM.Initialize` (Go `cchain/vm.go`): set up genesis, build hooks, compose
    /// the SAE core VM, and construct the atomic txpool.
    ///
    /// `db` is the per-chain VM database (shared with `shared_memory` so the
    /// atomic apply commits in one batch — Go `vm_test.go::newSUT`),
    /// `shared_memory` the C-Chain's view onto cross-chain shared memory,
    /// `chain_id` the C-Chain id, and `avax_asset_id` the AVAX asset id the
    /// atomic txpool / ops are denominated in.
    ///
    /// # Errors
    /// [`Error::Genesis`] if the genesis block cannot be constructed, or
    /// [`Error::State`] if the state store cannot be opened.
    pub fn initialize(
        db: &Arc<dyn DynDatabase>,
        shared_memory: Arc<dyn SharedMemory>,
        chain_id: Id,
        avax_asset_id: Id,
    ) -> Result<Self, Error> {
        // 1. Set up the genesis SAE block (height 0, self-settling). The EVM-state
        //    genesis (Go `core.SetupGenesisBlock`) is owned by ava-evm / M8.
        let genesis = build_genesis().map_err(|e| Error::Genesis(e.to_string()))?;

        // 2. Build the C-Chain hooks (the SAE hook surface = the BlockBuilderSeam).
        let hooks = CChainHooks::new(EmptyAtomicSource);
        let builder = HookBuilderSeam { hooks };

        // 3. Compose the SAE core VM (`sae.NewVM` — composing core::Vm IS the
        //    `sae::Vm` analog, specs/11 §5). The injected clock is the system
        //    clock (the parse-time future-block bound, specs/24).
        let executor = Arc::new(NoopExecutor);
        let core = Arc::new(CoreVm::new(&genesis, builder, executor, SystemTime::now));

        // 4. Open the atomic state + construct the atomic txpool.
        let state = Arc::new(Mutex::new(State::new(Arc::clone(db))?));
        let txpool = Arc::new(AtomicTxpool::new(avax_asset_id));

        // The avax service admits issued txs into the atomic pool and reads the
        // accepted-tx index for getAtomicTx.
        let avax = AvaxService::new(
            Arc::clone(&txpool),
            Arc::clone(&state),
            shared_memory,
            chain_id,
        );

        Ok(Self {
            core,
            state,
            txpool,
            avax,
        })
    }

    /// The composed SAE core VM (the `sae.VM` analog).
    #[must_use]
    pub fn core(&self) -> &Arc<CChainCoreVm> {
        &self.core
    }

    /// The cross-chain (atomic) transaction pool.
    #[must_use]
    pub fn atomic_txpool(&self) -> &Arc<AtomicTxpool> {
        &self.txpool
    }

    /// The atomic-tx state store.
    #[must_use]
    pub fn state(&self) -> &Arc<Mutex<State>> {
        &self.state
    }

    /// The `avax` JSON-RPC service.
    #[must_use]
    pub fn avax_service(&self) -> &AvaxService {
        &self.avax
    }

    /// `VM.LastAccepted` — the id of the last-accepted block (genesis at init).
    #[must_use]
    pub fn last_accepted(&self) -> Id {
        self.core.last_accepted_id()
    }

    /// `VM.CreateHandlers` (Go `vm.go::CreateHandlers`): the SAE EVM RPC handlers
    /// augmented with the `avax` service at the [`AVAX_EXTENSION_PATH`]. The SAE
    /// EVM RPC surface (M7.19) mounts under `/rpc`/`/ws`; here we expose the
    /// `/avax` extension descriptor (the in-process service is [`Vm::avax_service`]).
    ///
    /// The handler value is the opaque [`ava_vm::HttpHandler`] descriptor (the
    /// workspace has no in-process HTTP stack — see `ava_vm::HttpHandler`).
    ///
    /// [`AVAX_EXTENSION_PATH`]: crate::api::AVAX_EXTENSION_PATH
    // The `/avax` descriptor is self-independent today (the in-process service is
    // [`Vm::avax_service`]); kept as a `&self` method to mirror Go
    // `VM.CreateHandlers` and so the SAE EVM RPC merge (M7.19) reads VM state.
    #[allow(clippy::unused_self)]
    #[must_use]
    pub fn create_handlers(&self) -> HashMap<String, ava_vm::HttpHandler> {
        let mut m: HashMap<String, ava_vm::HttpHandler> = HashMap::new();
        // The SAE EVM RPC handlers (M7.19) would be merged in here; the harness
        // adds the /avax extension alongside them.
        m.insert(
            crate::api::AVAX_EXTENSION_PATH.to_string(),
            ava_vm::HttpHandler::new(ava_vm::LockOptions::WriteLock, vec![AVAX_LOCK_WRITE]),
        );
        m
    }
}

/// Builds the genesis SAE block: an empty eth block at height 0, marked
/// synchronous (self-settling) — the block the core VM is rooted at.
///
/// Go's `core.SetupGenesisBlock` produces the EVM genesis block; here we build
/// the minimal SAE genesis block (the EVM-state genesis is M8).
fn build_genesis() -> std::result::Result<Arc<Block>, ava_saevm_blocks::Error> {
    // Genesis time 0 is fine for the SAE genesis root; the EVM genesis timestamp
    // (`saeparams.TauSeconds` in Go) is set by M8's real genesis config.
    let header = Header {
        number: 0,
        timestamp: 0,
        ..Header::default()
    };
    let eth = SealedBlock::seal_slow(RethBlock::uncle(header));
    let genesis = Arc::new(Block::new(eth, None, None)?);
    genesis.mark_synchronous()?;
    Ok(genesis)
}
