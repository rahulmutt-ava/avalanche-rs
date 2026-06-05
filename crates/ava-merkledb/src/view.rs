// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`View`]/`TrieView` — an immutable proposal layering changes over a parent
//! (the DB or another view). Byte-exact-in-behavior port of Go
//! `x/merkledb/view.go` + `trie.go` (spec 04 §3.5).
//!
//! A view lazily computes node IDs / its root only when queried. The validity
//! model uses `Arc`-linked parent pointers, an [`AtomicBool`] validity flag and
//! a [`Weak`] child list: committing a view invalidates its siblings and their
//! descendants (→ [`Error::Invalid`]), and a view commits only if its parent is
//! the DB and only once. Independent-subtrie hashing is pure, so the change
//! computation may use a rayon scope (we keep the deterministic single-pass
//! walk here; the parallel path is an optimization detail, not a behavior
//! difference — spec §6.1).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use bytes::Bytes;
use parking_lot::Mutex;

use ava_database::Database;
use ava_types::id::Id;

use crate::DefaultHasher;
use crate::db::MerkleDb;
use crate::error::{Error, Result};
use crate::history::{ChangeSummary, KeyChange};
use crate::key::Key;
use crate::maybe::Maybe;
use crate::node::Node;
use crate::trie::Trie;

/// A single key/value operation layered by a view (put or delete). Mirrors Go
/// `database.BatchOp` as used in `ViewChanges.BatchOps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchOp {
    /// The key (full, unpacked bytes).
    pub key: Vec<u8>,
    /// The value; ignored when `delete` is true.
    pub value: Vec<u8>,
    /// Whether this op deletes `key`.
    pub delete: bool,
}

impl BatchOp {
    /// A put of `value` at `key`.
    #[must_use]
    pub fn put(key: &[u8], value: &[u8]) -> BatchOp {
        BatchOp {
            key: key.to_vec(),
            value: value.to_vec(),
            delete: false,
        }
    }

    /// A delete of `key`.
    #[must_use]
    pub fn delete(key: &[u8]) -> BatchOp {
        BatchOp {
            key: key.to_vec(),
            value: Vec::new(),
            delete: true,
        }
    }
}

/// The parent of a view: either the DB or another view.
pub(crate) enum Parent<D: Database> {
    /// The view is a direct child of the database.
    Db(Arc<MerkleDb<D>>),
    /// The view layers on another view.
    View(Arc<ViewInner<D>>),
}

impl<D: Database> Clone for Parent<D> {
    fn clone(&self) -> Self {
        match self {
            Parent::Db(db) => Parent::Db(db.clone()),
            Parent::View(v) => Parent::View(v.clone()),
        }
    }
}

/// A single node's before/after state, used when applying a view's changes.
pub(crate) struct NodeChange {
    /// The node before the change (`None` ⇒ newly created).
    pub before: Option<Node>,
    /// The node after the change (`None` ⇒ deleted).
    pub after: Option<Node>,
}

/// The result of computing a view's changes against its parent.
pub(crate) struct ViewChanges {
    /// The resulting root ID.
    pub root_id: Id,
    /// Per-full-key node changes (ascending).
    pub node_changes: BTreeMap<Key, NodeChange>,
    /// The history change-summary (per-key value changes + root).
    pub change_summary: ChangeSummary,
}

/// The shared inner state of a [`View`], referenced by `Arc` from both the
/// public handle and the parent/child links.
pub struct ViewInner<D: Database> {
    /// The view's parent (DB or another view).
    parent: Mutex<Parent<D>>,
    /// The operations this view layers over its parent.
    ops: Vec<BatchOp>,
    /// Outstanding child views (weak, to avoid cycles).
    child_views: Mutex<Vec<Weak<ViewInner<D>>>>,
    /// Whether this view has been invalidated.
    invalidated: AtomicBool,
    /// Whether this view has been committed.
    committed: AtomicBool,
    /// The trie's branch factor (cached from the DB).
    branch_factor: crate::key::BranchFactor,
}

/// A public handle to a view.
pub struct View<D: Database> {
    inner: Arc<ViewInner<D>>,
}

