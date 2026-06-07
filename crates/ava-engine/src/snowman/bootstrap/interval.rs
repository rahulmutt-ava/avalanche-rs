// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The bootstrap interval tree (port of
//! `snow/engine/snowman/bootstrap/interval/`, specs 06 §4.3).
//!
//! A [`Tree`] tracks a set of block heights as a union of contiguous
//! [`Interval`]s, so a continuously-fetched range takes `O(ranges)` space rather
//! than `O(heights)`. `add`/`remove`/`contains` run in `O(log ranges)`.
//!
//! ## Port note (in-memory)
//!
//! Go's `interval.Tree` is database-backed (it persists each interval to a
//! `database.KeyValueWriterDeleter` so bootstrap can resume after a restart).
//! This port keeps the tree purely in-memory (the engine drives a single
//! uninterrupted bootstrap pass), so the DB-write side effects are dropped. The
//! `Add`/`Remove`/`Contains`/`Flatten`/`Len` semantics are byte-for-byte faithful
//! to the Go btree logic. The fetched block bytes are held in a sibling
//! `BTreeMap<height, bytes>` ([`Blocks`]) in place of Go's `interval.PutBlock`.

use std::collections::BTreeMap;

/// A closed height range `[lower_bound, upper_bound]` (Go `interval.Interval`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval {
    /// Inclusive lower bound.
    pub lower_bound: u64,
    /// Inclusive upper bound.
    pub upper_bound: u64,
}

impl Interval {
    fn contains(&self, height: u64) -> bool {
        self.lower_bound <= height && height <= self.upper_bound
    }

    fn adjacent_to_lower_bound(&self, height: u64) -> bool {
        height < u64::MAX && height.saturating_add(1) == self.lower_bound
    }

    fn adjacent_to_upper_bound(&self, height: u64) -> bool {
        self.upper_bound < u64::MAX && self.upper_bound.saturating_add(1) == height
    }
}

/// A set of heights tracked as contiguous intervals (Go `interval.Tree`).
///
/// Intervals are keyed by their `upper_bound` in a `BTreeMap` (Go keys its btree
/// by `(*Interval).Less`, which compares `UpperBound`), so they are
/// non-overlapping and totally ordered.
#[derive(Debug, Default)]
pub struct Tree {
    /// `upper_bound -> Interval`.
    known: BTreeMap<u64, Interval>,
    /// Number of heights (not intervals) in the tree. Overflows to 0 for the
    /// full `[0, MaxU64]` range, matching Go.
    num_known: u64,
}

impl Tree {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The smallest interval whose `upper_bound >= height` (Go
    /// `AscendGreaterOrEqual` first hit).
    fn ceiling(&self, height: u64) -> Option<Interval> {
        self.known.range(height..).next().map(|(_, i)| *i)
    }

    /// The largest interval whose `upper_bound <= height` (Go
    /// `DescendLessOrEqual` first hit).
    fn floor(&self, height: u64) -> Option<Interval> {
        self.known.range(..=height).next_back().map(|(_, i)| *i)
    }

    /// Adds `height` to the set, merging/extending intervals as needed.
    pub fn add(&mut self, height: u64) {
        // `upper` is the smallest interval with upper_bound >= height.
        let upper = self.ceiling(height);
        if let Some(u) = upper
            && u.contains(height)
        {
            return; // already present
        }
        // `lower` is the largest interval with upper_bound <= height.
        let lower = self.floor(height);

        self.num_known = self.num_known.wrapping_add(1);

        let adjacent_to_lower = upper.is_some_and(|u| u.adjacent_to_lower_bound(height));
        let adjacent_to_upper = lower.is_some_and(|l| l.adjacent_to_upper_bound(height));

        match (adjacent_to_lower, adjacent_to_upper) {
            (true, true) => {
                // Merge the lower range into the upper range.
                let lower = lower.expect("adjacent_to_upper implies lower");
                let mut upper = upper.expect("adjacent_to_lower implies upper");
                self.known.remove(&lower.upper_bound);
                upper.lower_bound = lower.lower_bound;
                self.known.insert(upper.upper_bound, upper);
            }
            (true, false) => {
                // Extend the upper range down by one.
                let mut upper = upper.expect("adjacent_to_lower implies upper");
                upper.lower_bound = height;
                self.known.insert(upper.upper_bound, upper);
            }
            (false, true) => {
                // Extend the lower range up by one (its key, upper_bound, moves).
                let mut lower = lower.expect("adjacent_to_upper implies lower");
                self.known.remove(&lower.upper_bound);
                lower.upper_bound = height;
                self.known.insert(lower.upper_bound, lower);
            }
            (false, false) => {
                self.known.insert(
                    height,
                    Interval {
                        lower_bound: height,
                        upper_bound: height,
                    },
                );
            }
        }
    }

    /// Removes `height` from the set, splitting an interval if it is interior.
    pub fn remove(&mut self, height: u64) {
        let higher = match self.ceiling(height) {
            Some(h) if h.contains(height) => h,
            _ => return, // not in the tree
        };

        self.num_known = self.num_known.wrapping_sub(1);

        if higher.lower_bound == higher.upper_bound {
            self.known.remove(&higher.upper_bound);
        } else if higher.lower_bound == height {
            let mut h = higher;
            h.lower_bound = h.lower_bound.saturating_add(1);
            self.known.insert(h.upper_bound, h);
        } else if higher.upper_bound == height {
            self.known.remove(&higher.upper_bound);
            let mut h = higher;
            h.upper_bound = h.upper_bound.saturating_sub(1);
            self.known.insert(h.upper_bound, h);
        } else {
            // Interior removal: split into `[lower, height-1]` and `[height+1, upper]`.
            let lower_part = Interval {
                lower_bound: higher.lower_bound,
                upper_bound: height.saturating_sub(1),
            };
            self.known.insert(lower_part.upper_bound, lower_part);
            let mut upper_part = higher;
            upper_part.lower_bound = height.saturating_add(1);
            self.known.insert(upper_part.upper_bound, upper_part);
        }
    }

    /// Whether `height` is in the set.
    #[must_use]
    pub fn contains(&self, height: u64) -> bool {
        self.ceiling(height).is_some_and(|i| i.contains(height))
    }

    /// All intervals in ascending order.
    #[must_use]
    pub fn flatten(&self) -> Vec<Interval> {
        self.known.values().copied().collect()
    }

    /// The number of heights tracked (not the number of intervals).
    #[must_use]
    pub fn len(&self) -> u64 {
        self.num_known
    }

    /// Whether the tree tracks no heights.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.num_known == 0 && self.known.is_empty()
    }
}

