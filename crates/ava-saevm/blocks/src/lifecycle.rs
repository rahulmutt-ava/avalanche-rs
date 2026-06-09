// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`Block`] lifecycle state machine (specs/11 §4.2).
//!
//! Port of `vms/saevm/blocks/block.go`, `blocks/execution.go`, and the
//! execution-half of `blocks/settlement.go` (the [`Range`](crate::Range) /
//! `last_to_settle_at` half lives in `settlement.rs`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwapOption;
use tokio::sync::Notify;

use ava_evm_reth::{B256, RethBlock, SealedBlock};
use ava_saevm_gastime::GasTime;
use ava_saevm_proxytime::Time;
use ava_saevm_types::{Address, ExecutionResults, U256};
use ava_vm::components::gas::Price;

// ---------------------------------------------------------------------------
// In-memory GC counter (specs/11 §10 invariant 8)
// ---------------------------------------------------------------------------

/// Number of [`Block`] instances currently live (constructed but not yet
/// dropped). The Go reference uses `runtime.AddCleanup` + an `atomic.Int64`
/// (`blocks.InMemoryBlockCount`); the Rust port uses a [`Drop`] impl. A leak in
/// the ancestry linked-list shows up as this counter failing to return to its
/// baseline after settlement releases the parent `Arc`s.
static IN_MEMORY_BLOCK_COUNT: AtomicI64 = AtomicI64::new(0);

/// Returns the number of [`Block`] instances yet to be dropped (mirrors Go's
/// `blocks.InMemoryBlockCount`).
#[must_use]
pub fn in_memory_block_count() -> i64 {
    IN_MEMORY_BLOCK_COUNT.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failure constructing or transitioning a [`Block`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The parent's hash does not match this block's `parent_hash`.
    #[error("block-parent hash mismatch: parent {got:#x}, expected {want:#x}")]
    ParentHashMismatch {
        /// The parent block's actual hash.
        got: B256,
        /// The `parent_hash` declared by this block's header.
        want: B256,
    },
    /// The parent's height is not exactly one less than this block's.
    #[error("block height not incrementing: parent height {parent}, own height {own}")]
    HeightNotIncrementing {
        /// The parent block's height.
        parent: u64,
        /// This block's height.
        own: u64,
    },
    /// [`Block::mark_executed`] (or `mark_synchronous`) was called more than
    /// once.
    #[error("block re-marked as executed: height {0}")]
    ReExecuted(u64),
    /// [`Block::mark_settled`] (or `mark_synchronous`) was called more than
    /// once.
    #[error("block re-settled: height {0}")]
    ReSettled(u64),
    /// A persist step (the "D" of D→M→I→X) supplied by the caller failed.
    #[error("persisting execution artefacts: {0}")]
    Persist(String),
}

// ---------------------------------------------------------------------------
// Lifecycle stage
// ---------------------------------------------------------------------------

/// The async-execution lifecycle stage of a [`Block`] (specs/11 §4.2).
///
/// Ordered so `NotExecuted < Executed < Settled`, mirroring the Go
/// `blocks.LifeCycleStage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LifeCycleStage {
    /// Accepted by consensus but not yet executed.
    NotExecuted,
    /// Execution committed; results available.
    Executed,
    /// Execution results agreed by consensus (settled); ancestry severed.
    Settled,
}

// ---------------------------------------------------------------------------
// Ancestry / bounds / artefacts
// ---------------------------------------------------------------------------

/// The ancestral pointers a non-settled block holds: its `parent` and the
/// `last_settled` block at the time of its acceptance. Severed (the whole
/// [`Ancestry`] is dropped) once the block settles, so the chain of ancestors
/// can be garbage-collected (specs/11 §4.2). Mirrors Go's `blocks.ancestry`.
#[derive(Clone)]
pub struct Ancestry {
    /// The parent block (one height below).
    pub parent: Option<Arc<Block>>,
    /// The last-settled block as of this block's acceptance.
    pub last_settled: Option<Arc<Block>>,
}

