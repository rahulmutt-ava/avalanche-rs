// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The pure 10-step SAE execute step (specs/11 §6.1).
//!
//! [`execute_step`] is the heart of the streaming executor: a deterministic
//! function of `(ordered block, parent gas clock, parent state root, bounds,
//! driver, hooks)` that runs one block's transactions + end-of-block ops through
//! the [`EvmDriver`] reuse seam, ticks the SAE gas clock, asserts the realised
//! base fee stays within the builder-agreed worst-case bound, commits the
//! resulting state in strict **D→M→I→X** / CC-ORDER (specs/27 §2.4), and marks
//! the block executed.
//!
//! Faithful port of Go `vms/saevm/saexec/execution.go::{Execute, afterExecution}`,
//! collapsed into one function because the Rust [`EvmDriver`] seam already owns
//! the per-tx revm loop (the Go `Execute` body) and the Firewood bundle propose.
//!
//! # Purity
//!
//! No wall-clock enters any consensus output (Go's `FinishBy.Wall` is dropped —
//! it is observational only) and no unsorted map is iterated (specs/00 §6.1).
//! The only time source is the gas clock, which advances by gas consumption.

use std::sync::Arc;

use arc_swap::ArcSwapOption;
use ava_saevm_blocks::{Block, ExecutionArtefacts, WorstCaseBounds};
use ava_saevm_db::Tracker;
use ava_saevm_gastime::GasTime;
use ava_saevm_types::{B256, ExecutionResults};
use ava_vm::components::gas::Price;

use crate::driver::{EvmDriver, ExecHooks, TxReceipt};
use crate::error::{Error, Result};

/// The outputs of one [`execute_step`] the caller / tests assert on.
///
/// A pure summary of the executed block: the per-tx receipts, the committed
/// post-execution state root, the realised base fee, and the block's final
/// gas-clock instant. The block itself is mutated in place (marked executed via
/// the strict D→M→I→X sequence); this struct is the value-level echo of that.
#[derive(Clone, Debug)]
pub struct StepOutput {
    /// Per-transaction receipts in execution order.
    pub receipts: Vec<TxReceipt>,
    /// The committed post-execution state root (the `X`-visible root).
    pub post_state_root: B256,
    /// The realised base fee that applied during execution.
    pub base_fee: Price,
    /// The block's final gas clock after `after_block` (the persisted gas-time).
    pub gas_time: GasTime,
    /// Total gas consumed by the block (txs + end-of-block ops).
    pub gas_used: u64,
}

