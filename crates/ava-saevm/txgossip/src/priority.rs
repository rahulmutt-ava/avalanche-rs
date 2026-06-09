// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Effective-tip priority ordering over a set of mempool transactions
//! (`txgossip/priority.go`).
//!
//! Go's `TransactionsByPriority` orders pending txs so the block builder packs
//! the highest-paying work first: by **effective tip per gas** at the block's
//! base fee (descending), breaking ties deterministically. A tx whose
//! `effective_tip_per_gas(base_fee)` is `None` (it cannot pay the base fee) is
//! skipped entirely — it is not eligible for inclusion at this base fee.
//!
//! This module is generic over a local [`Priced`] trait rather than the concrete
//! reth tx, so the ordering — the headline testable unit — can be exercised with
//! synthetic fee/nonce inputs without constructing signed txs. The real
//! [`Transaction`](crate::Transaction) implements [`Priced`] via the
//! `ConsensusTx` facade (`effective_tip_per_gas` / `nonce`).

use core::cmp::Ordering;

/// A value that can be ordered for block inclusion by effective tip
/// (`txgossip/priority.go`).
///
/// `effective_tip` is the per-gas tip the tx pays *above* the block base fee
/// (EIP-1559 `min(max_priority_fee, max_fee - base_fee)`, or `gas_price -
/// base_fee` for a legacy tx). `None` means the tx cannot pay `base_fee` and is
/// ineligible at this base fee. `nonce` and `arrival` break ties deterministically.
pub trait Priced {
    /// Effective tip per gas at `base_fee`, or `None` if the tx cannot pay it.
    fn effective_tip(&self, base_fee: u64) -> Option<u128>;

    /// The account nonce — the secondary ordering key (ascending).
    fn nonce(&self) -> u64;
}

/// The deterministic ordering key for one eligible tx at a fixed base fee.
///
/// Ordering precedence (matching Go's mempool invariant, `02` §4):
/// 1. effective tip per gas, **descending** (higher tip first);
/// 2. nonce, **ascending**;
/// 3. arrival/insertion index, **ascending** (FIFO among otherwise-equal txs).
///
/// All three keys are integers (`u128`/`u64`/`usize`) — no float comparison, so
/// the order is a strict total order with deterministic tie-breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PriorityKey {
    tip: u128,
    nonce: u64,
    arrival: usize,
}

impl Ord for PriorityKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher tip is "greater" (pops first); then lower nonce, then lower
        // arrival index. Reverse the tip comparison so descending-tip becomes a
        // max-first total order while keeping a single canonical direction.
        other
            .tip
            .cmp(&self.tip)
            .then_with(|| self.nonce.cmp(&other.nonce))
            .then_with(|| self.arrival.cmp(&other.arrival))
    }
}

impl PartialOrd for PriorityKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An ordered, drainable view of pending txs by descending effective tip at a
/// fixed base fee (`txgossip/priority.go` `TransactionsByPriority`).
///
/// Construction filters out txs that cannot pay `base_fee` (`effective_tip ==
/// None`) and sorts the remainder by `PriorityKey`. [`Self::pop`] yields the
/// highest-priority tx first; the iterator yields the full order. The builder
/// (`sae/vm.go` block-build path) drains this until the gas budget is hit.
#[derive(Debug)]
pub struct TransactionsByPriority<T> {
    /// Eligible txs in descending-priority order (pop from the back is O(1); we
    /// keep them front-to-back highest-first and pop from the front via index).
    ordered: Vec<T>,
    /// Index of the next tx to pop (front of the priority order).
    cursor: usize,
}

impl<T: Priced> TransactionsByPriority<T> {
    /// Builds a priority view of `txs` at `base_fee`.
    ///
    /// Txs whose `effective_tip(base_fee)` is `None` are skipped (ineligible at
    /// this base fee). The remainder are sorted by (tip desc, nonce asc, arrival
    /// asc), where `arrival` is each tx's index in the input — so equal-priced
    /// txs keep their insertion order (FIFO), matching Go's stable ordering.
    #[must_use]
    pub fn new(txs: Vec<T>, base_fee: u64) -> Self {
        let mut keyed: Vec<(PriorityKey, T)> = txs
            .into_iter()
            .enumerate()
            .filter_map(|(arrival, tx)| {
                tx.effective_tip(base_fee).map(|tip| {
                    (
                        PriorityKey {
                            tip,
                            nonce: tx.nonce(),
                            arrival,
                        },
                        tx,
                    )
                })
            })
            .collect();
        keyed.sort_by_key(|(key, _)| *key);
        Self {
            ordered: keyed.into_iter().map(|(_, tx)| tx).collect(),
            cursor: 0,
        }
    }

    /// Pops the next-highest-priority tx, or `None` when drained.
    pub fn pop(&mut self) -> Option<&T> {
        let tx = self.ordered.get(self.cursor)?;
        self.cursor = self.cursor.saturating_add(1);
        Some(tx)
    }

    /// The number of eligible txs remaining (not yet popped).
    #[must_use]
    pub fn len(&self) -> usize {
        self.ordered.len().saturating_sub(self.cursor)
    }

    /// Whether all eligible txs have been popped (or none were eligible).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The full priority order as a slice (highest-priority first), ignoring the
    /// pop cursor. Used by the builder to inspect the whole candidate set.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.ordered
    }

    /// Consumes the view, returning the eligible txs in priority order.
    #[must_use]
    pub fn into_ordered(self) -> Vec<T> {
        self.ordered
    }
}
