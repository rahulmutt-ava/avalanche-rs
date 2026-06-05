// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Bag<T>` multiset (threshold bookkeeping) + `UniqueBag`.
//!
//! Mirrors Go `utils/bag.Bag` (a `HashMap<T, usize>` multiset with a `threshold`
//! and a `met_threshold` set tracking elements whose count ≥ threshold — used by
//! Snowball vote counting) and `utils/bag.UniqueBag` (`HashMap<T, Bits>`).
//! Owning spec: `specs/03-core-primitives.md` §4.2.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use crate::bits::Bits;

/// A multiset with threshold bookkeeping (Go `bag.Bag`).
#[derive(Debug, Clone)]
pub struct Bag<T> {
    counts: HashMap<T, usize>,
    size: usize,
    threshold: usize,
    met_threshold: HashSet<T>,
}

impl<T: Eq + Hash + Clone> Default for Bag<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Eq + Hash + Clone> Bag<T> {
    /// An empty bag with `threshold` 0 (every element immediately meets it once
    /// added, matching Go's default `threshold == 0`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            counts: HashMap::new(),
            size: 0,
            threshold: 0,
            met_threshold: HashSet::new(),
        }
    }

    /// Builds a bag from the given elements (Go `bag.Of`).
    pub fn of<I: IntoIterator<Item = T>>(items: I) -> Self {
        let mut b = Self::new();
        for v in items {
            b.add(v);
        }
        b
    }

    /// Sets the threshold and recomputes the met-threshold set (Go `SetThreshold`).
    pub fn set_threshold(&mut self, threshold: usize) {
        if self.threshold == threshold {
            return;
        }
        self.threshold = threshold;
        self.met_threshold.clear();
        for (v, &c) in &self.counts {
            if c >= threshold {
                self.met_threshold.insert(v.clone());
            }
        }
    }

    /// Adds one occurrence of `v` (Go `Add`).
    pub fn add(&mut self, v: T) {
        self.add_count(v, 1);
    }

    /// Adds `count` occurrences of `v` (Go `AddCount`).
    pub fn add_count(&mut self, v: T, count: usize) {
        if count == 0 {
            return;
        }
        let total = self.counts.entry(v.clone()).or_insert(0);
        *total += count;
        self.size += count;
        if *total >= self.threshold {
            self.met_threshold.insert(v);
        }
    }

    /// The number of occurrences of `v` (Go `Count`).
    #[must_use]
    pub fn count(&self, v: &T) -> usize {
        self.counts.get(v).copied().unwrap_or(0)
    }

    /// The total number of occurrences across all elements (Go `Len`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.size
    }

    /// Reports whether the bag is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// The distinct elements (Go `List`).
    #[must_use]
    pub fn list(&self) -> Vec<T> {
        self.counts.keys().cloned().collect()
    }

    /// The element with the highest count (Go `Mode`), with its count. `None`
    /// if the bag is empty. Ties resolve to an arbitrary maximal element.
    #[must_use]
    pub fn mode(&self) -> Option<(T, usize)> {
        self.counts
            .iter()
            .max_by_key(|&(_, &c)| c)
            .map(|(v, &c)| (v.clone(), c))
    }

    /// The elements whose count meets the current threshold (Go `Threshold`).
    #[must_use]
    pub fn threshold(&self) -> Vec<T> {
        self.met_threshold.iter().cloned().collect()
    }
}

/// Maps each element to the set of voter indices that voted for it (Go
/// `bag.UniqueBag`, a `HashMap<T, set.Bits>`).
#[derive(Debug, Clone, Default)]
pub struct UniqueBag<T> {
    inner: HashMap<T, Bits>,
}

impl<T: Eq + Hash + Clone> UniqueBag<T> {
    /// An empty unique bag.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Records that voter `index` voted for `v` (Go `Add`).
    pub fn add(&mut self, index: u64, v: T) {
        self.inner.entry(v).or_default().add(index);
    }

    /// The set of voters for `v` (Go `GetSet`).
    #[must_use]
    pub fn get_set(&self, v: &T) -> Option<&Bits> {
        self.inner.get(v)
    }

    /// The distinct elements that received at least one vote (Go `List`).
    #[must_use]
    pub fn list(&self) -> Vec<T> {
        self.inner.keys().cloned().collect()
    }

    /// The number of distinct voted-for elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Reports whether the unique bag is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}