/// Runs the pure 10-step SAE execute step for `block` (specs/11 §6.1).
///
/// `parent_gas_clock` is the parent block's executed gas clock (the executor
/// caches this for exact continuity; the recovery fallback is
/// [`rebuild_gas_clock`](crate::driver::rebuild_gas_clock)). `parent_root` is the
/// parent's committed post-execution state root. `bounds` is the builder's
/// worst-case prediction attached before execution (threaded in rather than read
/// back off the block, since [`Block`] exposes no bounds getter).
/// `last_executed_ptr` is the VM-level last-executed pointer advanced inside the
/// `X` step.
///
/// # Errors
///
/// * [`Error::ParentMismatch`] (fatal) — `block.parent_hash() != last_executed`.
/// * [`Error::WorstCase`] — the realised base fee exceeds the worst-case bound.
/// * [`Error::Fatal`] — a transaction *errored* (vs. reverted), a hook failed,
///   or the EVM/state layer could not make progress.
/// * [`Error::StateDb`] / [`Error::Lifecycle`] — a commit / mark-executed
///   failure.
#[allow(clippy::too_many_arguments)]
pub fn execute_step<D: EvmDriver, H: ExecHooks>(
    block: &Arc<Block>,
    last_executed: &Block,
    parent_gas_clock: &GasTime,
    parent_root: B256,
    bounds: &WorstCaseBounds,
    driver: &D,
    hooks: &H,
    tracker: &Tracker,
    last_executed_ptr: &ArcSwapOption<Block>,
) -> Result<StepOutput> {
    // (1) Parent-hash sanity (Go: `last := e.lastExecuted.Load().Hash()`). If the
    // VM re-enqueues a block after a post-enqueue error, we'd see the same block
    // twice; a mismatch is fatal — it can only mean a broken ordering invariant.
    let parent_hash = block.parent_hash();
    let last_hash = last_executed.hash();
    if parent_hash != last_hash {
        return Err(Error::ParentMismatch {
            parent: parent_hash,
            last: last_hash,
        });
    }

    // (2) Clone the parent gas clock and advance it to this block's build time
    // (Go: `gasClock := parent.ExecutedByGasTime().Clone(); gasClock.BeforeBlock`).
    // The per-tx clock seeds from the same instant (Go: `perTxClock`).
    let sealed_header = block.eth_block().clone_sealed_header();
    let (block_unix, block_nanos) = hooks.block_time(&sealed_header);
    let mut gas_clock = parent_gas_clock.clone();
    gas_clock.before_block(block_unix, block_nanos);
    let mut per_tx_clock = gas_clock.time();

    // (3) Parent state is opened by the driver from `parent_root` (Go:
    // `sdbo.StateDB(parent.PostExecutionStateRoot())`). (4) the before-block hook
    // is a no-op for the M7.14 pure-EVM path (`NoopExecHooks`); the C-Chain body
    // lands in M7.21.

    // (5) The realised base fee is the gas clock's price; assert it stays within
    // the builder-agreed worst-case bound (Go: `b.CheckBaseFeeBound(baseFee)`).
    let base_fee = gas_clock.price();
    ava_saevm_worstcase::check_base_fee_bound(bounds, base_fee)?;

    // (6) Execute the block's ordered transactions through the EVM reuse seam.
    // (7) End-of-block ops are appended (Go: `hooks.EndOfBlockOps`); for the
    // pure-EVM path they are empty.
    //
    // TODO(M7.21+): the Go reference also runs the per-tx early-warning
    // prediction-model assertions here — `b.CheckSenderBalanceBound(...)` (per
    // tx, saexec/execution.go ~L192) and `b.CheckOpBurnerBalanceBounds(...)`
    // (~L241). These are NOT wired into the executor's per-tx path yet:
    // `worstcase::check_sender_balance_bound` exists but needs the per-tx
    // `StateRead` handles the driver owns (a driver-interface extension, best
    // done with the C-Chain hook bodies in M7.21+). M7.27 proved the bound is
    // never violated over the property space via the `Op` seam directly
    // (`worstcase/tests/bounds_prop.rs`, specs/11 §12); the consensus-load-bearing
    // base-fee bound (step 5) IS enforced above.
    let ops = hooks.end_of_block_ops(block)?;
    let outcome = driver.execute_block(block, parent_root, base_fee, &ops)?;

    // (6, continued) Tick the per-tx clock by each receipt's gas and advance the
    // block's interim execution time (Go: per-tx `perTxClock.Tick` +
    // `b.SetInterimExecutionTime`). This is the monotonic mid-block instant the
    // settlement layer (M7.17) reads, so it MUST be ticked tx-by-tx, not in bulk.
    for receipt in &outcome.receipts {
        per_tx_clock.tick(receipt.gas_used);
        block.set_interim_execution_time(per_tx_clock.clone());
    }

    // (8) The after-block hook is a no-op for the pure-EVM path (M7.21).

    // (9) Advance the gas clock past this block (Go: `gasClock.AfterBlock`).
    let (target, gas_cfg) = hooks.gas_config_after(&sealed_header);
    gas_clock.after_block(outcome.gas_used, target.0, gas_cfg);
    let interim_execution_time = gas_clock.time();

    // (10) Commit in strict D→M→I→X / CC-ORDER (specs/27 §2.4). The settled root
    // is the block's worst-case-settled root; under the pure-EVM path with no
    // separate settlement frontier it equals the post-execution root (archival
    // commits the execution root every block, interval commits the settled root
    // on a boundary). The tracker's `maybe_commit` performs the durable Firewood
    // commit BEFORE we advance any consensus pointer.
    let post_state_root = outcome.post_state_root;
    tracker.maybe_commit(post_state_root, post_state_root, block.height())?;
    // Retain the committed revision in the consensus-critical window.
    tracker.track(post_state_root);

    // Build the persisted execution results blob (the `D` artefact).
    let results = ExecutionResults {
        gas_time: gas_clock.time(),
        base_fee,
        receipt_root: outcome.receipt_root,
        post_state_root,
    };

    // `Block::mark_executed` itself runs D→M→I→X (disk-persist closure → set the
    // execution pointer → interim time → executed notify + advance
    // `last_executed_ptr`). The default `mark_executed` persist closure is a
    // no-op `Ok(())`; the rawdb write lands with the VM (M7.18).
    block.mark_executed(
        ExecutionArtefacts {
            results,
            interim_execution_time,
        },
        Some(last_executed_ptr),
    )?;

    Ok(StepOutput {
        receipts: outcome.receipts,
        post_state_root,
        base_fee,
        gas_time: gas_clock,
        gas_used: outcome.gas_used,
    })
}
