// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The DB-backed merkledb: [`MerkleDb`]. Wraps a base [`ava_database::Database`]
//! with a value-node store + an intermediate-node store, an in-memory root
//! (`arc_swap`), a bounded [`History`], and the set of outstanding child
//! [`View`]s. On open it honors the `cleanShutdown` flag, rebuilding
//! intermediate nodes from value nodes when the previous shutdown was unclean
//! (spec 04 §3.5, §10.8; 27 §4.1).
//!
//! Validity/commit state machine (Go `db.go` `commitView` + `view.go`
//! `invalidate`): committing a view invalidates all *sibling* views (and their
//! descendants) and re-parents the committed view's children onto the DB; a
//! view commits only if its parent is the DB and only once.

use std::sync::{Arc, Weak};

use arc_swap::ArcSwap;
use bytes::Bytes;
use parking_lot::Mutex;

use ava_database::{Database, Iterator as _};
use ava_types::id::Id;

use crate::DefaultHasher;
use crate::error::{Error, Result};
use crate::history::{DEFAULT_HISTORY_SIZE, History};
use crate::key::{BranchFactor, Key};
use crate::maybe::Maybe;
use crate::node_store::{IntermediateNodeDb, ValueNodeDb};
use crate::view::{Parent, View, ViewInner};

/// The metadata key body for the clean-shutdown flag (after `metadataPrefix`).
const CLEAN_SHUTDOWN_BODY: &[u8] = b"cleanShutdown";
/// `hadCleanShutdown` flag value (Go `hadCleanShutdown = []byte{1}`).
const HAD_CLEAN_SHUTDOWN: u8 = 0x01;
/// `didNotHaveCleanShutdown` flag value (Go `didNotHaveCleanShutdown = []byte{0}`).
const DID_NOT_HAVE_CLEAN_SHUTDOWN: u8 = 0x00;

/// A merkledb over a base key/value [`Database`].
pub struct MerkleDb<D: Database> {
    /// The base key/value store.
    pub(crate) base: Arc<D>,
    /// The value-node store (durable).
    pub(crate) value_node_db: ValueNodeDb<D>,
    /// The intermediate-node store (rebuildable, LRU-cached).
    pub(crate) intermediate_node_db: IntermediateNodeDb<D>,
    /// The trie's branch factor.
    pub(crate) branch_factor: BranchFactor,
    /// The current committed root ID (lock-free swap on commit).
    root_id: ArcSwap<Id>,
    /// Recent change-sets keyed by root ID.
    pub(crate) history: Mutex<History>,
    /// Outstanding child views (weak, to avoid reference cycles).
    pub(crate) child_views: Mutex<Vec<Weak<ViewInner<D>>>>,
    /// Serializes commits (Go `commitLock`).
    commit_lock: Mutex<()>,
}

impl<D: Database> MerkleDb<D> {
    /// Opens a merkledb over `base`. Honors the `cleanShutdown` flag: an unclean
    /// (missing-flag-but-data-present is treated as clean for a fresh DB; an
    /// explicit `false` triggers a rebuild of intermediate nodes from value
    /// nodes). Mirrors Go `newDatabase`.
    pub fn new(base: Arc<D>, branch_factor: BranchFactor) -> Result<Arc<Self>> {
        let value_node_db = ValueNodeDb::new(base.clone());
        let intermediate_node_db = IntermediateNodeDb::new(base.clone(), branch_factor);

        let db = Arc::new(MerkleDb {
            base,
            value_node_db,
            intermediate_node_db,
            branch_factor,
            root_id: ArcSwap::from_pointee(Id::EMPTY),
            history: Mutex::new(History::new(DEFAULT_HISTORY_SIZE)),
            child_views: Mutex::new(Vec::new()),
            commit_lock: Mutex::new(()),
        });

        let shutdown = db.read_clean_shutdown_flag()?;
        match shutdown {
            // Missing flag ⇒ fresh DB, nothing to do.
            None => {}
            // Clean ⇒ recompute the root from the durable value nodes.
            Some(true) => db.initialize_root()?,
            // Unclean ⇒ rebuild intermediate nodes from value nodes.
            Some(false) => db.rebuild()?,
        }

        // Mark "not cleanly shut down" until a future clean close re-sets it.
        db.write_clean_shutdown_flag(DID_NOT_HAVE_CLEAN_SHUTDOWN)?;
        Ok(db)
    }

    /// Returns the current committed root ID.
    pub fn get_merkle_root(&self) -> Result<Id> {
        Ok(**self.root_id.load())
    }