impl<D: Database> View<D> {
    /// Creates a new view layering `ops` over `parent`. Registers the view as a
    /// child of its parent for the validity-tracking model.
    pub(crate) fn new(parent: Parent<D>, ops: Vec<BatchOp>) -> Result<View<D>> {
        // A view can't be created on an already-invalid parent.
        let branch_factor = match &parent {
            Parent::Db(db) => db.branch_factor,
            Parent::View(v) => {
                if v.is_invalid() {
                    return Err(Error::Invalid);
                }
                v.branch_factor
            }
        };

        let inner = Arc::new(ViewInner {
            parent: Mutex::new(parent.clone()),
            ops,
            child_views: Mutex::new(Vec::new()),
            invalidated: AtomicBool::new(false),
            committed: AtomicBool::new(false),
            branch_factor,
        });

        match &parent {
            Parent::Db(db) => db.add_child_view(&inner),
            Parent::View(v) => v.add_child_view(&inner),
        }

        Ok(View { inner })
    }

    /// Creates a child view layering `ops` over this view.
    pub fn new_view(&self, ops: Vec<BatchOp>) -> Result<View<D>> {
        View::new(Parent::View(self.inner.clone()), ops)
    }

    /// Returns this view's merkle root, computing it lazily.
    pub fn get_merkle_root(&self) -> Result<Id> {
        if self.inner.is_invalid() {
            return Err(Error::Invalid);
        }
        let changes = self.inner.build_changes()?;
        if self.inner.is_invalid() {
            return Err(Error::Invalid);
        }
        Ok(changes.root_id)
    }

    /// Returns the value at `key` as seen through this view (own ops layered
    /// over the parent), or `None`.
    pub fn get_value(&self, key: &[u8]) -> Result<Option<Bytes>> {
        if self.inner.is_invalid() {
            return Err(Error::Invalid);
        }
        let merged = self.inner.merged_values()?;
        Ok(merged.get(&Key::from_bytes(key)).cloned())
    }

    /// Commits this view to the DB. Mirrors Go `view.CommitToDB` /
    /// `commitToDB`: requires the parent to be the DB; commits only once.
    pub fn commit(&self) -> Result<()> {
        let parent = self.inner.parent.lock().clone();
        match parent {
            Parent::Db(db) => db.commit_view(&self.inner),
            Parent::View(_) => Err(Error::ParentNotDatabase),
        }
    }
}

impl<D: Database> ViewInner<D> {
    /// Whether this view has been invalidated.
    pub(crate) fn is_invalid(&self) -> bool {
        self.invalidated.load(Ordering::Acquire)
    }

    /// Whether this view has been committed.
    pub(crate) fn is_committed(&self) -> bool {
        self.committed.load(Ordering::Acquire)
    }

    /// Whether this view's parent is the DB.
    pub(crate) fn parent_is_db(&self) -> bool {
        matches!(&*self.parent.lock(), Parent::Db(_))
    }

    /// Marks this view committed.
    pub(crate) fn mark_committed(&self) {
        self.committed.store(true, Ordering::Release);
    }

    /// Invalidates this view and all descendants. Mirrors Go `view.invalidate`.
    pub(crate) fn invalidate(&self) {
        self.invalidated.store(true, Ordering::Release);
        let children = std::mem::take(&mut *self.child_views.lock());
        for weak in children {
            if let Some(child) = weak.upgrade() {
                child.invalidate();
            }
        }
    }

    /// Registers `child` as a child view.
    pub(crate) fn add_child_view(&self, child: &Arc<ViewInner<D>>) {
        let mut guard = self.child_views.lock();
        guard.retain(|w| w.strong_count() > 0);
        guard.push(Arc::downgrade(child));
    }

    /// Removes and returns this view's children (for re-parenting on commit).
    pub(crate) fn take_children(&self) -> Vec<Weak<ViewInner<D>>> {
        std::mem::take(&mut *self.child_views.lock())
    }

    /// Replaces this view's parent (used when re-parenting onto the DB).
    pub(crate) fn set_parent(&self, parent: Parent<D>) {
        *self.parent.lock() = parent;
    }

