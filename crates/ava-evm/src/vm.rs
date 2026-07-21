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
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::{ArcSwap, ArcSwapOption};
use async_trait::async_trait;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

use ava_database::{DynDatabase, MemDb};
use ava_evm_reth::{Address, B256, Chain, ConsensusTx, EMPTY_ROOT_HASH};
use ava_p2p::gossip::handler::GossipHandler;
use ava_p2p::gossip::pull::PullGossiper;
use ava_p2p::gossip::push::PushGossiper;
use ava_p2p::gossip::{GossipParams, every};
use ava_p2p::handler::TX_GOSSIP_HANDLER_ID;
use ava_p2p::network::P2pNetwork;
use ava_snow::{ChainContext, EngineState};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, RealClock};
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{Block as VmBlock, ChainVm};
use ava_vm::connector::Connector;
use ava_vm::error::{Error as VmError, Result as VmResult};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{HttpHandler, LockOptions, PendingWorkWaiter, Vm, VmEvent, VmHttpService};

use crate::atomic::mempool::AtomicMempool;
use crate::block::{
    AvaBlockParts, AvaHeader, EvmBlock, EvmBlockContext, assemble_ava_block, decode_ava_evm_block,
};
use crate::builder::BlockBuilderDriver;
use crate::canonical::CanonicalStore;
use crate::chainspec::{AvaChainSpec, CChainGenesis};
use crate::error::Error;
use crate::evmconfig::{AvaEvmConfig, AvaExecCtx, AvaNextBlockCtx};
use crate::gossip::{EthTxGossipSet, EthTxMarshaller, GossipEthTx, VmSenderAccountReader};
use crate::mempool::{AdmissionRules, EvmMempool};
use crate::receipts::AcceptedTxIndex;
use crate::rpc::admin::AdminRpc;
use crate::rpc::avax::{AcceptedAtomicTxIndex, AvaxRpc};
use crate::rpc::eth::EthRpc;
use crate::rpc::service::{
    ADMIN_ENDPOINT, AVAX_ENDPOINT, ETH_RPC_ENDPOINT, ETH_WS_ENDPOINT, EthHttpService,
    admin_service, avax_service,
};
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
        // The parent's coreth header, for the contextual `verifyHeaderGasFields`
        // checks — resolved from the SAME set `parent_state_root` covers, so both
        // reads agree on which parent this block verifies against.
        let parent_header = self.shared.parent_header(self.parent)?;
        // Live reads, mirroring Go's b.vm.clock.Time() / b.vm.bootstrapped.Get()
        // at verify time (wrapped_block.go:359,376).
        let now_ms = {
            let clock = Arc::clone(&self.shared.clock.lock());
            clock
                .now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        };
        let bootstrapped = self.shared.bootstrapped.load(Ordering::Acquire);
        let precommit = self
            .block
            .verify_with_predicates(
                &self.ctx,
                parent_root,
                &parent_header,
                &AvaExecCtx::default(),
                now_ms,
                bootstrapped,
            )
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
        // EVM mempool maintenance (cchain-tx-pipeline task 5): drop the txs this
        // block just consumed, keyed off the txs ACTUALLY IN THE BLOCK (recovered
        // here, not the build-time pool snapshot — a block adopted from a peer was
        // never in our pool, and even a self-built block may have packed a subset).
        // Never-fail posture (mirrors `EvmBlock::accept`'s swallow-and-warn after
        // `append_canonical`, N3): a recovery hiccup here must NOT fail an
        // already-committed block — the sender was already recovered at verify time,
        // so this is defensive.
        match self.block.recover_senders() {
            Ok(recovered) => {
                let included: Vec<(Address, u64, B256)> = recovered
                    .iter()
                    .map(|tx| (tx.signer(), ConsensusTx::nonce(tx.inner()), *tx.hash()))
                    .collect();
                if !included.is_empty() {
                    self.shared.evm_mempool.lock().on_block_accepted(&included);
                }
            }
            Err(e) => tracing::warn!(
                error = %e,
                block_number = self.block.number(),
                "could not recover senders for accepted-block EVM mempool maintenance; \
                 the pool will self-correct on the next admission/build"
            ),
        }
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
    /// The C-Chain EVM mempool (cchain-tx-pipeline task 5). The same `Arc` the
    /// [`EvmVm`] holds; kept here so [`VerifiedEvmBlock::accept`] can run
    /// accept-time pool maintenance ([`EvmMempool::on_block_accepted`]) over the
    /// txs the just-accepted block included, without threading the handle
    /// through every block.
    evm_mempool: Arc<parking_lot::Mutex<EvmMempool>>,
    /// Injectable wall clock (specs/24 hazard #5) — the single live-read
    /// source for build_block's timestamp AND verify's `VerifyTime` future
    /// bound (Go `vm.clock`). Behind a mutex so the builder-style
    /// [`EvmVm::with_clock`] can swap it after `Shared` is assembled; reads
    /// clone the `Arc` (cheap) and never hold the lock across work.
    clock: parking_lot::Mutex<Arc<dyn Clock>>,
    /// Go `vm.bootstrapped` (`utils.Atomic[bool]`): true once `set_state`
    /// enters `NormalOp`. Read live at verify time to gate
    /// `verifyIntrinsicGas` (wrapped_block.go:376) — a wrap-time copy would
    /// be stale for blocks re-verified after bootstrap completes.
    bootstrapped: AtomicBool,
    /// The cchain-tx-gossip p2p network + registered tx-gossip handler
    /// (Task 12; coreth `vm.go:780-833` wiring order). `None` until
    /// `initialize` builds it over the supplied `AppSender`; the
    /// `AppHandler`/`Connector` impls delegate to the loaded value when
    /// present, else no-op with a `tracing::debug!` (an inbound app message
    /// or connect event arriving before `initialize` has nothing VM-side to
    /// route to — mirrors the `ArcSwapOption`-backed late-binding precedent
    /// `ava-network`'s `IpSigner::cached` uses for a similarly built-after-
    /// construction value).
    p2p: ArcSwapOption<P2pNetwork>,
    /// Cancelled by `shutdown` to stop the two `initialize`-spawned gossip
    /// loops (push+regossip cadence, pull cadence). A plain field (not
    /// `Option`-wrapped): created once in `EvmVm::new`, so cancelling it
    /// before `initialize` ever spawns a loop is just as harmless as
    /// cancelling it after — there is simply nothing listening yet.
    gossip_token: CancellationToken,
    /// The [`GossipParams`] `initialize` builds the push/pull gossipers with
    /// (cchain-tx-gossip task 14 test seam). Seeded to [`GossipParams::default`]
    /// by [`EvmVm::new`]; [`EvmVm::with_gossip_params_for_test`] overrides it
    /// (builder-style, mirroring [`EvmVm::with_clock`]) so a test can disable a
    /// node's push cadence (e.g. `push_period: Duration::from_secs(3600)`)
    /// while leaving pull gossip live, to exercise the pull-only reconciliation
    /// path without waiting out the production 100ms push period.
    gossip_params: parking_lot::Mutex<GossipParams>,
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

    /// The parent block's coreth [`AvaHeader`], for the contextual
    /// `verifyHeaderGasFields` checks (coreth `consensus/dummy/consensus.go:125-176`,
    /// [`crate::feerules::verify_header_gas_fields`]).
    ///
    /// Resolves from the `verified` processing tree only — the same effective
    /// coverage as [`Shared::parent_state_root`]: accepted blocks are RETAINED
    /// in `verified` after `accept` (for `get_block` resolvability), so the
    /// committed tip resolves here, as do all still-processing parents (the
    /// verify-then-build-child case). A parent evicted from the tree CANNOT be
    /// reconstructed here — the [`CanonicalStore`] persists only the header
    /// commitment + ext_data, not the full block RLP (the same limitation
    /// `get_block` documents for accepted-but-evicted ids, deferred to
    /// M6.23/M6.24) — so this returns [`Error::MissingProposal`], matching
    /// `parent_state_root`'s and `build_block`'s resolution contract and every
    /// current caller.
    fn parent_header(&self, parent: Id) -> ava_snow::Result<AvaHeader> {
        if let Some(pb) = self.verified.get(&parent) {
            return Ok(pb.block.header().clone());
        }
        Err(ava_snow::Error::from(Error::MissingProposal(hash_of(
            parent,
        ))))
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
    /// Behind an `Arc` so the [`EvmPendingWorkWaiter`] can watch the next
    /// build's parent without holding the VM.
    preferred: Arc<ArcSwap<Id>>,
    /// The accepted-atomic-tx index the `avax.*` handlers read (coreth
    /// `AtomicRepository`). The accept-side writer wiring (recording each
    /// accepted block's atomic txs) is the M8 follow-up noted in
    /// [`AcceptedAtomicTxIndex`]; the VM owns the index so `create_handlers`
    /// and the (future) accept path share one instance.
    accepted_atomic_txs: Arc<AcceptedAtomicTxIndex>,
    /// The accepted-tx receipt index (cchain-tx-pipeline task 3): `EvmBlock`'s
    /// lifecycle context (wired in [`EvmVm::wrap`] via
    /// [`EvmBlockContext::with_accepted_tx_index`]) records each accepted
    /// block's [`crate::receipts::TxReceiptRecord`]s here; Task 4's `eth_*` RPC
    /// handlers read it via [`EvmVm::accepted_tx_index`].
    accepted_tx_index: Arc<AcceptedTxIndex>,
    /// The C-Chain EVM mempool (cchain-tx-pipeline tasks 1/4/5):
    /// `create_handlers` hands this to [`EthRpc::new`] so
    /// `eth_sendRawTransaction` admits into it; `wait_for_event` wakes on its
    /// admission notify and `build_block` drains [`EvmMempool::best_txs`] into
    /// the block (task 5). The same `Arc` lives on [`Shared`] so
    /// [`VerifiedEvmBlock::accept`] can run pool maintenance. Held behind a
    /// [`parking_lot::Mutex`] (the same convention as the atomic `txpool`
    /// above — `EvmMempool::add_local` takes `&mut self`).
    evm_mempool: Arc<parking_lot::Mutex<EvmMempool>>,
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
        // Same capacity as the atomic `txpool` below (4096; coreth
        // legacypool `globalSlots` default order — the exact capacity is not
        // consensus). No admission rules are baked in here — `create_handlers`
        // builds per-call `AdmissionRules` from the VM's configured chain id
        // (task 4 scope). The `Arc` is shared with `Shared` so accept-time pool
        // maintenance can reach it (task 5).
        let evm_mempool = Arc::new(parking_lot::Mutex::new(EvmMempool::new(4096)));
        let shared = Arc::new(Shared {
            state: Arc::clone(&state),
            blocks,
            verified: DashMap::new(),
            last_accepted: ArcSwap::from_pointee(tip),
            evm_mempool: Arc::clone(&evm_mempool),
            clock: parking_lot::Mutex::new(Arc::new(RealClock)),
            bootstrapped: AtomicBool::new(false),
            p2p: ArcSwapOption::from(None),
            gossip_token: CancellationToken::new(),
            gossip_params: parking_lot::Mutex::new(GossipParams::default()),
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
            preferred: Arc::new(ArcSwap::from_pointee(tip.0)),
            accepted_atomic_txs: Arc::new(AcceptedAtomicTxIndex::new()),
            accepted_tx_index: Arc::new(AcceptedTxIndex::new()),
            evm_mempool,
            ctx: None,
            engine_state: EngineState::Initializing,
        }
    }

    /// Builds a fully-initialized `EvmVm` straight from the production C-Chain
    /// genesis JSON — the construction seam the C-Chain boot path
    /// (`run_queued_chains`, M9.15) drives so a solo node can dispatch the
    /// C-Chain to `NormalOp`. This is the M6.8 `golden::cchain_genesis_root`
    /// parse + alloc-materialization path, now wired into VM construction (the
    /// completion of the `Vm::initialize` genesis wiring noted on [`EvmVm::new`]).
    ///
    /// `network_id` selects the C-Chain fork schedule ([`AvaChainSpec::c_chain`]);
    /// the chain id is read from the genesis `config`. `data_dir` is the on-disk
    /// Firewood directory (the chain's `chain_data_dir`). `genesis_bytes` is the
    /// coreth `core.Genesis` JSON the genesis `CreateChainTx` carries (M5.f4).
    /// Returns the VM paired with its genesis block id
    /// (`keccak256(genesis header)`).
    ///
    /// The genesis `alloc` is materialized + committed only on a fresh db (the
    /// Firewood tip is the empty-trie root); on re-open the persisted tip stands.
    /// The block-metadata side stores (canonical / bytecode / block-hashes) are
    /// in-memory here — threading the node's real chain db through this seam is
    /// the deferred real-DB-threading half of the chains milestone (plan/M9.15).
    ///
    /// # Errors
    /// Returns [`Error::GenesisParse`] on non-UTF-8 / malformed genesis JSON or a
    /// bytecode-seed failure, or a provider error from opening Firewood or
    /// materializing the alloc.
    pub fn from_genesis(
        network_id: u32,
        data_dir: impl AsRef<Path>,
        genesis_bytes: &[u8],
    ) -> Result<(Self, Id), Error> {
        let json = std::str::from_utf8(genesis_bytes)
            .map_err(|e| Error::GenesisParse(format!("genesis bytes are not UTF-8: {e}")))?;
        let genesis = CChainGenesis::parse(json)?;

        // Fork schedule from the chosen network; chain id from the genesis config.
        let chain_spec = AvaChainSpec::c_chain(network_id, Chain::from_id(genesis.chain_id()));

        // The genesis state = alloc + precompile activation accounts (warp at
        // Durango when active at the genesis timestamp — network-id=local), and
        // the full bytecode side-store seed list (M9.15 rung 4).
        let (genesis_bundle, genesis_bytecode) =
            genesis.genesis_alloc(chain_spec.network_upgrades());

        // Open the Firewood-ethhash state db at the chain's data dir. The bytecode
        // + block-hash side stores are in-memory (real-DB threading deferred).
        let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        let provider = FirewoodStateProvider::open(data_dir, bytecode, block_hashes)?;

        // Seed contract bytecode (the state root commits only the code_hash; the
        // bytecode lives in the side KV — spec 10 §5.1).
        for (code_hash, code) in &genesis_bytecode {
            provider
                .bytecode_store()
                .put(code_hash.as_slice(), code)
                .map_err(|e| Error::GenesisParse(format!("seed genesis bytecode: {e}")))?;
        }

        // Materialize + commit the genesis state on a fresh db (the empty-trie
        // tip); a re-opened db keeps its persisted tip.
        if provider.root() == EMPTY_ROOT_HASH {
            let root = provider.propose_from_bundle(&genesis_bundle)?;
            provider.commit(root)?;
        }
        let genesis_root = provider.root();
        let genesis_header = genesis.genesis_header(genesis_root, chain_spec.network_upgrades());
        let genesis_id = id_of(genesis_header.hash());
        let config = AvaEvmConfig::new(chain_spec);

        let canonical = Arc::new(CanonicalStore::new(Arc::new(MemDb::new())));
        let vm = Self::new(provider, config, canonical, genesis_id);

        // Seed the accepted genesis block into the processing tree so the engine's
        // bootstrap (`vm.get_block(last_accepted)`, ava-engine
        // `snowman::bootstrap`) resolves the genesis tip — mirroring how `accept`
        // retains accepted blocks in `verified` for resolvability. Genesis has no
        // body/atomic/ext_data; its pre-commit root IS the committed genesis root.
        let genesis_block = assemble_ava_block(
            AvaBlockParts {
                header: genesis_header,
                transactions: Vec::new(),
                atomic_txs: Vec::new(),
                ext_data: Vec::new(),
                version: 0,
            },
            vm.evm_config.chain_spec(),
        )?;
        vm.shared.verified.insert(
            genesis_id,
            ProcessingBlock {
                block: genesis_block,
                precommit_root: genesis_root,
            },
        );
        Ok((vm, genesis_id))
    }

    /// Overrides the injectable wall clock (specs/24 hazard #5). Builder-style
    /// test seam mirroring `ava-platformvm`'s `with_clock`; production keeps the
    /// [`RealClock`] seeded by [`EvmVm::new`].
    #[must_use]
    pub fn with_clock(self, clock: Arc<dyn Clock>) -> Self {
        *self.shared.clock.lock() = clock;
        self
    }

    /// **Test-only seam** (cchain-tx-gossip task 14): overrides the
    /// [`GossipParams`] `initialize` builds the push/pull gossip loops with.
    /// Builder-style, mirroring [`EvmVm::with_clock`]; production keeps
    /// [`GossipParams::default`] seeded by [`EvmVm::new`]. Lets a test disable
    /// push gossip on one node (e.g. `push_period: Duration::from_secs(3600)`)
    /// while leaving pull gossip on its production cadence, to exercise the
    /// pull-only reconciliation path in isolation. Must be called before
    /// [`Vm::initialize`] runs — `initialize` reads the value once when it
    /// builds the gossip system and a later override has no effect.
    #[doc(hidden)]
    #[must_use]
    pub fn with_gossip_params_for_test(self, params: GossipParams) -> Self {
        *self.shared.gossip_params.lock() = params;
        self
    }

    /// The accepted-atomic-tx index shared with the `avax.*` handlers (the
    /// accept-side writer records into it; coreth `AtomicRepository`).
    #[must_use]
    pub fn accepted_atomic_txs(&self) -> Arc<AcceptedAtomicTxIndex> {
        Arc::clone(&self.accepted_atomic_txs)
    }

    /// The accepted-tx receipt index shared with the `eth_*` handlers
    /// (cchain-tx-pipeline task 3; the accept-side writer is wired into every
    /// block's [`EvmBlockContext`] by [`EvmVm::wrap`]).
    #[must_use]
    pub fn accepted_tx_index(&self) -> Arc<AcceptedTxIndex> {
        Arc::clone(&self.accepted_tx_index)
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

    /// The atomic X<->C mempool handle (test/inspection helper). Exposed so tests
    /// can seed an atomic batch and exercise the `build_block` path end-to-end
    /// (there is no public submission seam yet — `app_gossip` is an M8 stub).
    #[doc(hidden)]
    #[must_use]
    pub fn mempool_handle(&self) -> Arc<parking_lot::Mutex<AtomicMempool>> {
        Arc::clone(&self.txpool)
    }

    /// The EVM mempool handle (test/inspection helper; cchain-tx-pipeline
    /// task 4). `create_handlers` hands the same `Arc` to [`EthRpc::new`], so
    /// a test can seed/inspect it exactly like `mempool_handle` does for the
    /// atomic pool.
    #[doc(hidden)]
    #[must_use]
    pub fn evm_mempool_handle(&self) -> Arc<parking_lot::Mutex<EvmMempool>> {
        Arc::clone(&self.evm_mempool)
    }

    /// Test seam: seed an already-decided block into the processing tree under an
    /// explicit consensus `id`, so it resolves as a parent for
    /// [`VerifiedEvmBlock::verify`]'s `parent_state_root` / `parent_header` reads.
    ///
    /// [`EvmVm::from_genesis`] performs the equivalent genesis seeding internally
    /// (so the production boot path never needs this). Tests that construct the VM
    /// via the lower-level [`EvmVm::new`] — which receives only the genesis id, not
    /// a reconstructable block — use this to seed the genesis parent they verify a
    /// child against.
    #[doc(hidden)]
    pub fn seed_verified(&self, id: Id, block: EvmBlock, precommit_root: B256) {
        self.shared.verified.insert(
            id,
            ProcessingBlock {
                block,
                precommit_root,
            },
        );
    }

    /// Wraps an [`EvmBlock`] as the engine-facing [`ava_snow::Block`], cloning the
    /// shared collaborators into the block's lifecycle context.
    fn wrap(&self, block: EvmBlock) -> Arc<dyn VmBlock> {
        let id = id_of(block.hash());
        let parent = id_of(*block.parent_hash());
        let ctx = Arc::new(
            EvmBlockContext::new(
                Arc::clone(&self.shared.state),
                self.evm_config.clone(),
                Arc::clone(&self.shared.blocks),
            )
            .with_accepted_tx_index(Arc::clone(&self.accepted_tx_index)),
        );
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
// Vm supertraits (app / health / connector) — mirroring the avm/pchain
// precedent. Inbound app messages are the cchain-tx-gossip tx-gossip system
// (Task 12): every method delegates to the `Shared::p2p` `P2pNetwork`'s
// inherent `&self` dispatch methods (`network.rs`'s module doc explains why
// `P2pNetwork` exposes those alongside its own `&mut self` trait impls — a
// shared `Arc<P2pNetwork>` can never yield `&mut self`). Before `initialize`
// builds the network (`p2p` still `None`), every method is a no-op — there is
// nothing VM-side to route to yet.
// ---------------------------------------------------------------------------

#[async_trait]
impl AppHandler for EvmVm {
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: std::time::Instant,
        request: &[u8],
    ) -> VmResult<()> {
        let Some(p2p) = self.shared.p2p.load_full() else {
            tracing::debug!(%node, request_id, "app_request before gossip system initialized");
            return Ok(());
        };
        p2p.handle_app_request(token, node, request_id, deadline, request)
            .await
    }

    async fn app_request_failed(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> VmResult<()> {
        let Some(p2p) = self.shared.p2p.load_full() else {
            tracing::debug!(
                %node,
                request_id,
                "app_request_failed before gossip system initialized"
            );
            return Ok(());
        };
        p2p.handle_app_request_failed(token, node, request_id, err)
            .await
    }

    async fn app_response(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> VmResult<()> {
        let Some(p2p) = self.shared.p2p.load_full() else {
            tracing::debug!(%node, request_id, "app_response before gossip system initialized");
            return Ok(());
        };
        p2p.handle_app_response(token, node, request_id, response)
            .await
    }

    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> VmResult<()> {
        let Some(p2p) = self.shared.p2p.load_full() else {
            tracing::debug!(%node, "app_gossip before gossip system initialized");
            return Ok(());
        };
        p2p.handle_app_gossip(token, node, msg).await
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
        token: &CancellationToken,
        node: NodeId,
        version: ava_version::application::Application,
    ) -> VmResult<()> {
        let Some(p2p) = self.shared.p2p.load_full() else {
            tracing::debug!(%node, "connected before gossip system initialized");
            return Ok(());
        };
        p2p.handle_connected(token, node, version).await
    }

    async fn disconnected(&mut self, token: &CancellationToken, node: NodeId) -> VmResult<()> {
        let Some(p2p) = self.shared.p2p.load_full() else {
            tracing::debug!(%node, "disconnected before gossip system initialized");
            return Ok(());
        };
        p2p.handle_disconnected(token, node).await
    }
}

// ---------------------------------------------------------------------------
// Vm.
// ---------------------------------------------------------------------------

/// A lock-free [`PendingWorkWaiter`] over `EvmVm`'s two mempools (the atomic
/// X<->C pool and the EVM pool), paced by the parent's ACP-226 minimum block
/// delay (coreth `waitForEvent`, `plugin/evm/block_builder.go:140-214`). Holds
/// only `Arc`s the VM already owns — NEVER the outer `Arc<Mutex<dyn Vm>>` a
/// proposal forwarder would otherwise have to hold to call `wait_for_event`
/// (the M7.18 lock-parking hazard this seam exists to avoid): `verified` is a
/// `DashMap`, the clock read takes the clock mutex only for the duration of the read, and no lock is held across an
/// `.await`.
struct EvmPendingWorkWaiter {
    atomic: Arc<parking_lot::Mutex<AtomicMempool>>,
    evm: Arc<parking_lot::Mutex<EvmMempool>>,
    /// The shared core — the preferred parent's header (`verified`) and the
    /// injected clock, the two ACP-226 pacing inputs.
    shared: Arc<Shared>,
    /// The preferred (leaf) block id the next build will extend.
    preferred: Arc<ArcSwap<Id>>,
}

impl EvmPendingWorkWaiter {
    /// True iff either pool currently holds work.
    fn pending(&self) -> bool {
        !self.atomic.lock().is_empty() || !self.evm.lock().is_empty()
    }
}

#[async_trait]
impl PendingWorkWaiter for EvmPendingWorkWaiter {
    fn has_pending(&self) -> bool {
        self.pending()
    }

    async fn wait(&self) {
        // Mirrors `EvmVm::wait_for_event` below: register on BOTH pools'
        // notify BEFORE the emptiness check so a tx admitted between the
        // check and the `select!` is never lost (tokio `Notify` stores one
        // permit — the `.notified()` future created here observes a
        // `notify_one` that fires after this line).
        loop {
            let atomic_notify = self.atomic.lock().subscribe();
            let evm_notify = self.evm.lock().subscribe();
            if !self.pending() {
                tokio::select! {
                    () = atomic_notify.notified() => {}
                    () = evm_notify.notified() => {}
                }
                // Loop back: re-subscribe and re-check. A spurious wake (e.g.
                // the admission that woke us was immediately drained by a
                // concurrent `build_block`) simply re-arms.
                continue;
            }

            // Work exists. coreth waitForEvent (block_builder.go:140-163): do
            // not signal the engine before the ACP-226 minimum delay after
            // the parent has elapsed — a block built earlier dies at its own
            // VerifyTime with MinDelayNotMet. Fail-open: an unresolvable
            // preferred id or a pre-Granite parent means nothing to wait for
            // (verify remains the safety backstop; coreth's nil-arm).
            let preferred = *self.preferred.load_full();
            let Some(min_next_ms) = self
                .shared
                .verified
                .get(&preferred)
                .and_then(|pb| crate::feerules::min_next_block_time_ms(pb.block.header()))
            else {
                return;
            };
            // `build_block` stamps whole seconds (`timestamp_ms = secs *
            // 1000`), so round UP to the next whole second that clears the
            // delay — a Go-built parent can carry a mid-second ms timestamp,
            // and flooring would still fail MinDelayNotMet.
            let target_secs = min_next_ms.div_ceil(1000);
            let now_secs = self.shared.clock.lock().unix();
            let remaining = target_secs.saturating_sub(now_secs);
            if remaining == 0 {
                return;
            }
            // Sleep the remainder, then loop back to the top: the preference
            // may have moved or the work may have drained while we slept.
            // Each iteration either returns or sleeps a strictly positive
            // remainder — no busy-spin. Cancellation-safe: the forwarder
            // `select!`s this future against the chain token, so a sleeping
            // waiter is simply dropped at teardown.
            tokio::time::sleep(Duration::from_secs(remaining)).await;
        }
    }
}

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
        app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        // Genesis-JSON parsing (alloc seeding + the upgrade schedule) is M6.8, and
        // it builds the provider/config/store collaborators this VM is constructed
        // over. Until that wiring lands, `EvmVm::new` is the construction seam
        // (the node bootstrap / tests supply the collaborators); `initialize` only
        // records the immutable chain context here.
        let node_id = chain_ctx.node_id;
        self.ctx = Some(chain_ctx);
        self.engine_state = EngineState::Initializing;

        // cchain-tx-gossip task 12: wire the C-Chain tx-gossip system over the
        // app_sender the engine hands us here (coreth `vm.go:780-833`
        // ordering — built AFTER the mempool/builder collaborators, which
        // `EvmVm::new` already assembled above the engine ever calls
        // `initialize`).
        let network = P2pNetwork::new(node_id, app_sender);

        let accounts: Arc<dyn crate::gossip::SenderAccountReader> =
            Arc::new(VmSenderAccountReader::new(Arc::clone(&self.shared.state)));
        let chain_id = {
            use ava_evm_reth::EthChainSpec;
            self.evm_config.chain_spec().chain().id()
        };
        let rules = AdmissionRules {
            chain_id,
            ..AdmissionRules::default()
        };
        let gossip_set = Arc::new(EthTxGossipSet::new(
            Arc::clone(&self.evm_mempool),
            accounts,
            rules,
        )?);

        // Task 14 test seam: `with_gossip_params_for_test` overrides this
        // (production keeps `GossipParams::default`, seeded by `EvmVm::new`).
        let params = self.shared.gossip_params.lock().clone();
        // Both the push and pull gossipers need a `Client` bound to
        // `TX_GOSSIP_HANDLER_ID`; `P2pNetwork::client` mints one WITHOUT
        // registering a handler (Go `Network.NewClient`/`AddHandler` are
        // likewise decoupled), which is what breaks the otherwise circular
        // dependency: the `GossipHandler` we register below needs `push`
        // already built (to forward newly-pushed items into it), so we can't
        // get a `Client` from `add_handler` first. Mint exactly ONE `Client`
        // and `.clone()` it for the second gossiper — matching Go's
        // `gossip.NewSystem` (`network/p2p/gossip/system.go:151-166`), which
        // builds a single `client` and passes that SAME value to both
        // `NewPullGossiper` and `NewPushGossiper`.
        let client = network.client(TX_GOSSIP_HANDLER_ID);
        let push = Arc::new(PushGossiper::new(
            EthTxMarshaller,
            Arc::clone(&gossip_set),
            client.clone(),
            params.clone(),
        ));
        let pull = PullGossiper::new(
            EthTxMarshaller,
            Arc::clone(&gossip_set),
            client,
            Arc::clone(&network),
            params.clone(),
        );
        let handler = Arc::new(GossipHandler::new(
            EthTxMarshaller,
            Arc::clone(&gossip_set),
            Some(Arc::clone(&push)),
            params.clone(),
        ));
        network
            .add_handler(TX_GOSSIP_HANDLER_ID, handler)
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to register the C-Chain tx-gossip handler");
                VmError::InvalidComponent("evm gossip handler registration failed")
            })?;

        // Two loops suffice: `PushGossiper::gossip_cycle` unconditionally
        // drains BOTH the to-gossip queue and the due-regossip queue on every
        // call (`ava-p2p`'s `gossip/push.rs`), so a third, separate regossip
        // loop is unnecessary — the `push_period` cadence below covers both,
        // matching Go's own two `Every(...)` goroutines (`vm.go:818-828`).
        let evm_mempool = Arc::clone(&self.evm_mempool);
        let push_for_loop = Arc::clone(&push);
        let push_op_token = self.shared.gossip_token.clone();
        tokio::spawn(every(
            self.shared.gossip_token.clone(),
            params.push_period,
            move || {
                let push = Arc::clone(&push_for_loop);
                let mempool = Arc::clone(&evm_mempool);
                let op_token = push_op_token.clone();
                async move {
                    // Drains newly-admitted local/remote txs into the push
                    // queue (Go's `ethTxPool.Subscribe` forwarding,
                    // `vm.go:773-778`; this port has no tx-pool subscription
                    // channel, so the push cycle itself pulls the outbox
                    // each tick instead — see `EvmMempool::take_gossip_outbox`).
                    let outbox = mempool.lock().take_gossip_outbox();
                    // Permanent live-debuggability hook (cchain-tx-gossip
                    // task 16 live debugging): this crate's own push cycle is
                    // the ONLY place a newly-admitted tx becomes a wire
                    // `PushGossip` send, and prior live debugging burned
                    // hours with no visibility into whether this closure
                    // ever ran with non-empty work. Only logs when there is
                    // something to report — a healthy idle node should not
                    // spam a log line every `push_period`.
                    if !outbox.is_empty() {
                        tracing::debug!(
                            node = %node_id,
                            batch_size = outbox.len(),
                            "C-Chain tx-gossip push: draining newly-admitted txs into the push queue"
                        );
                    }
                    for tx in outbox {
                        push.add(GossipEthTx(tx));
                    }
                    push.gossip_cycle(&op_token).await
                }
            },
        ));

        let pull = Arc::new(pull);
        let pull_op_token = self.shared.gossip_token.clone();
        tokio::spawn(every(
            self.shared.gossip_token.clone(),
            params.pull_period,
            move || {
                let pull = Arc::clone(&pull);
                let op_token = pull_op_token.clone();
                async move { pull.pull_cycle(&op_token).await }
            },
        ));

        self.shared.p2p.store(Some(network));
        Ok(())
    }

    async fn set_state(&mut self, _token: &CancellationToken, state: EngineState) -> VmResult<()> {
        self.engine_state = state;
        // Go coreth vm.go SetState → bootstrapped.Set(state == snow.NormalOp):
        // onNormalOperationsStarted flips true; re-entering bootstrap flips false.
        self.shared
            .bootstrapped
            .store(matches!(state, EngineState::NormalOp), Ordering::Release);
        Ok(())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> VmResult<()> {
        // Stops the two `initialize`-spawned gossip loops (cchain-tx-gossip
        // task 12). `CancellationToken::cancel` is idempotent and harmless to
        // call even if `initialize` never ran (no loop is listening yet).
        self.shared.gossip_token.cancel();
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
        // coreth `CreateHandlers` (graft/coreth/plugin/evm/vm.go:1029-1075)
        // plus the atomic wrapper (atomic/vm/vm.go:337-355): "/rpc" and "/ws"
        // serve the SAME geth rpc.Server (vm.go:1067-1068; the node's WS
        // adapter bridges frames as buffered POSTs, so one dispatch covers
        // both — eth_subscribe push is a documented deferral), "/admin" the
        // coreth admin API, "/avax" the atomic AvaxAPI. The Go map carries no
        // lock semantics (plain http.Handler), hence NoLock.
        let chain_id = {
            use ava_evm_reth::EthChainSpec;
            self.evm_config.chain_spec().chain().id()
        };
        let eth: Arc<dyn VmHttpService> = Arc::new(EthHttpService::new(EthRpc::new(
            Arc::clone(&self.shared.state),
            Arc::clone(&self.shared.blocks),
            self.evm_config.clone(),
            chain_id,
            Arc::clone(&self.evm_mempool),
            Arc::clone(&self.accepted_tx_index),
        )));
        let avax = avax_service(AvaxRpc::new(
            Arc::clone(&self.txpool),
            Arc::clone(&self.shared.blocks),
            Arc::clone(&self.accepted_atomic_txs),
        ));
        // TODO(M8.23/M8.29): Go exposes "/admin" only when the chain config
        // sets admin-api-enabled (coreth vm.go:1046 `if
        // vm.config.AdminAPIEnabled`). EvmVm has no per-chain config plumbed
        // yet (initialize ignores config_bytes), so the handler is exposed
        // unconditionally until that plumbing lands.
        let admin = admin_service(AdminRpc::new());

        let mut handlers = HashMap::new();
        handlers.insert(
            ETH_RPC_ENDPOINT.to_string(),
            HttpHandler::in_process(LockOptions::NoLock, Arc::clone(&eth)),
        );
        handlers.insert(
            ETH_WS_ENDPOINT.to_string(),
            HttpHandler::in_process(LockOptions::NoLock, eth),
        );
        handlers.insert(
            AVAX_ENDPOINT.to_string(),
            HttpHandler::in_process(LockOptions::NoLock, avax),
        );
        handlers.insert(
            ADMIN_ENDPOINT.to_string(),
            HttpHandler::in_process(LockOptions::NoLock, admin),
        );
        Ok(handlers)
    }

    async fn new_http_handler(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        // coreth `NewHTTPHandler` returns `(nil, nil)` at this pin
        // (graft/coreth/plugin/evm/vm.go:1079-1081): the C-Chain has no
        // header-routed handler; clients reach the EVM via the "/rpc"/"/ws"/
        // "/avax"/"/admin" PATH extensions mounted by `create_handlers`
        // (14 §10). `None` is the wire-faithful answer.
        Ok(None)
    }

    async fn wait_for_event(&self, token: &CancellationToken) -> VmResult<VmEvent> {
        // Report PendingTxs when EITHER pool (atomic X<->C or EVM) is non-empty;
        // otherwise park until the first admission notify from either pool, or
        // cancellation. The engine's notification forwarder re-invokes
        // `wait_for_event` after each event (Go `common.NotificationForwarder`
        // loop; the C-Chain rpcchainvm host drives this over gRPC), re-checking
        // emptiness each round, so a spurious wake is harmless.
        //
        // Register on BOTH notifies BEFORE the emptiness check so a tx admitted
        // between the check and the `select!` still wakes us (tokio `Notify`
        // stores one permit): the `.notified()` future created here observes a
        // `notify_one` that fires after this line. `VmEvent` has no cancellation
        // variant, so on shutdown we return `PendingTxs` — the engine re-checks
        // emptiness and re-parks, harmless when tearing down.
        let atomic_notify = self.txpool.lock().subscribe();
        let evm_notify = self.evm_mempool.lock().subscribe();
        let pending = !self.txpool.lock().is_empty() || !self.evm_mempool.lock().is_empty();
        if pending {
            return Ok(VmEvent::PendingTxs);
        }
        tokio::select! {
            () = atomic_notify.notified() => {}
            () = evm_notify.notified() => {}
            () = token.cancelled() => {}
        }
        Ok(VmEvent::PendingTxs)
    }

    fn pending_work_waiter(&self) -> Option<Arc<dyn PendingWorkWaiter>> {
        Some(Arc::new(EvmPendingWorkWaiter {
            atomic: Arc::clone(&self.txpool),
            evm: Arc::clone(&self.evm_mempool),
            shared: Arc::clone(&self.shared),
            preferred: Arc::clone(&self.preferred),
        }))
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

        // The next-block build/fee context (§17.3). The build-time timestamp comes
        // from the injectable clock (specs/24 hazard #5: never read the wall clock
        // directly — this header time is consensus state), clamped to >
        // parent.time so the header is strictly monotonic. The parent's real
        // dynamic-fee state is parsed from its extra prefix (coreth
        // `feeStateBeforeBlock`/`feeWindow` initial state) so the child base fee is
        // derived from the actual parent state, not a default. The atomic gas
        // budget is the post-AP5 limit the mempool packs against.
        let now_secs = self
            .shared
            .clock
            .lock()
            .unix()
            .max(parent_header.time.saturating_add(1));
        let parent_fee_state =
            crate::feerules::parent_fee_state_of(self.evm_config.chain_spec(), &parent_header)
                .map_err(VmError::from)?;
        let ctx = AvaNextBlockCtx {
            timestamp: now_secs,
            // The header carries `Time` (seconds) and, at Granite, a millisecond
            // `TimeMilliseconds`; we build at whole-second precision so the two
            // stay consistent (`time_ms / 1000 == time`).
            timestamp_ms: now_secs.saturating_mul(1000),
            parent_fee_state,
            ..AvaNextBlockCtx::with_atomic_gas_limit(100_000)
        };

        // Pull the fee-cap-ordered EVM candidates from the mempool (M6.23:
        // purpose-built `EvmMempool` in place of reth's pool — design doc
        // `crates/ava-evm/src/mempool.rs`). `build_on` re-filters each for
        // base-fee affordability and gas budget as it packs (`builder.rs`
        // `pack_evm_txs`); the atomic batch (if any) is drained inside `build_on`.
        // Snapshot `(signer, nonce, hash)` BEFORE moving the candidates into
        // `build_on` so a batch-execution failure can evict exactly them.
        let evm_candidates = self.evm_mempool.lock().best_txs();
        let stale: Vec<(Address, u64, B256)> = evm_candidates
            .iter()
            .map(|tx| (tx.signer(), ConsensusTx::nonce(tx.inner()), *tx.hash()))
            .collect();
        let had_candidates = !stale.is_empty();
        match self
            .builder
            .build_on(&parent_header, parent_state_root, &ctx, evm_candidates)
        {
            Ok(block) => Ok(self.wrap(block)),
            // "Nothing to build" / min-retry-delay guard -> no pending block.
            // This is the benign path (no atomic batch AND no includable EVM
            // tx); NEVER evict here — the candidates were simply not packable
            // (e.g. underpriced at the real base fee), not poison.
            Err(Error::MissingProposal(_)) => Err(VmError::NotFound),
            Err(e) => {
                // Admission pre-validates nonce/balance/gas, so a batch-execution
                // failure with candidates present is exceptional. Evict the whole
                // snapshotted batch (design §Component 3 — no per-tx bisection) so
                // a poisoned tx can never wedge block building, and say so loudly.
                if had_candidates {
                    self.evm_mempool.lock().on_block_accepted(&stale);
                    tracing::warn!(
                        error = %e,
                        evicted = stale.len(),
                        "build_on failed with EVM candidates present; evicted the batch"
                    );
                }
                Err(VmError::from(e))
            }
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