    /// Returns the value stored at `key`, if present.
    pub fn get_value(&self, key: &[u8]) -> Result<Option<Bytes>> {
        let k = Key::from_bytes(key);
        match self.value_node_db.get(&k)? {
            Some(node) => Ok(node.value.value().cloned()),
            None => Ok(None),
        }
    }

    /// Creates a new view layering `ops` over this DB.
    pub fn new_view(self: &Arc<Self>, ops: Vec<crate::view::BatchOp>) -> Result<View<D>> {
        View::new(Parent::Db(self.clone()), ops)
    }

    /// Marks a clean shutdown and persists the current root. Mirrors Go `Close`.
    pub fn close(&self) -> Result<()> {
        self.write_clean_shutdown_flag(HAD_CLEAN_SHUTDOWN)
    }

    // --- internals -------------------------------------------------------

    /// Reads the clean-shutdown flag: `None` if absent, else `Some(is_clean)`.
    fn read_clean_shutdown_flag(&self) -> Result<Option<bool>> {
        let key = clean_shutdown_key();
        match self.base.get(&key) {
            Ok(v) => Ok(Some(v.first().copied() == Some(HAD_CLEAN_SHUTDOWN))),
            Err(ava_database::Error::NotFound) => Ok(None),
            Err(e) => Err(Error::from(e)),
        }
    }

    fn write_clean_shutdown_flag(&self, value: u8) -> Result<()> {
        self.base.put(&clean_shutdown_key(), &[value])?;
        Ok(())
    }

    /// Recomputes the root from the durable value nodes (clean-open path).
    fn initialize_root(&self) -> Result<()> {
        let kvs = self.read_all_values()?;
        let root = self.compute_root_from_values(&kvs);
        self.root_id.store(Arc::new(root));
        Ok(())
    }

    /// Deletes every intermediate node and rebuilds them by re-adding every
    /// value node. Mirrors Go `merkleDB.rebuild` (27 §4.1).
    fn rebuild(&self) -> Result<()> {
        // Clear all intermediate nodes.
        self.clear_prefix(IntermediateNodeDb::<D>::prefix())?;

        let kvs = self.read_all_values()?;
        // Recompute the full node set and persist intermediate nodes (+ root).
        self.persist_full_trie(&kvs)?;
        Ok(())
    }

    /// Reads every (key, value) pair from the value-node store.
    fn read_all_values(&self) -> Result<Vec<(Key, Bytes)>> {
        let mut out = Vec::new();
        let prefix = [ValueNodeDb::<D>::prefix()];
        let mut it = self.base.new_iterator_with_prefix(&prefix);
        while it.next() {
            let (Some(k), Some(v)) = (it.key(), it.value()) else {
                break;
            };
            // Strip the 1-byte value-node prefix to recover the packed key bytes.
            let Some(key_bytes) = k.get(1..) else {
                continue;
            };
            let node = crate::codec::decode_db_node(v)?;
            if let Maybe::Some(value) = node.value {
                out.push((Key::from_bytes(key_bytes), value));
            }
        }
        it.error().map_err(Error::from)?;
        Ok(out)
    }

    /// Builds the full trie from `kvs` and returns its root ID.
    fn compute_root_from_values(&self, kvs: &[(Key, Bytes)]) -> Id {
        let mut trie = crate::trie::Trie::new(self.branch_factor);
        for (k, v) in kvs {
            trie.apply(k.clone(), Maybe::Some(v.clone()));
        }
        trie.root_id(&DefaultHasher)
    }

    /// Builds the full trie from `kvs`, persists value + intermediate nodes and
    /// the root. Used by the rebuild path.
    fn persist_full_trie(&self, kvs: &[(Key, Bytes)]) -> Result<()> {
        let mut trie = crate::trie::Trie::new(self.branch_factor);
        for (k, v) in kvs {
            trie.apply(k.clone(), Maybe::Some(v.clone()));
        }
        let nodes = trie.nodes(&DefaultHasher);
        let root = trie.root_id(&DefaultHasher);

        let mut value_ops: Vec<(Vec<u8>, Option<Bytes>)> = Vec::new();
        for (key, node) in &nodes {
            if node.has_value() {
                self.value_node_db
                    .stage_put(&mut value_ops, key, &node.db_node);
            } else {
                self.intermediate_node_db.put(key, &node.db_node)?;
            }
        }
        self.write_value_ops(value_ops)?;
        self.root_id.store(Arc::new(root));
        Ok(())
    }