/// Builder-predicted worst-case bounds, set before execution as an early-warning
/// system for near-miss mispredictions (specs/11 §4.2/§6.1, Go
/// `blocks.WorstCaseBounds`).
///
/// Produced by `ava_saevm_worstcase::State::finish_block` (M7.13) and attached to
/// a [`Block`] before execution via [`Block::set_worst_case_bounds`]. During
/// actual execution the executor asserts the realised base fee and per-op
/// burner balances stay within these bounds (the `check_base_fee_bound` /
/// `check_sender_balance_bound` assertions live in `ava-saevm-worstcase`,
/// reading this data; a violation is test-fatal).
///
/// Note: there is deliberately no `Default` impl — [`GasTime`] has no sensible
/// zero value, and the only construction site is the worst-case replay.
#[derive(Clone, Debug)]
pub struct WorstCaseBounds {
    /// Upper bound on the base fee this block will encounter at execution.
    pub max_base_fee: Price,
    /// The final worst-case gas clock after replaying the candidate block (Go
    /// `LatestEndTime`).
    pub latest_end_time: GasTime,
    /// Per-op snapshots of each burner's balance taken immediately before that
    /// op was applied during worst-case replay (Go `MinOpBurnerBalances`). The
    /// outer `Vec` is op-indexed; an empty inner map is an op with no burns.
    pub min_op_burner_balances: Vec<BTreeMap<Address, U256>>,
}

/// The artefacts produced by executing a block, handed to
/// [`Block::mark_executed`]. Bundles the persisted [`ExecutionResults`] with
/// the final interim execution time (the `I` of D→M→I→X).
#[derive(Clone)]
pub struct ExecutionArtefacts {
    /// The persisted per-block execution results (gas-time, base fee, roots).
    pub results: ExecutionResults,
    /// The final interim execution time (proxy-clock instant in gas units).
    /// Stored monotonically so `last_to_settle_at` can settle mid-block.
    pub interim_execution_time: Time<u64>,
}

// ---------------------------------------------------------------------------
// Block
// ---------------------------------------------------------------------------

/// A SAE block: a wire-identical Ethereum [`SealedBlock`] plus the SAE async
/// execution/settlement lifecycle (specs/11 §4.2). Must be constructed with
/// [`Block::new`]. Mirrors Go's `blocks.Block`.
pub struct Block {
    /// The wire block. RLP/keccak-identical to a geth/`libevm` block.
    eth: SealedBlock<RethBlock>,
    /// `Some` iff the block has **not** yet settled (severed for GC on settle).
    /// Invariant: `Some(ancestry)` ⇔ stage `< Settled`.
    ancestry: ArcSwapOption<Ancestry>,
    /// Genesis / last pre-SAE block. Self-settling — its `ancestry` is `None`.
    synchronous: OnceLock<()>,
    /// Builder worst-case prediction, set before execution. TODO(M7.13).
    #[allow(dead_code)]
    bounds: OnceLock<WorstCaseBounds>,
    /// `Some` iff executed (the `M` of D→M→I→X).
    execution: ArcSwapOption<ExecutionResults>,
    /// Monotonic interim execution time, set during/after execution (`I`).
    interim_execution_time: ArcSwapOption<Time<u64>>,
    /// Fired after `execution` is set (the `X` of D→M→I→X).
    executed: Notify,
    /// Fired after `ancestry` is cleared (settlement).
    settled: Notify,
}

impl Block {
    /// Constructs a new block from its wire form and ancestry.
    ///
    /// While both `parent` and `last_settled` MAY be `None` (e.g. when parsing
    /// an encoded block before verification populates ancestry), the resulting
    /// block is incomplete until [`Block::set_ancestors`] is called. Increments
    /// the [`in_memory_block_count`] GC counter.
    ///
    /// # Errors
    /// [`Error::ParentHashMismatch`] / [`Error::HeightNotIncrementing`] if a
    /// non-`None` `parent` is inconsistent with this block's header.
    pub fn new(
        eth: SealedBlock<RethBlock>,
        parent: Option<Arc<Block>>,
        last_settled: Option<Arc<Block>>,
    ) -> Result<Self, Error> {
        IN_MEMORY_BLOCK_COUNT.fetch_add(1, Ordering::SeqCst);
        let b = Self {
            eth,
            ancestry: ArcSwapOption::from(None),
            synchronous: OnceLock::new(),
            bounds: OnceLock::new(),
            execution: ArcSwapOption::from(None),
            interim_execution_time: ArcSwapOption::from(None),
            executed: Notify::new(),
            settled: Notify::new(),
        };
        b.set_ancestors(parent, last_settled)?;
        Ok(b)
    }