/// The fetched block bytes keyed by height (replaces Go's `interval.PutBlock`).
#[derive(Debug, Default)]
pub struct Blocks {
    by_height: BTreeMap<u64, Vec<u8>>,
}

impl Blocks {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `bytes` at `height`.
    pub fn put(&mut self, height: u64, bytes: Vec<u8>) {
        self.by_height.insert(height, bytes);
    }

    /// The bytes stored at `height`, if any.
    #[must_use]
    pub fn get(&self, height: u64) -> Option<&[u8]> {
        self.by_height.get(&height).map(Vec::as_slice)
    }

    /// Removes and returns the bytes at `height`.
    pub fn remove(&mut self, height: u64) -> Option<Vec<u8>> {
        self.by_height.remove(&height)
    }

    /// Heights in ascending order.
    #[must_use]
    pub fn heights(&self) -> Vec<u64> {
        self.by_height.keys().copied().collect()
    }
}

/// Adds a block at `height` to the tree, returning whether the parent (at
/// `height-1`) should now be fetched (Go `interval.Add`).
///
/// Returns `false` if `height` is at or below `last_accepted_height` or already
/// tracked.
pub fn add_block(
    tree: &mut Tree,
    blocks: &mut Blocks,
    last_accepted_height: u64,
    height: u64,
    blk_bytes: Vec<u8>,
) -> bool {
    if height <= last_accepted_height || tree.contains(height) {
        return false;
    }
    blocks.put(height, blk_bytes);
    tree.add(height);

    // height > last_accepted_height here, so height-1 cannot underflow.
    let next_height = height.saturating_sub(1);
    next_height != last_accepted_height && !tree.contains(next_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_merge_extend() {
        let mut t = Tree::new();
        t.add(4);
        t.add(6);
        t.add(7);
        assert_eq!(t.len(), 3);
        assert_eq!(t.flatten().len(), 2); // {4}, {6,7}

        // Adding 5 merges {4} and {6,7} into {4,5,6,7}.
        t.add(5);
        assert_eq!(t.len(), 4);
        let flat = t.flatten();
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].lower_bound, 4);
        assert_eq!(flat[0].upper_bound, 7);
    }

    #[test]
    fn contains_and_remove_interior() {
        let mut t = Tree::new();
        for h in 1..=5 {
            t.add(h);
        }
        assert!(t.contains(3));
        // Remove the interior height 3 -> {1,2} and {4,5}.
        t.remove(3);
        assert!(!t.contains(3));
        assert_eq!(t.len(), 4);
        assert_eq!(t.flatten().len(), 2);
    }

    #[test]
    fn add_block_signals_parent_fetch() {
        let mut t = Tree::new();
        let mut b = Blocks::new();
        // last_accepted = 0; add height 3 -> parent (2) should be fetched.
        assert!(add_block(&mut t, &mut b, 0, 3, vec![3]));
        assert_eq!(b.get(3), Some(&[3][..]));
        // Add height 2 -> parent (1) should be fetched (1 != lastAccepted 0).
        assert!(add_block(&mut t, &mut b, 0, 2, vec![2]));
        // Add height 1 -> parent (0) == lastAccepted: no more fetch.
        assert!(!add_block(&mut t, &mut b, 0, 1, vec![1]));
        // Re-adding a tracked height is a no-op (false).
        assert!(!add_block(&mut t, &mut b, 0, 2, vec![2]));
    }
}
