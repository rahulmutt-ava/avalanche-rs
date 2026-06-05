// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! A bounded ring of recent change-sets keyed by root ID, so the DB can answer
//! change-proof requests. Byte-exact-in-behavior port of Go
//! `x/merkledb/history.go` (the trim/size bound; spec 04 §3.5).
//!
//! This is intentionally a thin recorder: it keeps, for each recorded root, the
//! per-key value changes (`before`/`after`) that produced it, bounded to the
//! most-recent `max_history_size` entries. Range/change-proof *construction*
//! from these records lands in M1.18.

use std::collections::{BTreeMap, VecDeque};

use bytes::Bytes;

use ava_types::id::Id;

use crate::key::Key;
use crate::maybe::Maybe;

/// A single key's value change between two consecutive roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyChange {
    /// The value before the change (`Nothing` ⇒ the key was absent).
    pub before: Maybe<Bytes>,
    /// The value after the change (`Nothing` ⇒ the key was deleted).
    pub after: Maybe<Bytes>,
}

/// The summary of one commit: the resulting root and the per-key changes that
/// produced it. Mirrors the relevant fields of Go `changeSummary`.
#[derive(Clone, Debug, Default)]
pub struct ChangeSummary {
    /// The root ID after applying these changes.
    pub root_id: Id,
    /// Per-key value changes (ascending by key).
    pub key_changes: BTreeMap<Key, KeyChange>,
}

/// A bounded ring of recent [`ChangeSummary`]s. Mirrors Go `trieHistory`.
pub struct History {
    history: VecDeque<ChangeSummary>,
    max_history_size: usize,
}

/// The default number of change-sets retained (mirrors Go `HistoryLength`'s
/// typical configured value; not protocol-relevant).
pub const DEFAULT_HISTORY_SIZE: usize = 300;

impl History {
    /// Creates a history retaining at most `max_history_size` change-sets.
    pub fn new(max_history_size: usize) -> Self {
        History {
            history: VecDeque::new(),
            max_history_size,
        }
    }

    /// Records `summary`, trimming the oldest entries beyond the size bound.
    pub fn record(&mut self, summary: ChangeSummary) {
        if self.max_history_size == 0 {
            return;
        }
        self.history.push_back(summary);
        while self.history.len() > self.max_history_size {
            self.history.pop_front();
        }
    }

    /// Returns the number of retained change-sets.
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// Returns `true` iff no change-sets are retained.
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// Returns the most-recently recorded change-set, if any.
    pub fn latest(&self) -> Option<&ChangeSummary> {
        self.history.back()
    }

    /// Returns the recorded change-set whose resulting root is `root_id`.
    pub fn get(&self, root_id: Id) -> Option<&ChangeSummary> {
        self.history.iter().rev().find(|c| c.root_id == root_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_size_bounded() {
        let mut h = History::new(2);
        for i in 0..5u8 {
            h.record(ChangeSummary {
                root_id: Id::from([i; 32]),
                key_changes: BTreeMap::new(),
            });
        }
        assert_eq!(h.len(), 2);
        // Only the two most-recent roots remain.
        assert!(h.get(Id::from([4; 32])).is_some());
        assert!(h.get(Id::from([3; 32])).is_some());
        assert!(h.get(Id::from([2; 32])).is_none());
    }

    #[test]
    fn zero_size_records_nothing() {
        let mut h = History::new(0);
        h.record(ChangeSummary::default());
        assert!(h.is_empty());
    }
}
