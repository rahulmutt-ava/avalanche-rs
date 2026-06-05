// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Set<T>` — a `HashSet`-like with the convenience API Go uses.
//!
//! Mirrors Go `utils/set`. Where Go serializes set contents, callers sort first
//! (see `specs/00` §6.1) — [`Set::sorted_list`] provides the deterministic order.
//! Owning spec: `specs/03-core-primitives.md` §4.2.

use std::collections::HashSet;
use std::hash::Hash;

/// A set with the convenience API mirrored from Go `utils/set.Set`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Set<T: Eq + Hash> {
    inner: HashSet<T>,
}

impl<T: Eq + Hash + Clone> Set<T> {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HashSet::new(),
        }
    }

    /// Builds a set from the given elements (Go `set.Of`).
    pub fn of<I: IntoIterator<Item = T>>(items: I) -> Self {
        Self {
            inner: items.into_iter().collect(),
        }
    }

    /// Adds `v`; returns `true` if it was newly inserted (Go `Add`).
    pub fn add(&mut self, v: T) -> bool {
        self.inner.insert(v)
    }

    /// Removes `v`; returns `true` if it was present (Go `Remove`).
    pub fn remove(&mut self, v: &T) -> bool {
        self.inner.remove(v)
    }

    /// Reports whether `v` is in the set (Go `Contains`).
    #[must_use]
    pub fn contains(&self, v: &T) -> bool {
        self.inner.contains(v)
    }

    /// Reports whether the two sets share any element (Go `Overlaps`).
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        if self.inner.len() <= other.inner.len() {
            self.inner.iter().any(|v| other.inner.contains(v))
        } else {
            other.inner.iter().any(|v| self.inner.contains(v))
        }
    }

    /// The number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Reports whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The elements in arbitrary order (Go `List`).
    #[must_use]
    pub fn list(&self) -> Vec<T> {
        self.inner.iter().cloned().collect()
    }
}

impl<T: Eq + Hash + Clone + Ord> Set<T> {
    /// The elements sorted ascending (Go `SortedList`) — deterministic, used
    /// before serializing per `specs/00` §6.1.
    #[must_use]
    pub fn sorted_list(&self) -> Vec<T> {
        let mut v: Vec<T> = self.inner.iter().cloned().collect();
        v.sort();
        v
    }
}