    /// Returns the merged key/value set seen through this view: the parent's
    /// committed values (or the parent view's merged values) with this view's
    /// ops layered on top.
    fn merged_values(&self) -> Result<BTreeMap<Key, Bytes>> {
        let mut merged = match &*self.parent.lock() {
            Parent::Db(db) => {
                let mut m = BTreeMap::new();
                for (k, v) in db.read_all_committed_values()? {
                    m.insert(k, v);
                }
                m
            }
            Parent::View(v) => v.merged_values()?,
        };
        for op in &self.ops {
            let key = Key::from_bytes(&op.key);
            if op.delete {
                merged.remove(&key);
            } else {
                merged.insert(key, Bytes::copy_from_slice(&op.value));
            }
        }
        Ok(merged)
    }

    /// Computes this view's root + the full node set for the merged values.
    fn build_changes(&self) -> Result<ViewChangesLite> {
        let merged = self.merged_values()?;
        let mut trie = Trie::new(self.branch_factor);
        for (k, v) in &merged {
            trie.apply(k.clone(), Maybe::Some(v.clone()));
        }
        let root_id = trie.root_id(&DefaultHasher);
        Ok(ViewChangesLite { root_id })
    }

    /// Builds a full change summary for committing this view to the DB. The
    /// `before` state is the DB's current node set, the `after` is this view's
    /// merged node set; the difference drives the value/intermediate node
    /// writes. Mirrors Go `view.changes`.
    pub(crate) fn build_change_summary(&self) -> Result<ViewChanges> {
        // Resolve the DB parent (commit only happens with a DB parent).
        let db = match &*self.parent.lock() {
            Parent::Db(db) => db.clone(),
            Parent::View(_) => return Err(Error::ParentNotDatabase),
        };

        // Before: the DB's current value set + node set.
        let before_values: BTreeMap<Key, Bytes> =
            db.read_all_committed_values()?.into_iter().collect();
        let mut before_trie = Trie::new(self.branch_factor);
        for (k, v) in &before_values {
            before_trie.apply(k.clone(), Maybe::Some(v.clone()));
        }
        let before_nodes = before_trie.nodes(&DefaultHasher);

        // After: the merged value set + node set.
        let after_values = self.merged_values()?;
        let mut after_trie = Trie::new(self.branch_factor);
        for (k, v) in &after_values {
            after_trie.apply(k.clone(), Maybe::Some(v.clone()));
        }
        let after_nodes = after_trie.nodes(&DefaultHasher);
        let root_id = after_trie.root_id(&DefaultHasher);

        // Diff the node sets.
        let mut node_changes: BTreeMap<Key, NodeChange> = BTreeMap::new();
        let mut all_keys: Vec<Key> = Vec::new();
        all_keys.extend(before_nodes.keys().cloned());
        all_keys.extend(after_nodes.keys().cloned());
        all_keys.sort();
        all_keys.dedup();
        for key in all_keys {
            let before = before_nodes.get(&key).cloned();
            let after = after_nodes.get(&key).cloned();
            if before != after {
                node_changes.insert(key, NodeChange { before, after });
            }
        }

        // Diff the value set for the history change-summary.
        let mut key_changes: BTreeMap<Key, KeyChange> = BTreeMap::new();
        let mut value_keys: Vec<Key> = Vec::new();
        value_keys.extend(before_values.keys().cloned());
        value_keys.extend(after_values.keys().cloned());
        value_keys.sort();
        value_keys.dedup();
        for key in value_keys {
            let before = before_values.get(&key).cloned();
            let after = after_values.get(&key).cloned();
            if before != after {
                key_changes.insert(
                    key,
                    KeyChange {
                        before: before.map(Maybe::Some).unwrap_or(Maybe::Nothing),
                        after: after.map(Maybe::Some).unwrap_or(Maybe::Nothing),
                    },
                );
            }
        }

        Ok(ViewChanges {
            root_id,
            node_changes,
            change_summary: ChangeSummary {
                root_id,
                key_changes,
            },
        })
    }
}

/// The light-weight result of [`ViewInner::build_changes`] (just the root).
struct ViewChangesLite {
    root_id: Id,
}
