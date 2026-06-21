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
use ava_evm_reth::{B256, Bytes, Header, RethBlock, RlpDecodable, SealedBlock};
use ava_saevm_blocks::Block;
use ava_saevm_core::{BlockBuilderSeam, BuildError, ExecutorSeam, SaeBlock, Vm as CoreVm};
use ava_saevm_hook::{BlockBuilder, PointsG, Settled};
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::SharedMemory;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::api::AvaxService;
use crate::gossip::{BloomSet, PULL_GOSSIP_PERIOD, PUSH_GOSSIP_PERIOD};
use crate::hooks::{AtomicOp, AtomicOpSource, CChainHooks, Error as HooksError};
use crate::state::State;
use crate::txpool::AtomicTxpool;

/// Configuration for the cross-chain tx gossip loops (Go `cchain/vm.go`'s
/// `pushGossipPeriod`/`pullGossipPeriod`). Tests pass shorter periods (Go uses
/// 100 ms); production uses the [`PUSH_GOSSIP_PERIOD`]/[`PULL_GOSSIP_PERIOD`]
/// defaults.
#[derive(Clone, Copy, Debug)]
pub struct GossipConfig {
    /// How often the push gossiper re-broadcasts the pool (default
    /// [`PUSH_GOSSIP_PERIOD`]).
    pub push_period: std::time::Duration,
    /// How often the pull gossiper reconciles against peers (default
    /// [`PULL_GOSSIP_PERIOD`]).
    pub pull_period: std::time::Duration,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            push_period: PUSH_GOSSIP_PERIOD,
            pull_period: PULL_GOSSIP_PERIOD,
        }
    }
}