    /// Deletes every base-DB entry under the 1-byte `prefix`.
    fn clear_prefix(&self, prefix: u8) -> Result<()> {
        let p = [prefix];
        let mut keys = Vec::new();
        let mut it = self.base.new_iterator_with_prefix(&p);
        while it.next() {
            if let Some(k) = it.key() {
                keys.push(k.to_vec());
            }
        }
        it.error().map_err(Error::from)?;
        drop(it);
        for k in keys {
            self.base.delete(&k)?;
        }
        Ok(())
    }

    /// Flushes a set of value-node put/delete ops to the base DB atomically.
    fn write_value_ops(&self, ops: Vec<(Vec<u8>, Option<Bytes>)>) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let mut batch = self.base.new_batch();
        for (k, v) in ops {
            match v {
                Some(value) => batch.put(&k, &value)?,
                None => batch.delete(&k)?,
            }
        }
        batch.write()?;
        Ok(())
    }

    /// Registers `child` as an outstanding child view of this DB.
    pub(crate) fn add_child_view(&self, child: &Arc<ViewInner<D>>) {
        let mut guard = self.child_views.lock();
        guard.retain(|w| w.strong_count() > 0);
        guard.push(Arc::downgrade(child));
    }

    /// Commits `view` to this DB. Mirrors Go `merkleDB.commitView`:
    /// validity/commit/parent checks, sibling invalidation, child re-parenting,
    /// then applies the node changes and swaps the root.
    pub(crate) fn commit_view(self: &Arc<Self>, view: &Arc<ViewInner<D>>) -> Result<()> {
        let _commit = self.commit_lock.lock();

        if view.is_invalid() {
            return Err(Error::Invalid);
        }
        if view.is_committed() {
            return Err(Error::Committed);
        }
        if !view.parent_is_db() {
            return Err(Error::ParentNotDatabase);
        }

        // Invalidate every other DB child view (+ descendants), then re-parent
        // the committed view's own children onto the DB.
        self.invalidate_children_except(view);
        self.move_child_views_to_db(view);

        // Compute and apply the node changes (diff against the parent DB state).
        let summary = view.build_change_summary()?;

        let mut value_ops: Vec<(Vec<u8>, Option<Bytes>)> = Vec::new();
        for (key, change) in &summary.node_changes {
            match (&change.before, &change.after) {
                (_, Some(node)) if node.has_value() => {
                    self.value_node_db
                        .stage_put(&mut value_ops, key, &node.db_node);
                }
                (_, Some(node)) => {
                    self.intermediate_node_db.put(key, &node.db_node)?;
                }
                (Some(before), None) if before.has_value() => {
                    self.value_node_db.stage_delete(&mut value_ops, key);
                }
                (Some(_), None) => {
                    self.intermediate_node_db.delete(key)?;
                }
                (None, None) => {}
            }
        }
        self.write_value_ops(value_ops)?;

        self.history.lock().record(summary.change_summary);
        self.root_id.store(Arc::new(summary.root_id));
        view.mark_committed();
        Ok(())
    }

    /// Invalidates every outstanding DB child view (and descendants) except
    /// `keep`. Mirrors Go `invalidateChildrenExcept`.
    fn invalidate_children_except(&self, keep: &Arc<ViewInner<D>>) {
        let mut guard = self.child_views.lock();
        for weak in guard.drain(..) {
            if let Some(child) = weak.upgrade() {
                if Arc::ptr_eq(&child, keep) {
                    continue;
                }
                child.invalidate();
            }
        }
    }

    /// Re-parents `committed`'s children onto the DB and tracks them as DB child
    /// views. Mirrors Go `moveChildViewsToDB`.
    fn move_child_views_to_db(self: &Arc<Self>, committed: &Arc<ViewInner<D>>) {
        let children = committed.take_children();
        let mut guard = self.child_views.lock();
        for weak in children {
            if let Some(child) = weak.upgrade() {
                child.set_parent(Parent::Db(self.clone()));
                guard.push(Arc::downgrade(&child));
            }
        }
    }

    /// Reads every committed (full-key, value) pair (used by views to build the
    /// merged set). Mirrors iterating the value-node store.
    pub(crate) fn read_all_committed_values(&self) -> Result<Vec<(Key, Bytes)>> {
        self.read_all_values()
    }
}

/// The full base-DB key for the clean-shutdown flag.
fn clean_shutdown_key() -> Vec<u8> {
    let mut k = vec![crate::node::prefix::METADATA];
    k.extend_from_slice(CLEAN_SHUTDOWN_BODY);
    k
}
