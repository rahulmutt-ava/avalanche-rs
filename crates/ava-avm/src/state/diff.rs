// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The in-memory [`Diff`] overlay (`vms/avm/state/diff.go`, specs 09 §5).
//!
//! A `Diff` is a layered, in-memory overlay over a parent [`Chain`] resolved
//! through a [`Versions`] by block id (or directly over a parent via
//! [`Diff::new_on`]). Reads consult the overlay first and fall through to the
//! parent; mutations are recorded only in the overlay. [`Diff::apply`] flushes
//! the overlay onto a base `Chain` (the bottom of the diff stack is
//! [`State`](super::state::State)).
//!
//! Every overlay map is a [`BTreeMap`], so [`apply`](Diff::apply) emits keys in
//! sorted order regardless of insertion order — the determinism contract of
//! 00 §6.1 (verified by the `diff_flush_is_sorted` proptest).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::state::chain::{Chain, ReadOnlyChain, UtxoBytes};
use crate::state::versions::Versions;

/// A pending overlay op on a single UTXO.
#[derive(Clone, Debug)]
enum UtxoOp {
    /// Add (or replace) the UTXO with these opaque codec bytes.
    Add(UtxoBytes),
    /// Delete the UTXO (a tombstone shadowing the parent).
    Delete,
}

/// The in-memory diff overlay over a parent [`Chain`] (`state.diff`).
pub struct Diff {
    /// The parent state view (resolved through [`Versions`] at construction).
    parent: Arc<dyn Chain>,

    /// Modified UTXOs: `Add` ⇒ added/replaced, `Delete` ⇒ removed.
    modified_utxos: BTreeMap<Id, UtxoOp>,
    /// Added txs (`txID → signed-tx bytes`).
    added_txs: BTreeMap<Id, Vec<u8>>,
    /// Added `height → blockID` index entries.
    added_block_ids: BTreeMap<u64, Id>,
    /// Added blocks (`blockID → block bytes`).
    added_blocks: BTreeMap<Id, Vec<u8>>,

    /// Pending last-accepted (initialized from the parent).
    last_accepted: Id,
    /// Pending timestamp (initialized from the parent).
    timestamp: SystemTime,
}

impl Diff {
    /// Builds a `Diff` over the parent block `parent_id`, resolved through
    /// `versions` (Go `state.NewDiff`).
    ///
    /// # Errors
    /// Returns [`Error::MissingParentState`] when `versions` cannot resolve
    /// `parent_id` (Go returns `ErrMissingParentState`).
    pub fn new(parent_id: Id, versions: &dyn Versions) -> Result<Self> {
        let parent = versions
            .get_state(parent_id)
            .ok_or(Error::MissingParentState)?;
        Ok(Self::over(parent))
    }

    /// Builds a `Diff` directly over `parent` (Go `state.NewDiffOn`).
    ///
    /// # Errors
    /// Currently infallible, but returns [`Result`] to mirror Go's `NewDiffOn`.
    pub fn new_on(parent: Arc<dyn Chain>) -> Result<Self> {
        Ok(Self::over(parent))
    }

    /// Builds an empty overlay seeded with the parent's last-accepted/timestamp.
    fn over(parent: Arc<dyn Chain>) -> Self {
        let last_accepted = parent.get_last_accepted();
        let timestamp = parent.get_timestamp();
        Self {
            parent,
            modified_utxos: BTreeMap::new(),
            added_txs: BTreeMap::new(),
            added_block_ids: BTreeMap::new(),
            added_blocks: BTreeMap::new(),
            last_accepted,
            timestamp,
        }
    }

    /// Flushes this overlay onto `base` (Go `diff.Apply`).
    ///
    /// Mutations replay in deterministic `BTreeMap` (sorted-key) order: UTXOs,
    /// txs, blocks (and their height index), then the last-accepted/timestamp
    /// singletons.
    pub fn apply(&self, base: &mut dyn Chain) {
        for (&id, op) in &self.modified_utxos {
            match op {
                UtxoOp::Add(bytes) => base.add_utxo(id, bytes.clone()),
                UtxoOp::Delete => base.delete_utxo(id),
            }
        }
        for (&tx_id, bytes) in &self.added_txs {
            base.add_tx(tx_id, bytes.clone());
        }
        for (&blk_id, bytes) in &self.added_blocks {
            // Recover the height for this block from the index overlay; default
            // to 0 if (impossibly) absent — `add_block` always records both.
            let height = self
                .added_block_ids
                .iter()
                .find_map(|(&h, &id)| (id == blk_id).then_some(h))
                .unwrap_or(0);
            base.add_block(blk_id, height, bytes.clone());
        }
        base.set_last_accepted(self.last_accepted);
        base.set_timestamp(self.timestamp);
    }

    /// The modified-UTXO ids in flush order (sorted, `BTreeMap` key order).
    ///
    /// Exposed for the `diff_flush_is_sorted` determinism proptest (00 §6.1).
    #[must_use]
    pub fn flush_utxo_ids(&self) -> Vec<Id> {
        self.modified_utxos.keys().copied().collect()
    }
}

impl ReadOnlyChain for Diff {
    fn get_utxo(&self, utxo_id: Id) -> Result<UtxoBytes> {
        match self.modified_utxos.get(&utxo_id) {
            Some(UtxoOp::Add(bytes)) => Ok(bytes.clone()),
            Some(UtxoOp::Delete) => Err(Error::Database(ava_database::error::Error::NotFound)),
            None => self.parent.get_utxo(utxo_id),
        }
    }

    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>> {
        match self.added_txs.get(&tx_id) {
            Some(bytes) => Ok(bytes.clone()),
            None => self.parent.get_tx(tx_id),
        }
    }

    fn get_block_id_at_height(&self, height: u64) -> Option<Id> {
        match self.added_block_ids.get(&height) {
            Some(&id) => Some(id),
            None => self.parent.get_block_id_at_height(height),
        }
    }

    fn get_block(&self, blk_id: Id) -> Result<Vec<u8>> {
        match self.added_blocks.get(&blk_id) {
            Some(bytes) => Ok(bytes.clone()),
            None => self.parent.get_block(blk_id),
        }
    }

    fn get_last_accepted(&self) -> Id {
        self.last_accepted
    }

    fn get_timestamp(&self) -> SystemTime {
        self.timestamp
    }
}

impl Chain for Diff {
    fn add_utxo(&mut self, id: Id, utxo: UtxoBytes) {
        self.modified_utxos.insert(id, UtxoOp::Add(utxo));
    }

    fn delete_utxo(&mut self, id: Id) {
        self.modified_utxos.insert(id, UtxoOp::Delete);
    }

    fn add_tx(&mut self, tx_id: Id, bytes: Vec<u8>) {
        self.added_txs.insert(tx_id, bytes);
    }

    fn add_block(&mut self, blk_id: Id, height: u64, bytes: Vec<u8>) {
        self.added_block_ids.insert(height, blk_id);
        self.added_blocks.insert(blk_id, bytes);
    }

    fn set_last_accepted(&mut self, blk_id: Id) {
        self.last_accepted = blk_id;
    }

    fn set_timestamp(&mut self, t: SystemTime) {
        self.timestamp = t;
    }
}
