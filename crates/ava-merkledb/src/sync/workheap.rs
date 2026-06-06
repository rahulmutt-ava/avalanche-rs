// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Work-splitting priority queue for the sync driver (spec 19 §4.1/§4.2).
//!
//! Faithful port of Go `database/merkle/sync/workheap.go`. A [`WorkHeap`] holds
//! [`WorkItem`] ranges that **never overlap**; it supports priority-ordered
//! [`WorkHeap::get_work`] and range coalescing via [`WorkHeap::merge_insert`].
//!
//! Go keeps two views over the *same* pointers: a max-heap by priority and a
//! BTree ordered by range start (a `Nothing` start sorts smallest). Rust can't
//! share a mutable pointer between two containers under `#![forbid(unsafe_code)]`
//! without `Rc<RefCell>`, so we keep a **single** canonical store — a
//! [`BTreeMap`] keyed by [`RangeStart`] (None sorts smallest) — and derive the
//! priority pop by a linear scan. The keyspace is tiny (bounded by
//! `2 * SimultaneousWorkLimit`), so the scan is cheap and the behavior is
//! identical: items still never overlap, merge the same way, and pop highest
//! priority (ties broken by insertion order, matching Go's `heap.Set`).

use std::collections::BTreeMap;

use ava_types::id::Id;

/// Work-item priority. `Retry > High > Med > Low` (Go `priority` constants;
/// `heap.Pop` returns the highest).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Lowest priority — a freshly-split sub-range tail.
    Low = 1,
    /// Medium priority — the leading half of a split range.
    Med = 2,
    /// High priority — re-queued processed work after a target advance.
    High = 3,
    /// Highest priority — a failed item being retried.
    Retry = 4,
}

/// A range `[start, end]` of the keyspace to fetch (spec 19 §4.1). `None`
/// bounds are unbounded (`Maybe.Nothing`): `start = None` means no lower bound,
/// `end = None` means no upper bound. `local_root == Id::EMPTY` means the range
/// was never downloaded -> fetch a range proof; otherwise -> change proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkItem {
    /// Lower bound (inclusive), or `None` for unbounded-below.
    pub start: Option<Vec<u8>>,
    /// Upper bound (inclusive), or `None` for unbounded-above.
    pub end: Option<Vec<u8>>,
    /// Fetch priority.
    pub priority: Priority,
    /// The root of this range in our local DB (`Id::EMPTY` ⇒ never downloaded).
    pub local_root: Id,
    /// Number of failed attempts (drives retry backoff).
    pub attempt: u32,
    /// Monotonic insertion sequence (FIFO tie-break for equal priority).
    seq: u64,
}

impl WorkItem {
    /// A new item over `[start, end]`.
    #[must_use]
    pub fn new(
        local_root: Id,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        priority: Priority,
    ) -> WorkItem {
        WorkItem {
            start,
            end,
            priority,
            local_root,
            attempt: 0,
            seq: 0,
        }
    }

    /// The whole keyspace `[Nothing, Nothing]` at `Id::EMPTY` (the seed item).
    #[must_use]
    pub fn whole_keyspace(priority: Priority) -> WorkItem {
        WorkItem::new(Id::EMPTY, None, None, priority)
    }

    /// Records a failed attempt (with the Go overflow guard).
    pub fn request_failed(&mut self) {
        self.attempt = self.attempt.saturating_add(1);
    }
}

/// Ordering key over a range start where `None` (Nothing) sorts smallest.
/// Mirrors the Go BTree comparator.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RangeStart {
    /// `Maybe.Nothing` — smaller than every concrete start.
    Nothing,
    /// A concrete lower bound.
    Some(Vec<u8>),
}

impl RangeStart {
    fn from_opt(start: &Option<Vec<u8>>) -> RangeStart {
        match start {
            None => RangeStart::Nothing,
            Some(v) => RangeStart::Some(v.clone()),
        }
    }
}

/// A non-overlapping range priority queue (spec 19 §4.2).
#[derive(Debug, Default)]
pub struct WorkHeap {
    /// Canonical store, keyed by range start (None sorts smallest). Ranges never
    /// overlap, so the start uniquely identifies an item.
    items: BTreeMap<RangeStart, WorkItem>,
    /// Insertion counter for FIFO tie-breaking among equal priorities.
    next_seq: u64,
    /// Once closed, inserts are dropped and `get_work` returns `None`.
    closed: bool,
}

impl WorkHeap {
    /// A fresh, empty heap.
    #[must_use]
    pub fn new() -> WorkHeap {
        WorkHeap::default()
    }