/// Errors returned by the C-Chain VM harness.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The genesis SAE block could not be constructed.
    #[error("constructing genesis block: {0}")]
    Genesis(String),
    /// The C-Chain state store could not be opened.
    #[error("opening cchain state: {0}")]
    State(#[from] crate::state::Error),
    /// Parsing the block via the embedded SAE VM failed (Go `vm.VM.ParseBlock`).
    #[error("parsing block: {0}")]
    Parse(String),
    /// The block's `BlockBodyExtra` carries a `Version` other than `0`, the only
    /// supported version (Go `vm.go::errInvalidBlockVersion`, #5543). The header
    /// commits neither the `Version` nor the `extData` bytes (only the
    /// `ExtDataHash`), so a block with a tampered `Version` keeps the same ID —
    /// `parse_block` is the boundary that rejects it (specs/11 §8, M7.39).
    #[error("invalid block version: {0}")]
    InvalidBlockVersion(u32),
    /// Decoding the C-Chain block's trailing `BlockBodyExtra` (`Version` +
    /// `extData`) RLP items failed.
    #[error("decoding extData: {0}")]
    ExtDataRlp(String),
    /// The block's `extData` body does not hash to the `ExtDataHash` committed
    /// in its header (Go `vm.go::errExtDataHashMismatch`). The block ID commits
    /// the hash, so a tampered `extData` body keeps the same ID — this is the
    /// boundary that rejects it (specs/11 §8, M7.37).
    #[error("extData hash mismatch: header committed {claimed}, extData hashes to {actual:#x}")]
    ExtDataHashMismatch {
        /// The hash committed in the header's `extra_data` (hex, `0x`-prefixed).
        claimed: String,
        /// The hash recomputed from the block's `extData` body.
        actual: B256,
    },
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
    /// The cross-chain tx gossip set (bloom stand-in over the txpool). `issueTx`
    /// admits through it; the push/pull loops gossip from it (Go `vm.gossipSet`).
    gossip_set: Arc<BloomSet>,
    /// The `avax` JSON-RPC service mounted at `/avax`.
    avax: AvaxService,
    /// Cancels the spawned gossip loops on [`Vm::shutdown`] (Go `onClose`).
    gossip_shutdown: CancellationToken,
    /// Tracks the spawned gossip loop tasks so [`Vm::shutdown`] can await them.
    gossip_tasks: TaskTracker,
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
        Self::initialize_with_gossip(
            db,
            shared_memory,
            chain_id,
            avax_asset_id,
            None::<(crate::gossip::NoGossipTransport, GossipConfig)>,
        )
    }

    /// `VM.Initialize` with an explicit [`GossipConfig`] and an optional live
    /// gossip [`Transport`](crate::gossip::GossipTransport).
    ///
    /// When `gossip` is `Some((transport, config))`, the push/pull gossip loops
    /// are spawned (Go's `gossip.Every` goroutines), cancelled by
    /// [`Vm::shutdown`] (Go `onClose`). When `None`, no loops are spawned (the
    /// live `Network` transport is M8; in-process multi-node tests drive the
    /// [`Vm::gossip_set`] via the `ava-saevm-testutil` network harness). Either
    /// way the [`BloomSet`] is constructed and the `avax` service admits through
    /// it.
    ///
    /// # Errors
    /// As [`Vm::initialize`].
    pub fn initialize_with_gossip<T>(
        db: &Arc<dyn DynDatabase>,
        shared_memory: Arc<dyn SharedMemory>,
        chain_id: Id,
        avax_asset_id: Id,
        gossip: Option<(T, GossipConfig)>,
    ) -> Result<Self, Error>
    where
        T: crate::gossip::GossipTransport + Clone,
    {
        // 1. Set up the genesis SAE block (height 0, self-settling). The EVM-state
        //    genesis (Go `core.SetupGenesisBlock`) is owned by ava-evm / M8.
        let genesis = build_genesis().map_err(|e| Error::Genesis(e.to_string()))?;

        // 2. Build the C-Chain hooks (the SAE hook surface = the BlockBuilderSeam).
        //    The hooks' header-build path uses the injected `now` clock (Go
        //    threads `now func() time.Time` into both `newHooks` and
        //    `sae.Config.Now`, PR #5524); here the system wall clock is the
        //    determinism-gated source (specs/00 §6.1, spec/24). The core VM's own
        //    `now` (the parse-time future-block bound) is the same source.
        let hooks = CChainHooks::new(EmptyAtomicSource)
            .with_clock(Arc::new(SystemTime::now) as crate::hooks::Clock);
        let builder = HookBuilderSeam { hooks };

        // 3. Compose the SAE core VM (`sae.NewVM` — composing core::Vm IS the
        //    `sae::Vm` analog, specs/11 §5). The injected clock is the system
        //    clock (the parse-time future-block bound, specs/24).
        let executor = Arc::new(NoopExecutor);
        let core = Arc::new(CoreVm::new(&genesis, builder, executor, SystemTime::now));

        // 4. Open the atomic state + construct the atomic txpool.
        let state = Arc::new(Mutex::new(State::new(Arc::clone(db))?));
        let txpool = Arc::new(AtomicTxpool::new(avax_asset_id));

        // 5. Build the gossip set (bloom stand-in over the txpool — Go
        //    `gossip.NewBloomSet(newGossipTxPool(vm.txpool), ...)`).
        let gossip_set = Arc::new(BloomSet::new(Arc::clone(&txpool)));

        // The avax service admits issued txs through the gossip set + push
        // gossiper (Go `newService(ctx, gossipSet, pushGossiper, state)`).
        let avax = AvaxService::new(
            Arc::clone(&gossip_set),
            Arc::clone(&state),
            shared_memory,
            chain_id,
        );

        // 6. Spawn the push/pull gossip loops when a live transport is supplied
        //    (Go `gossip.Every` goroutines, cancelled via `onClose`).
        let gossip_shutdown = CancellationToken::new();
        let gossip_tasks = TaskTracker::new();
        if let Some((transport, config)) = gossip {
            let push = crate::gossip::PushGossiper::new(
                Arc::clone(&gossip_set),
                transport.clone(),
                config.push_period,
            );
            let pull = crate::gossip::PullGossiper::new(
                Arc::clone(&gossip_set),
                transport,
                config.pull_period,
            );
            let push_cancel = gossip_shutdown.clone();
            gossip_tasks.spawn(async move {
                Self::run_gossip_loop(&push_cancel, config.push_period, || {
                    push.gossip_once();
                })
                .await;
            });
            let pull_cancel = gossip_shutdown.clone();
            gossip_tasks.spawn(async move {
                Self::run_gossip_loop(&pull_cancel, config.pull_period, || {
                    pull.gossip_once();
                })
                .await;
            });
        }
        gossip_tasks.close();

        Ok(Self {
            core,
            state,
            txpool,
            gossip_set,
            avax,
            gossip_shutdown,
            gossip_tasks,
        })
    }

    /// The shared body of a gossip loop: tick at `period`, calling `tick` each
    /// time, until the [`CancellationToken`] fires (Go `gossip.Every`).
    async fn run_gossip_loop<F: FnMut()>(
        cancel: &CancellationToken,
        period: std::time::Duration,
        mut tick: F,
    ) {
        let mut ticker = tokio::time::interval(period);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = ticker.tick() => tick(),
            }
        }
    }

    /// `VM.Shutdown` (Go `onClose`): cancel the gossip loops and await their
    /// completion.
    pub async fn shutdown(&self) {
        self.gossip_shutdown.cancel();
        self.gossip_tasks.wait().await;
    }

    /// The cross-chain tx gossip set (the bloom stand-in over the atomic txpool).
    /// Multi-node tests connect this into the `ava-saevm-testutil` network.
    #[must_use]
    pub fn gossip_set(&self) -> &Arc<BloomSet> {
        &self.gossip_set
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

    /// `VM.ParseBlock` (Go `cchain/vm.go::ParseBlock`, #5447 + #5543): parse
    /// `bytes` via the embedded SAE VM, then perform the C-Chain syntactic checks
    /// the SAE VM is unaware of — that the `BlockBodyExtra` `Version` is `0` (the
    /// only supported version) and that the block's `extData` hashes to the
    /// `ExtDataHash` committed in its header.
    ///
    /// The block ID is the header hash. The header commits neither the `Version`
    /// nor the `extData` bytes (only `ExtDataHash`), so a block whose `Version`
    /// or `extData` was tampered (leaving the header — and ID — unchanged) would
    /// pass the base SAE `ParseBlock` (which is unaware of the C-Chain `extData`
    /// concept). This override rejects a non-zero `Version`
    /// ([`Error::InvalidBlockVersion`]) and recomputes
    /// [`calc_ext_data_hash`](crate::block_ext::calc_ext_data_hash) over the
    /// block's `extData`, rejecting a mismatch.
    ///
    /// The `BlockBodyExtra` rides as the trailing RLP items `[Version, extData]`
    /// appended after the bare SAE eth block (approach (B), M7.37/M7.39 — the SAE
    /// core stays a stock alloy block; the C-Chain layer owns the carrier, with
    /// `Version` before `extData` matching Go's `[Header, Txs, Uncles, Version,
    /// ExtData]` block-RLP field order). A bare block (no trailing items) decodes
    /// to `Version = 0` and empty `extData`. A block carrying no `ExtDataHash`
    /// commitment (empty header `extra_data`) skips the hash check — Go's pre-AP1
    /// `TODO` analog, and the dormant state until the build path commits
    /// `ExtDataHash` (the remainder of M7.22, coupled to the M7.21 C-Chain
    /// builder + atomic source).
    ///
    /// # Errors
    /// [`Error::Parse`] if the embedded SAE VM rejects the bytes;
    /// [`Error::InvalidBlockVersion`] if the `BlockBodyExtra` `Version` is not `0`;
    /// [`Error::ExtDataRlp`] if the trailing `BlockBodyExtra` items are malformed;
    /// [`Error::ExtDataHashMismatch`] if the committed hash and the recomputed
    /// `extData` hash differ.
    pub fn parse_block(&self, bytes: &[u8]) -> Result<SaeBlock, Error> {
        let handle = self
            .core
            .parse(bytes)
            .map_err(|e| Error::Parse(e.to_string()))?;

        // The C-Chain syntactic checks the SAE VM is unaware of (Go
        // `vm.go::ParseBlock`): the `BlockBodyExtra` `Version` must be 0, and the
        // `extData` must hash to the header's committed `ExtDataHash`. Decode the
        // trailing `[Version, extData]` items once and check `Version` first —
        // unconditionally, matching Go: the header commits neither, so a tampered
        // `Version` keeps the same block ID.
        let (version, ext_data) = decode_trailing_body_extra(bytes)?;
        if version != 0 {
            return Err(Error::InvalidBlockVersion(version));
        }

        // The committed ExtDataHash lives in the header's `extra_data`. An empty
        // commitment marks a block that does not commit an ExtDataHash (Go's
        // pre-AP1 TODO); accept it unchecked.
        let claimed = &handle.block().eth_block().header().extra_data;
        if claimed.is_empty() {
            return Ok(handle);
        }

        let actual = crate::block_ext::calc_ext_data_hash(&ext_data);
        if claimed.as_ref() != actual.as_slice() {
            return Err(Error::ExtDataHashMismatch {
                claimed: claimed.to_string(),
                actual,
            });
        }
        Ok(handle)
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

/// Decodes the trailing `BlockBodyExtra` RLP items — `[Version: u32, extData:
/// bytes]` — that the C-Chain appends after the bare SAE eth block in the wire
/// bytes (Go's `Version`/`ExtData` block fields, carried out-of-band here —
/// approach (B), M7.37/M7.39). Returns `(0, empty)` when the block carries no
/// trailing items (a bare SAE block, matching Go's `BlockVersion`/`BlockExtData`
/// defaults of `0`/`nil` for a block with no `BlockBodyExtra`). `Version`
/// precedes `extData`, mirroring Go's `[Header, Txs, Uncles, Version, ExtData]`
/// block-RLP field order.
///
/// `RethBlock::decode` consumes exactly the eth block and leaves the slice
/// pointing at any trailing bytes, so the SAE core's own `parse_block` (which
/// ignores the trailing items) and this decoder agree on the boundary.
fn decode_trailing_body_extra(bytes: &[u8]) -> Result<(u32, Vec<u8>), Error> {
    let mut slice = bytes;
    // Advance past the eth block; the SAE core already validated it.
    let _eth = RethBlock::decode(&mut slice).map_err(|e| Error::Parse(e.to_string()))?;
    if slice.is_empty() {
        return Ok((0, Vec::new()));
    }
    let version = u32::decode(&mut slice).map_err(|e| Error::ExtDataRlp(e.to_string()))?;
    // `extData` follows `Version`; tolerate its absence (treat as empty) so a
    // version-only carrier still decodes, matching Go's nilable `ExtData`.
    let ext_data = if slice.is_empty() {
        Vec::new()
    } else {
        Bytes::decode(&mut slice)
            .map_err(|e| Error::ExtDataRlp(e.to_string()))?
            .to_vec()
    };
    Ok((version, ext_data))
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
    // Mirror Go's `MarkSynchronous(hooks, ...)`, which seeds the post-block gas
    // clock from `hooks.GasConfigAfter(header)`. The C-Chain hook returns a fixed
    // (1_000_000 target, default ACP-176 config) — see `CChainHooks::gas_config_after`.
    genesis.mark_synchronous((
        crate::hooks::GAS_CONFIG_AFTER_TARGET,
        ava_saevm_gastime::GasPriceConfig::default(),
    ))?;
    Ok(genesis)
}
