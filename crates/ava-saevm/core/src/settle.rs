// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The settlement driver: advance `LastSettled` on the gas-time clock (specs/11
//! §1.2). Port of the settle half of `vms/saevm/blocks/settlement.go` as driven
//! by acceptance.
//!
//! When a block `b` is accepted, the blocks it *settles* are the contiguous,
//! increasing-height range of ancestors that finished executing no later than
//! `BlockTime(b) − Tau`, measured on the gas-time clock (specs/11 §1.2). The
//! candidate boundary is chosen by [`last_to_settle_at`]: it returns
//! `(candidate, known)`, where `known == false` means the executor has not
//! progressed far enough to decide (a block that *might* have settled has not
//! executed yet) — [`settle`] surfaces this as [`SettleError::ExecutionLagging`]
//! and the caller retries later, rather than settling prematurely.

use std::sync::Arc;
use std::time::SystemTime;

use ava_saevm_blocks::{Block, Range, last_to_settle_at};
use ava_saevm_params::TAU;

use crate::frontier::Frontier;

/// A failure of the [`settle`] driver. Mirrors Go's `ErrExecutionLagging`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettleError {
    /// Execution has not progressed far enough to decide what `b` settles: a
    /// block whose build time is `<= BlockTime(b) − Tau` (so it *might* have
    /// settled) has not finished executing. The caller retries once execution
    /// catches up. Mirrors Go's `ErrExecutionLagging`.
    #[error("execution lagging: cannot yet determine the settlement frontier")]
    ExecutionLagging,
    /// Marking an ancestor settled failed (e.g. it was already settled). Wraps
    /// the underlying [`ava_saevm_blocks::Error`] message.
    #[error("marking ancestor settled: {0}")]
    MarkSettled(String),
}

/// Advances the [`Frontier`]'s `LastSettled` pointer on behalf of the
/// freshly-accepted block `accepted`, marking every newly-settled ancestor
/// (`Σ`) settled in **increasing height** order (specs/11 §10 invariant 5).
///
/// The settlement instant is `BlockTime(accepted) − Tau` (the Tau discipline,
/// saturating at the UNIX epoch); the last ancestor finished executing no later
/// than that instant becomes the new `LastSettled`. Returns the newly-settled
/// blocks in increasing height order (empty if nothing newly settled).
///
/// # Errors
/// [`SettleError::ExecutionLagging`] when [`last_to_settle_at`] reports
/// `known == false` (the executor has not caught up); the frontier is left
/// untouched. [`SettleError::MarkSettled`] if an ancestor could not be marked
/// settled.
pub fn settle(frontier: &Frontier, accepted: &Arc<Block>) -> Result<Vec<Arc<Block>>, SettleError> {
    // The block being accepted settles `(parent.LastSettled, candidate]`. A
    // block with no parent (genesis / synchronous) settles nothing new here.
    let Some(parent) = accepted.parent_block() else {
        return Ok(Vec::new());
    };

    // settle_at = BlockTime(accepted) − Tau (saturating at the epoch). Mirrors
    // `block_builder.go::lastToSettle`'s `bTime.Add(-saeparams.Tau)`.
    let settle_at = accepted
        .timestamp()
        .checked_sub(TAU)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    // The last ancestor provably finished executing by settle_at, with a
    // `known` flag. `known == false` => execution is lagging; do not settle.
    let (candidate, known) = last_to_settle_at(settle_at, &parent)
        .map_err(|e| SettleError::MarkSettled(e.to_string()))?;
    if !known {
        return Err(SettleError::ExecutionLagging);
    }
    let Some(candidate) = candidate else {
        return Ok(Vec::new());
    };

    // The newly-settled range is `(current LastSettled, candidate]`, walked in
    // increasing height. `candidate` at or below the current S is a no-op.
    let current = frontier.last_settled();
    if candidate.height() <= current.height() {
        return Ok(Vec::new());
    }
    let range = Range::between(Some(current), Some(candidate));

    let mut newly = Vec::with_capacity(range.len());
    for block in range.iter() {
        // Already-settled blocks (e.g. genesis) are skipped, not an error.
        if block.settled() {
            continue;
        }
        block
            .mark_settled(None)
            .map_err(|e| SettleError::MarkSettled(e.to_string()))?;
        // Advance the frontier pointer + evict below-S blocks from the
        // consensus-critical map (increasing-height order is the loop order).
        frontier.advance_settled(block);
        newly.push(Arc::clone(block));
    }
    Ok(newly)
}
