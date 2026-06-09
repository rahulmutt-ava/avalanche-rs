// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The settlement [`Range`] and `last_to_settle_at` (specs/11 §1.2/§4.2).
//!
//! Port of the range/settlement-choice half of `vms/saevm/blocks/settlement.go`
//! (`Range`, `Settles`, `LastToSettleAt`). The lifecycle (`mark_executed` /
//! `mark_settled`) half lives in `lifecycle.rs`.

use std::ops::Deref;
use std::sync::Arc;
use std::time::SystemTime;

use crate::{Block, Error};

/// A contiguous, height-ordered run of blocks — the set a block *settles*
/// (`(parent.last_settled, self.last_settled]`, specs/11 §1.2). Stored in
/// **increasing height order** and derefs to a `[Arc<Block>]` slice (so `len`,
/// `iter`, indexing, and `is_empty` come for free). Mirrors Go's
/// `blocks.Range`.
#[derive(Clone, Default)]
pub struct Range(Vec<Arc<Block>>);

impl Range {
    /// The half-open range `(from, to]`: every ancestor reachable from `to` by
    /// walking parent pointers, down to **but excluding** `from` (matched by
    /// `Arc` identity), returned in increasing height order.
    ///
    /// `to == from` (same `Arc`) yields the empty range; `from == None` walks
    /// all the way to the genesis (inclusive).
    #[must_use]
    pub fn between(from: Option<Arc<Block>>, to: Option<Arc<Block>>) -> Self {
        // Consume `from` into a raw identity pointer for the exclusive lower
        // bound (compared, never dereferenced — the `to`-chain keeps the block
        // alive while it matters, and a pointer-equality test is sound even if
        // the `from` allocation were released).
        let from_ptr: Option<*const Block> = from.map(|f| Arc::as_ptr(&f));
        let mut acc: Vec<Arc<Block>> = Vec::new();
        let mut cur = to;
        while let Some(b) = cur {
            // Stop at `from` (exclusive lower bound).
            if from_ptr == Some(Arc::as_ptr(&b)) {
                break;
            }
            // Fetch the parent before moving `b` into the accumulator.
            let parent = b.parent_block();
            acc.push(b);
            cur = parent;
        }
        acc.reverse(); // walked high->low; expose low->high.
        Self(acc)
    }

    /// The range containing exactly `block` (a synchronous block settles
    /// itself, specs/11 §1.2).
    #[must_use]
    pub fn singleton(block: Arc<Block>) -> Self {
        Self(vec![block])
    }

    /// The blocks in increasing height order.
    #[must_use]
    pub fn blocks(&self) -> &[Arc<Block>] {
        &self.0
    }
}

impl Deref for Range {
    type Target = [Arc<Block>];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The last ancestor of `parent` that finished executing **no later than**
/// `settle_at`, with a `known` flag (specs/11 §1.2, Go
/// `blocks/settlement.go::LastToSettleAt`).
///
/// Returns `(candidate, known)`:
/// * `known == true` — the result is final: either a settled candidate was
///   found, or the search reached the synchronous (genesis / pre-SAE) terminus,
///   or there is nothing to settle (`None`).
/// * `known == false` — execution is **lagging**: a block whose build time is
///   `<= settle_at` (so it *might* have settled) has not executed yet, so we
///   cannot decide. The builder reports `ErrExecutionLagging` and retries later
///   (specs/11 §1.2). The returned block is the nearest provably-settled
///   ancestor (the synchronous floor), which the builder ignores while lagging.
///
/// A block cannot finish executing before it was built, so its **build time** is
/// a lower bound on its execution-completion instant: when `build_time >
/// settle_at` the block provably did *not* settle by `settle_at` and is skipped
/// without consulting its (possibly absent) execution result. Only when
/// `build_time <= settle_at` do we need the actual execution **gas-time**
/// (specs/11 §1.2 — settlement is measured on the gas clock, not wall time).
///
/// # Errors
/// The `Result` mirrors Go's `LastToSettleAt(...) (*Block, bool, error)`
/// signature; the error channel is reserved for the consensus-integration
/// validation (M7.17/M7.18, e.g. a malformed ancestry chain), which the pure
/// lifecycle layer cannot yet trigger — it currently always returns `Ok`.
#[allow(
    clippy::unnecessary_wraps,
    reason = "public signature mirrors Go's (*Block, bool, error); the error \
              channel is wired by the consensus integration (M7.17/M7.18)"
)]
pub fn last_to_settle_at(
    settle_at: SystemTime,
    parent: &Arc<Block>,
) -> Result<(Option<Arc<Block>>, bool), Error> {
    let mut cur = Some(Arc::clone(parent));
    while let Some(b) = cur {
        // The synchronous (genesis / last pre-SAE) block is self-settling and
        // therefore always a known, final terminus.
        if b.is_synchronous() {
            return Ok((Some(b), true));
        }

        // build_time is a lower bound on execution completion: build_time >
        // settle_at ⇒ b provably did not finish by settle_at. Skip (known).
        if b.timestamp() > settle_at {
            cur = b.parent_block();
            continue;
        }

        // build_time <= settle_at: b might have settled — decide on the gas clock.
        match b.executed_by_gas_time() {
            Some(gas_time) => {
                if gas_time.as_time() <= settle_at {
                    // b finished no later than settle_at ⇒ it is the last to settle.
                    return Ok((Some(b), true));
                }
                // b finished too recently; an older ancestor may still qualify.
                cur = b.parent_block();
            }
            None => {
                // Execution lagging: cannot decide. Report the synchronous floor
                // with known=false (the builder retries — ErrExecutionLagging).
                return Ok((nearest_settled_floor(b.parent_block()), false));
            }
        }
    }
    Ok((None, true))
}

/// Walks down from `start` to the nearest synchronous (always-settled) ancestor,
/// the safe lower bound reported when execution is lagging.
fn nearest_settled_floor(start: Option<Arc<Block>>) -> Option<Arc<Block>> {
    let mut cur = start;
    while let Some(b) = cur {
        if b.is_synchronous() {
            return Some(b);
        }
        cur = b.parent_block();
    }
    None
}
