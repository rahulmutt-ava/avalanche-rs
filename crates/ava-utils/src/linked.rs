// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `LinkedHashmap<K,V>` — insertion-ordered map; re-`put` moves key to back.
//!
//! Mirrors Go `utils/linked/hashmap.go`: an ordered O(1) map preserving
//! **insertion order** where `Put` on an existing key **moves it to the back**.
//! Implemented over [`indexmap::IndexMap`] with an explicit move-to-back. The
//! observable behavior, not Go's free-list, is what matters.
//! Owning spec: `specs/03-core-primitives.md` §4.3.

use std::hash::Hash;

use indexmap::IndexMap;

/// An insertion-ordered map. Re-inserting an existing key moves it to the back.
#[derive(Debug, Clone, Default)]
pub struct LinkedHashmap<K, V> {
    inner: IndexMap<K, V>,
}

impl<K: Hash + Eq, V> LinkedHashmap<K, V> {
    /// An empty map (Go `NewHashmap`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: IndexMap::new(),
        }
    }

    /// Inserts `k -> v`. If `k` already exists its value is replaced and the
    /// entry is moved to the back (most-recently-used position).
    pub fn put(&mut self, k: K, v: V) {
        use indexmap::map::Entry;
        match self.inner.entry(k) {
            Entry::Occupied(mut e) => {
                e.insert(v);
                let idx = e.index();
                let last = self.inner.len() - 1;
                if idx != last {
                    self.inner.move_index(idx, last);
                }
            }
            Entry::Vacant(e) => {
                e.insert(v);
            }
        }
    }

    /// Returns the value for `k`, if present (Go `Get`).
    pub fn get(&self, k: &K) -> Option<&V> {
        self.inner.get(k)
    }

    /// Reports whether `k` is present.
    pub fn contains(&self, k: &K) -> bool {
        self.inner.contains_key(k)
    }

    /// Removes `k`, returning its value if present (Go `Delete`).
    pub fn delete(&mut self, k: &K) -> Option<V> {
        // shift_remove preserves the order of the remaining entries.
        self.inner.shift_remove(k)
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Reports whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The oldest (front) entry (Go `Oldest`).
    #[must_use]
    pub fn oldest(&self) -> Option<(&K, &V)> {
        self.inner.first()
    }

    /// The newest (back) entry (Go `Newest`).
    #[must_use]
    pub fn newest(&self) -> Option<(&K, &V)> {
        self.inner.last()
    }

    /// Iterates keys in order (oldest → newest).
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.inner.keys()
    }

    /// Iterates `(key, value)` pairs in order (oldest → newest).
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.inner.iter()
    }
}
