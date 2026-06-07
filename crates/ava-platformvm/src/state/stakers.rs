// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! In-memory staker collections (`vms/platformvm/state/stakers.go`, specs 08 §3.3).
//!
//! [`Stakers`] holds the current and pending stakers in two `BTreeSet<Staker>`
//! (ordered by the `Staker` Less comparator) plus a per-`(subnet, node)` lookup
//! map for validators. This is the `baseStakers` structure; the diff overlay
//! (`diffStakers`) lives in [`Diff`](super::diff::Diff).
//!
//! `Staker`'s `Eq`/`Ord` are keyed on the `(next_time, priority, tx_id)` ordering
//! tuple, so a `BTreeSet<Staker>` correctly dedups and orders entries the same
//! way Go's `btree.Less` does.

use std::collections::{BTreeMap, BTreeSet};

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::state::staker::Staker;

/// The current/pending staker collection (`state.baseStakers`).
///
/// Validators are also indexed by `(subnet, node)` for `GetValidator`-style
/// point lookups; delegators live only in the ordered set.
#[derive(Clone, Debug, Default)]
pub struct Stakers {
    /// The ordered staker set (validators + delegators).
    set: BTreeSet<Staker>,
    /// Point-lookup index over the validators only, keyed by `(subnet, node)`.
    validators: BTreeMap<(Id, NodeId), Staker>,
}

impl Stakers {
    /// An empty staker collection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts/replaces a validator and indexes it by `(subnet, node)`.
    pub fn put_validator(&mut self, s: Staker) {
        // Replace any prior entry for the same (subnet, node) so the ordered set
        // does not retain a stale staker under a different ordering key.
        if let Some(prev) = self.validators.remove(&(s.subnet_id, s.node_id)) {
            self.set.remove(&prev);
        }
        self.validators.insert((s.subnet_id, s.node_id), s.clone());
        self.set.insert(s);
    }

    /// Removes a validator from both the ordered set and the index.
    pub fn delete_validator(&mut self, s: &Staker) {
        self.validators.remove(&(s.subnet_id, s.node_id));
        self.set.remove(s);
    }

    /// Inserts a delegator into the ordered set (delegators are not indexed by
    /// `(subnet, node)` — a node may have many).
    pub fn put_delegator(&mut self, s: Staker) {
        self.set.insert(s);
    }

    /// Removes a delegator from the ordered set.
    pub fn delete_delegator(&mut self, s: &Staker) {
        self.set.remove(s);
    }

    /// The validator for `(subnet, node)`, if present.
    #[must_use]
    pub fn get_validator(&self, subnet: Id, node: NodeId) -> Option<&Staker> {
        self.validators.get(&(subnet, node))
    }

    /// All stakers in `Staker` (Less) order.
    pub fn iter(&self) -> impl Iterator<Item = &Staker> {
        self.set.iter()
    }

    /// All stakers in `Staker` (Less) order, owned (clones).
    #[must_use]
    pub fn to_vec(&self) -> Vec<Staker> {
        self.set.iter().cloned().collect()
    }
}
