// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M1.19 property tests for the sync work-heap (spec 19 §8):
//!
//! - `prop::workheap_invariants` — partition the keyspace into disjoint adjacent
//!   segments, insert them (shuffled) via `merge_insert`, and assert:
//!   (1) the heap's ranges never overlap, (2) their union still covers the whole
//!   original span (no gaps — full coverage), and (3) same-root adjacent
//!   segments are coalesced (the count never exceeds the number of distinct
//!   root-runs). Also checks `get_work` pops in non-increasing priority order.
//!
//! `merge_insert` itself only ever *coalesces* boundary-adjacent same-root
//! ranges; it does not de-overlap arbitrary inserts (the syncer only ever feeds
//! it disjoint ranges from its split logic — Go `workheap.go` comment "work item
//! ranges never overlap"). So the property is stated over a disjoint partition,
//! exactly the contract the heap is built for.
//!
//! Gated on the `sync` feature; the green gate / nextest run `--all-features`.

#![cfg(feature = "sync")]

use proptest::collection::vec;
use proptest::prelude::*;

use ava_merkledb::sync::workheap::{Priority, WorkHeap, WorkItem};
use ava_types::id::Id;

fn priority() -> impl Strategy<Value = Priority> {
    prop_oneof![
        Just(Priority::Low),
        Just(Priority::Med),
        Just(Priority::High),
        Just(Priority::Retry),
    ]
}

/// One root of two, so adjacent segments sometimes share a root (mergeable) and
/// sometimes don't (kept separate).
fn root() -> impl Strategy<Value = Id> {
    prop_oneof![Just(Id::EMPTY), Just(Id::from([1u8; 32]))]
}

/// Encodes a numeric cut point in `1..=254` as a single-byte bound.
fn bound_at(cut: u8) -> Option<Vec<u8>> {
    Some(vec![cut])
}

fn start_pos(b: &Option<Vec<u8>>) -> u64 {
    b.as_deref().map_or(0, byte_pos)
}
fn end_pos(b: &Option<Vec<u8>>) -> u64 {
    b.as_deref().map_or(u64::MAX, byte_pos)
}
fn byte_pos(v: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    let n = v.len().min(8);
    buf[..n].copy_from_slice(&v[..n]);
    u64::from_be_bytes(buf)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn workheap_invariants(
        // Sorted distinct interior cut points partition [None, None] into segments.
        mut cuts in vec(1u8..=254, 0..6),
        roots in vec(root(), 0..8),
        prios in vec(priority(), 0..8),
        // A permutation seed to shuffle insertion order.
        perm in vec(any::<u64>(), 0..8),
    ) {
        cuts.sort_unstable();
        cuts.dedup();

        // Build the disjoint adjacent segments [None, c0], [c0, c1], ..., [cn, None].
        let mut bounds: Vec<Option<Vec<u8>>> = Vec::new();
        bounds.push(None);
        for c in &cuts {
            bounds.push(bound_at(*c));
        }
        bounds.push(None);

        let n_segments = bounds.len() - 1;
        let mut segments: Vec<WorkItem> = Vec::with_capacity(n_segments);
        for i in 0..n_segments {
            let r = roots.get(i).copied().unwrap_or(Id::EMPTY);
            let p = prios.get(i).copied().unwrap_or(Priority::Low);
            segments.push(WorkItem::new(r, bounds[i].clone(), bounds[i + 1].clone(), p));
        }

        // Shuffle the insertion order deterministically from `perm`.
        let mut order: Vec<usize> = (0..n_segments).collect();
        let len = order.len();
        if len > 0 {
            for (i, seed) in perm.iter().enumerate() {
                let a = i % len;
                let b = (*seed as usize) % len;
                order.swap(a, b);
            }
        }

        let mut heap = WorkHeap::new();
        for &i in &order {
            heap.merge_insert(segments[i].clone());
        }

        // Drain.
        let mut popped: Vec<WorkItem> = Vec::new();
        while let Some(w) = heap.get_work() {
            popped.push(w);
        }

        // (a) priority pop order is non-increasing.
        for w in popped.windows(2) {
            prop_assert!(w[0].priority >= w[1].priority);
        }

        // (b) ranges never overlap and (c) fully cover [0, MAX] with no gaps.
        let mut ranges: Vec<(u64, u64)> = popped
            .iter()
            .map(|w| (start_pos(&w.start), end_pos(&w.end)))
            .collect();
        ranges.sort();
        prop_assert!(!ranges.is_empty());
        prop_assert_eq!(ranges.first().map(|r| r.0), Some(0), "coverage must start at 0");
        prop_assert_eq!(ranges.last().map(|r| r.1), Some(u64::MAX), "coverage must end at MAX");
        for w in ranges.windows(2) {
            // adjacent (no gap) and non-overlapping: prev end == next start.
            prop_assert_eq!(w[0].1, w[1].0, "gap or overlap between merged ranges");
        }

        // (d) coalescing: the number of merged ranges never exceeds the number of
        // maximal same-root runs over the original ordered segments.
        let mut runs = 0usize;
        let mut prev_root: Option<Id> = None;
        for seg in &segments {
            if prev_root != Some(seg.local_root) {
                runs += 1;
                prev_root = Some(seg.local_root);
            }
        }
        prop_assert!(popped.len() <= runs, "expected <= {} runs, got {}", runs, popped.len());
    }
}