    /// Sets the block's ancestry, enforcing the parent-hash / height invariants.
    ///
    /// Mirrors Go's `blocks.Block.SetAncestors`.
    ///
    /// # Errors
    /// As [`Block::new`].
    pub fn set_ancestors(
        &self,
        parent: Option<Arc<Block>>,
        last_settled: Option<Arc<Block>>,
    ) -> Result<(), Error> {
        if let Some(p) = &parent {
            let got = p.hash();
            let want = self.parent_hash();
            if got != want {
                return Err(Error::ParentHashMismatch { got, want });
            }
            let own = self.height();
            // own >= 1 because a parent exists; saturating guards height 0 anyway.
            let expected_parent_height = own.saturating_sub(1);
            if p.height() != expected_parent_height {
                return Err(Error::HeightNotIncrementing {
                    parent: p.height(),
                    own,
                });
            }
        }
        self.ancestry.store(Some(Arc::new(Ancestry {
            parent,
            last_settled,
        })));
        Ok(())
    }

    /// Sets the builder's worst-case bounds (before execution). TODO(M7.13):
    /// gains real checks when `ava-saevm-worstcase` lands.
    pub fn set_worst_case_bounds(&self, bounds: WorstCaseBounds) {
        let _ = self.bounds.set(bounds);
    }

    // -- wire-block accessors ----------------------------------------------

    /// The wire (sealed Ethereum) block.
    #[must_use]
    pub fn eth_block(&self) -> &SealedBlock<RethBlock> {
        &self.eth
    }

    /// The block hash (`keccak256(RLP(header))`).
    #[must_use]
    pub fn hash(&self) -> B256 {
        self.eth.hash()
    }

    /// The parent block's hash, from the header.
    #[must_use]
    pub fn parent_hash(&self) -> B256 {
        self.eth.header().parent_hash
    }

    /// The block height.
    #[must_use]
    pub fn height(&self) -> u64 {
        self.eth.header().number
    }

    /// The block (inclusion) time as a Unix timestamp.
    #[must_use]
    pub fn build_time(&self) -> u64 {
        self.eth.header().timestamp
    }