    /// Marks the heap closed (Go `Close`).
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Number of queued items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the heap holds no work.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds `item` without merging (Go `Insert`).
    pub fn insert(&mut self, mut item: WorkItem) {
        if self.closed {
            return;
        }
        item.seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.items.insert(RangeStart::from_opt(&item.start), item);
    }

    /// Pops and returns the highest-priority item (Go `GetWork`). Ties are
    /// broken by insertion order (smallest `seq` first), matching `heap.Set`.
    /// Returns `None` if closed or empty.
    pub fn get_work(&mut self) -> Option<WorkItem> {
        if self.closed || self.items.is_empty() {
            return None;
        }
        // Pick max priority, then min seq.
        let key = self
            .items
            .iter()
            .max_by(|(_, a), (_, b)| a.priority.cmp(&b.priority).then_with(|| b.seq.cmp(&a.seq)))
            .map(|(k, _)| k.clone())?;
        self.items.remove(&key)
    }

    /// Inserts `item`, merging it with existing items that share a boundary
    /// **and** the same `local_root` (Go `MergeInsert`). E.g. `[0,10]` already
    /// present and `[10,20]` inserted -> `[0,20]`; with both `[0,10]` and
    /// `[20,30]` present, inserting `[10,20]` -> `[0,30]`.
    pub fn merge_insert(&mut self, item: WorkItem) {
        if self.closed {
            return;
        }

        let start_key = RangeStart::from_opt(&item.start);

        // Find the item with the greatest start strictly less than item.start
        // whose end == item.start and same root (merge before).
        let before_key = self
            .items
            .range(..start_key.clone())
            .next_back()
            .filter(|(_, before)| {
                before.local_root == item.local_root && bound_eq(&before.end, &item.start)
            })
            .map(|(k, _)| k.clone());

        // Find the item with the smallest start >= item.start whose start ==
        // item.end and same root (merge after).
        let after_key = self
            .items
            .range(start_key..)
            .next()
            .filter(|(_, after)| {
                after.local_root == item.local_root && bound_eq(&item.end, &after.start)
            })
            .map(|(k, _)| k.clone());

        match (before_key, after_key) {
            (Some(bk), Some(ak)) => {
                // Merge both: before.end = after.end, drop after.
                let after = self
                    .items
                    .remove(&ak)
                    .unwrap_or_else(|| unreachable!("after key just located"));
                if let Some(before) = self.items.get_mut(&bk) {
                    before.end = after.end;
                    before.priority = before.priority.max(item.priority).max(after.priority);
                }
            }
            (Some(bk), None) => {
                if let Some(before) = self.items.get_mut(&bk) {
                    before.end = item.end.clone();
                    before.priority = before.priority.max(item.priority);
                }
            }
            (None, Some(ak)) => {
                // after.start = item.start; the BTree key changes, so re-key.
                let mut after = self
                    .items
                    .remove(&ak)
                    .unwrap_or_else(|| unreachable!("after key just located"));
                after.start = item.start.clone();
                after.priority = after.priority.max(item.priority);
                self.items.insert(RangeStart::from_opt(&after.start), after);
            }
            (None, None) => self.insert(item),
        }
    }

    /// Approximate fraction (0..=100) of the keyspace queued for `root`,
    /// truncating keys to their leading 8 bytes (Go `KeyspacePercent`).
    #[must_use]
    pub fn keyspace_percent(&self, root: Id) -> f64 {
        let mut progress: u64 = 0;
        for item in self.items.values() {
            if item.local_root != root {
                continue;
            }
            let start = progress_from_key(item.start.as_deref());
            let mut end = progress_from_key(item.end.as_deref());
            if end == 0 {
                end = u64::MAX;
            }
            progress = progress.saturating_add(end.saturating_sub(start));
        }
        (progress as f64 / u64::MAX as f64) * 100.0
    }
}

/// Equality of two `Option` bounds (Go `maybe.Equal(..., bytes.Equal)`):
/// `Nothing == Nothing`, and `Some(a) == Some(b)` iff bytes equal.
fn bound_eq(a: &Option<Vec<u8>>, b: &Option<Vec<u8>>) -> bool {
    a == b
}

/// Maps a key's leading 8 bytes to a `u64` for progress accounting (Go
/// `timer.ProgressFromHash`): big-endian over the first 8 bytes, zero-padded.
fn progress_from_key(key: Option<&[u8]>) -> u64 {
    let Some(key) = key else {
        return 0;
    };
    let mut buf = [0u8; 8];
    let n = key.len().min(8);
    buf[..n].copy_from_slice(&key[..n]);
    u64::from_be_bytes(buf)
}