    /// The block timestamp as a [`SystemTime`].
    #[must_use]
    pub fn timestamp(&self) -> SystemTime {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(self.build_time()))
            .unwrap_or(UNIX_EPOCH)
    }

    // -- stage / ancestry --------------------------------------------------

    /// The current lifecycle stage.
    #[must_use]
    pub fn stage(&self) -> LifeCycleStage {
        if self.settled() {
            LifeCycleStage::Settled
        } else if self.executed() {
            LifeCycleStage::Executed
        } else {
            LifeCycleStage::NotExecuted
        }
    }

    /// The parent block, or `None` once the block has settled (ancestry
    /// severed). Mirrors Go's `Block.ParentBlock`.
    #[must_use]
    pub fn parent_block(&self) -> Option<Arc<Block>> {
        self.ancestry.load_full().and_then(|a| a.parent.clone())
    }

    /// The last-settled block as of this block's acceptance.
    ///
    /// Returns `self`'s identity-free `None` once settled, except a synchronous
    /// block always reports itself (it is self-settling). Mirrors Go's
    /// `Block.LastSettled`.
    #[must_use]
    pub fn last_settled(self: &Arc<Self>) -> Option<Arc<Block>> {
        if self.is_synchronous() {
            return Some(Arc::clone(self));
        }
        self.ancestry
            .load_full()
            .and_then(|a| a.last_settled.clone())
    }

    // -- execution (M / I / X) ---------------------------------------------

    /// Reports whether [`Block::mark_executed`] / `mark_synchronous` succeeded.
    #[must_use]
    pub fn executed(&self) -> bool {
        self.execution.load().is_some()
    }

    /// Reports whether the block has settled (ancestry severed). Mirrors Go's
    /// `Block.Settled`.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.ancestry.load().is_none()
    }

    /// Reports whether [`Block::mark_synchronous`] succeeded.
    #[must_use]
    pub fn is_synchronous(&self) -> bool {
        self.synchronous.get().is_some()
    }

    /// Marks the block executed in strict **D→M→I→X** order (specs/11 §4.2,
    /// `blocks/execution.go::markExecuted`):
    ///
    /// 1. **D** — runs the caller's `persist` step (writes receipts + the
    ///    execution-results blob to disk) *first*, so a successful return is a
    ///    durable-write guarantee.
    /// 2. **M** — `execution` pointer set via a once-only CAS.
    /// 3. **I** — interim execution time stored.
    /// 4. **X** — `executed` notified; `last_executed` advanced.
    ///
    /// `last_executed` is the optional VM-level last-executed pointer (advanced
    /// before the `executed` notification fires, matching Go).
    ///
    /// # Errors
    /// [`Error::ReExecuted`] if called twice; [`Error::Persist`] if `persist`
    /// fails (no in-memory state is mutated in that case).
    pub fn mark_executed(
        self: &Arc<Self>,
        artefacts: ExecutionArtefacts,
        last_executed: Option<&ArcSwapOption<Block>>,
    ) -> Result<(), Error> {
        self.mark_executed_with(artefacts, last_executed, || Ok(()))
    }

    /// As [`Block::mark_executed`] but with an explicit `persist` closure
    /// modelling the **D** step (disk write). The closure runs before any
    /// in-memory mutation; on `Err` the block is left untouched.
    ///
    /// # Errors
    /// As [`Block::mark_executed`].
    pub fn mark_executed_with<F>(
        self: &Arc<Self>,
        artefacts: ExecutionArtefacts,
        last_executed: Option<&ArcSwapOption<Block>>,
        persist: F,
    ) -> Result<(), Error>
    where
        F: FnOnce() -> Result<(), String>,
    {
        // D — disk first.
        persist().map_err(Error::Persist)?;
        self.mark_executed_after_disk(artefacts, last_executed)
    }

    /// The post-disk half of `mark_executed` (the `M`/`I`/`X` steps). Shared by
    /// [`Block::mark_executed`], `mark_synchronous`, and
    /// `restore_execution_artefacts`. Mirrors Go's
    /// `markExecutedAfterDiskArtefacts`.
    fn mark_executed_after_disk(
        self: &Arc<Self>,
        artefacts: ExecutionArtefacts,
        last_executed: Option<&ArcSwapOption<Block>>,
    ) -> Result<(), Error> {
        let ExecutionArtefacts {
            results,
            interim_execution_time,
        } = artefacts;

        // M — set the execution pointer once (None -> Some). `mark_executed` is
        // driven by the single execution thread (specs/11 §6.1), so this
        // check-then-store is a once-only *API-misuse* guard, not a data-race
        // guard: a non-`None` prior state means we were called twice (fatal in
        // Go). Done before any other mutation so an early return leaves the
        // block untouched.
        if self.execution.load().is_some() {
            return Err(Error::ReExecuted(self.height()));
        }
        self.execution.store(Some(Arc::new(results)));

        // I — interim execution time.
        self.interim_execution_time
            .store(Some(Arc::new(interim_execution_time)));

        // X — external indicators (last_executed advanced *before* the notify,
        // matching Go's ordering).
        if let Some(le) = last_executed {
            le.store(Some(Arc::clone(self)));
        }
        self.executed.notify_waiters();
        Ok(())
    }

    /// Stores the monotonic interim execution time during execution (the `I`
    /// step, exposed for the executor to call per-transaction). Mirrors Go's
    /// `Block.SetInterimExecutionTime`.
    pub fn set_interim_execution_time(&self, t: Time<u64>) {
        self.interim_execution_time.store(Some(Arc::new(t)));
    }

    /// The interim execution time, if execution has begun (read by the executor
    /// and the settlement layer — M7.14/M7.17).
    #[must_use]
    pub fn interim_execution_time(&self) -> Option<Arc<Time<u64>>> {
        self.interim_execution_time.load_full()
    }

    /// The committed execution results, if executed (read by the executor and
    /// the settlement layer — M7.14/M7.17).
    #[must_use]
    pub fn execution_results(&self) -> Option<Arc<ExecutionResults>> {
        self.execution.load_full()
    }

    /// Waits until [`Block::mark_executed`] fires, warning (via `tracing`) if
    /// the wait exceeds [`MAX_QUEUE_WALL_TIME`](ava_saevm_params::MAX_QUEUE_WALL_TIME).
    /// Mirrors Go's `executionArtefact` blocking-with-warn behaviour.
    pub async fn wait_until_executed(&self) {
        if self.executed() {
            return;
        }
        let notified = self.executed.notified();
        if self.executed() {
            return;
        }
        if tokio::time::timeout(ava_saevm_params::MAX_QUEUE_WALL_TIME, notified)
            .await
            .is_err()
        {
            tracing::warn!(
                height = self.height(),
                waited = ?ava_saevm_params::MAX_QUEUE_WALL_TIME,
                "blocking on execution artefact longer than expected",
            );
            self.executed.notified().await;
        }
    }

    /// Waits until [`Block::mark_settled`] fires.
    pub async fn wait_until_settled(&self) {
        if self.settled() {
            return;
        }
        let notified = self.settled.notified();
        if self.settled() {
            return;
        }
        notified.await;
    }

    /// The post-execution state root.
    ///
    /// # Panics
    /// Reads the committed execution results directly; the block MUST be
    /// executed (use [`Block::wait_until_executed`] first if unsure). The
    /// `OnceLock`-like guarantee is upheld by the once-only `mark_executed`.
    #[must_use]
    pub fn post_execution_state_root(&self) -> B256 {
        self.execution
            .load_full()
            .map_or(B256::ZERO, |e| e.post_state_root)
    }

    /// The base fee that applied during execution (vs. the worst-case
    /// prediction in the header). `0` if not yet executed.
    #[must_use]
    pub fn executed_base_fee(&self) -> Price {
        self.execution.load_full().map_or(Price(0), |e| e.base_fee)
    }

    /// The block's execution gas-time (proxy-clock instant). `None` if not
    /// executed. Mirrors Go's `Block.ExecutedByGasTime`.
    #[must_use]
    pub fn executed_by_gas_time(&self) -> Option<Time<u64>> {
        self.execution.load_full().map(|e| e.gas_time.clone())
    }

    // -- settlement (S) ----------------------------------------------------

    /// The contiguous half-open range of ancestors this block settles:
    /// `(parent.last_settled, self.last_settled]` (specs/11 §1.2, Go
    /// `blocks/settlement.go::Settles`). A synchronous (genesis / pre-SAE)
    /// block is self-settling, so it settles exactly itself.
    #[must_use]
    pub fn settles(self: &Arc<Self>) -> crate::settlement::Range {
        if self.is_synchronous() {
            return crate::settlement::Range::singleton(Arc::clone(self));
        }
        let from = self.parent_block().and_then(|p| p.last_settled());
        let to = self.last_settled();
        crate::settlement::Range::between(from, to)
    }

    /// Marks the block settled: CAS the ancestry to `None` (severing parent
    /// links so the ancestor chain can be GC'd), advance `last_settled`, fire
    /// the `settled` notification. Once-only. Mirrors Go's
    /// `blocks/settlement.go::markSettled`.
    ///
    /// `last_settled` is the optional VM-level last-settled pointer.
    ///
    /// # Errors
    /// [`Error::ReSettled`] if called twice (or after `mark_synchronous`).
    pub fn mark_settled(
        self: &Arc<Self>,
        last_settled: Option<&ArcSwapOption<Block>>,
    ) -> Result<(), Error> {
        // Atomically take the ancestry (Some -> None), severing the parent /
        // last-settled `Arc`s so the ancestor chain can be GC'd. `swap` returns
        // the *previous* value: a `None` prior state means the block was already
        // settled (or restored-as-settled), so this is a re-settle (once-only).
        let prev = self.ancestry.swap(None);
        if prev.is_none() {
            return Err(Error::ReSettled(self.height()));
        }
        if let Some(ls) = last_settled {
            ls.store(Some(Arc::clone(self)));
        }
        self.settled.notify_waiters();
        Ok(())
    }

    /// Combined execute + settle for the genesis / last pre-SAE block, which is
    /// self-settling (impossible under normal SAE rules). Mirrors Go's
    /// `Block.MarkSynchronous`.
    ///
    /// Derives synthetic execution artefacts from the wire block (the synchronous
    /// block's results were already "settled" by the block itself): the
    /// post-state root is the header `state_root`, the gas-time/base-fee are
    /// placeholders since a synchronous block carries no SAE gas clock.
    ///
    /// # Errors
    /// As [`Block::mark_executed`] / [`Block::mark_settled`].
    pub fn mark_synchronous(self: &Arc<Self>) -> Result<(), Error> {
        let header = self.eth.header();
        let base_fee = header.base_fee_per_gas.map_or(Price(0), Price);
        let results = ExecutionResults {
            // A synchronous block carries no SAE gas clock; a fixed
            // unit-rate instant at its build time is sufficient and never read
            // by SAE settlement (it is self-settling). TODO(M7.21): when the
            // last-pre-SAE handoff is wired, derive this from the hook's
            // `GasConfigAfter` (Go `MarkSynchronous` uses `gastime.New`).
            gas_time: Time::<u64>::new(header.timestamp, 0, 1),
            base_fee,
            receipt_root: header.receipts_root,
            post_state_root: header.state_root,
        };
        let artefacts = ExecutionArtefacts {
            interim_execution_time: results.gas_time.clone(),
            results,
        };
        // Mark synchronous BEFORE settling so `last_settled` reports self.
        let _ = self.synchronous.set(());
        // Synchronous blocks do not set the chain head here (Go: caller does).
        self.mark_executed_after_disk(artefacts, None)?;
        self.mark_settled(None)
    }

    /// Restores a block to the executed state from previously persisted
    /// artefacts (recovery / `GetBlock`). Mirrors Go's
    /// `Block.RestoreExecutionArtefacts`.
    ///
    /// # Errors
    /// As [`Block::mark_executed`].
    pub fn restore_execution_artefacts(
        self: &Arc<Self>,
        results: ExecutionResults,
    ) -> Result<(), Error> {
        let artefacts = ExecutionArtefacts {
            interim_execution_time: results.gas_time.clone(),
            results,
        };
        self.mark_executed_after_disk(artefacts, None)
    }

    /// Restores a block directly to the settled state (recovery / `GetBlock` of
    /// a settled block). Mirrors Go's `RestoreSettledBlock`.
    ///
    /// # Errors
    /// As [`Block::restore_execution_artefacts`] / [`Block::mark_settled`].
    pub fn restore_settled_block(self: &Arc<Self>, results: ExecutionResults) -> Result<(), Error> {
        self.restore_execution_artefacts(results)?;
        self.mark_settled(None)
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        IN_MEMORY_BLOCK_COUNT.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// BlockProperties (adaptor bridge) — DEFERRED to M7.18
// ---------------------------------------------------------------------------
//
// The Snowman `adaptor::BlockProperties` impl is intentionally NOT provided
// here. Two constraints push it to the VM layer (M7.18), where the concrete
// consensus block type is defined:
//
//  1. **Orphan rule.** `BlockProperties` is a foreign trait (ava-saevm-adaptor)
//     and `Arc<T>` is not a fundamental type, so `impl BlockProperties for
//     Arc<Block>` is illegal in this crate. The impl must live on a *local*
//     newtype, which is the VM's block handle.
//  2. **`bytes() -> &[u8]`** must return the block's RLP wire bytes. `Block`
//     stores only the `SealedBlock` (whose RLP can be recomputed), but the
//     `&[u8]` borrow needs a cached buffer owned alongside the block — the VM
//     caches the `parse_block` input bytes (M7.18), so the borrow is natural
//     there.
//
// See the M7.10 adaptor follow-up note (a real `BlockProperties`/verify-context
// test also needs the M3 VM plumbing). M7.11 delivers the block + lifecycle +
// settlement; the consensus bridge lands with the VM.
