// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! On-disk state for the C-Chain's cross-chain (atomic) transactions
//! (specs/11 §8, 27 §2.3).
//!
//! Port of `vms/saevm/cchain/state`. [`State`] tracks the accepted atomic txs
//! (indexed by id, with their accepted height) and the last applied height.
//! When applying, the per-chain shared-memory mutations are committed
//! **atomically** with the local index writes in a single batch — so a crash
//! can never leave the local state and shared memory disagreeing (ATOMIC-1, the
//! 27 §3.1 two-sided-consistency seam).
//!
//! # Port note (atomic trie)
//!
//! The Go state also maintains a height-indexed `AtomicTrie` (a Merkle root over
//! the per-chain requests) for state-sync / byte compatibility with coreth's
//! existing on-disk trie. That trie is **not** reproduced here: the SAE-Rust
//! C-Chain stores state in Firewood, and the cross-impl on-disk trie layout is
//! an M9 concern (specs 02). The consensus-critical behaviour this task needs —
//! the single-batch shared-memory apply + the tx index — is faithful.

use std::sync::Arc;

use ava_database::{BatchOps, DynDatabase, prefixdb};
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Requests, SharedMemory};

use crate::tx::Tx;

/// Errors returned by the atomic-tx [`State`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A database operation failed.
    #[error("database: {0}")]
    Database(#[from] ava_database::Error),
    /// A shared-memory apply failed.
    #[error("shared memory: {0}")]
    SharedMemory(ava_vm::error::Error),
    /// A tx failed to (un)marshal.
    #[error("tx codec: {0}")]
    Tx(#[from] crate::tx::Error),
    /// A stored value was malformed.
    #[error("malformed state value: {0}")]
    Malformed(&'static str),
}

/// The tx-index prefix (`atomicTxDB`, byte-compatible with coreth).
const TX_PREFIX: &[u8] = b"atomicTxDB";
/// The last-committed-height key (under `atomicTrieMetaDB`).
const META_PREFIX: &[u8] = b"atomicTrieMetaDB";
const LAST_HEIGHT_KEY: &[u8] = b"atomicTrieLastCommittedBlock";

/// `state.State` — the accepted-atomic-tx index + last applied height.
///
/// [`State::apply`] commits the tx index and the shared-memory mutation in one
/// batch. `apply` MUST NOT be called concurrently with itself.
pub struct State {
    db: Arc<dyn DynDatabase>,
    current_height: u64,
}

impl State {
    /// `state.New` — initialize the state over `db`, reading the last applied
    /// height (0 if none).
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the last-height read fails.
    pub fn new(db: Arc<dyn DynDatabase>) -> Result<Self, Error> {
        let current_height = read_last_height(db.as_ref())?;
        Ok(Self { db, current_height })
    }

    /// `State.CurrentHeight` — the highest height successfully applied.
    #[must_use]
    pub fn current_height(&self) -> u64 {
        self.current_height
    }

    /// `State.Apply` — persist the atomic `txs` accepted at `height`: index each
    /// tx by id, advance the last height, and apply the merged per-chain
    /// shared-memory requests — all in **one** atomic batch (27 §2.3).
    ///
    /// A no-op when `height` is not higher than [`State::current_height`]
    /// (restarts reprocess already-applied heights; shared memory must not be
    /// applied twice).
    ///
    /// # Errors
    /// Returns [`Error::Tx`] on a malformed tx, [`Error::Database`] on a write
    /// failure, or [`Error::SharedMemory`] if the atomic commit fails.
    pub fn apply<S: SharedMemory + ?Sized>(
        &mut self,
        height: u64,
        txs: &[Tx],
        sm: &S,
    ) -> Result<(), Error> {
        if height <= self.current_height {
            return Ok(());
        }

        // Merge the per-chain atomic requests in txID order so the byte content
        // is deterministic (Go `atomicRequests` sorts by txID).
        let requests = merge_atomic_requests(txs)?;

        // Buffer the local index writes into a side batch.
        let mut batch = BatchOps::new();
        for tx in txs {
            write_tx(&mut batch, height, tx)?;
        }
        batch.put(&last_height_key(), &height.to_be_bytes());

        // Commit the side batch atomically with the shared-memory mutation. A
        // crash leaves both applied or neither (ATOMIC-1).
        sm.apply(requests, std::slice::from_ref(&batch))
            .map_err(Error::SharedMemory)?;

        self.current_height = height;
        Ok(())
    }

    /// `State.GetTx` — the tx with `tx_id` and the height it was accepted at.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if `tx_id` is unknown, or
    /// [`Error::Malformed`] / [`Error::Tx`] if the stored value is corrupt.
    pub fn get_tx(&self, tx_id: Id) -> Result<(Tx, u64), Error> {
        let raw = self.db.get(&tx_key(tx_id))?;
        let height_bytes = raw.get(0..8).ok_or(Error::Malformed("truncated height"))?;
        let mut h = [0u8; 8];
        h.copy_from_slice(height_bytes);
        let height = u64::from_be_bytes(h);
        let tx_bytes = raw.get(8..).ok_or(Error::Malformed("missing tx bytes"))?;
        let tx = Tx::parse(tx_bytes)?;
        Ok((tx, height))
    }
}

/// `atomicRequests` — merge the per-chain atomic requests from `txs`, sorted by
/// txID so the merged content is order-independent of the input order.
fn merge_atomic_requests(txs: &[Tx]) -> Result<std::collections::BTreeMap<Id, Requests>, Error> {
    let mut sorted: Vec<&Tx> = txs.iter().collect();
    sorted.sort_by_key(|t| t.id());

    let mut ops: std::collections::BTreeMap<Id, Requests> = std::collections::BTreeMap::new();
    for tx in sorted {
        let (chain_id, req) = tx.atomic_requests()?;
        let entry = ops.entry(chain_id).or_default();
        entry.put.extend(req.put);
        entry.remove.extend(req.remove);
    }
    Ok(ops)
}

/// Buffers a tx index write: `tx_key(id) → height (8 BE bytes) ‖ tx_bytes`.
fn write_tx(batch: &mut BatchOps, height: u64, tx: &Tx) -> Result<(), Error> {
    let tx_bytes = tx.marshal()?;
    let mut value = Vec::with_capacity(8usize.saturating_add(tx_bytes.len()));
    value.extend_from_slice(&height.to_be_bytes());
    value.extend_from_slice(&tx_bytes);
    batch.put(&tx_key(tx.id()), &value);
    Ok(())
}

fn tx_key(id: Id) -> Vec<u8> {
    prefixdb::join_prefixes(&prefixdb::make_prefix(TX_PREFIX), id.as_bytes())
}

fn last_height_key() -> Vec<u8> {
    prefixdb::join_prefixes(&prefixdb::make_prefix(META_PREFIX), LAST_HEIGHT_KEY)
}

fn read_last_height(db: &dyn DynDatabase) -> Result<u64, Error> {
    match db.get(&last_height_key()) {
        Ok(raw) => {
            let bytes = raw.get(0..8).ok_or(Error::Malformed("truncated height"))?;
            let mut h = [0u8; 8];
            h.copy_from_slice(bytes);
            Ok(u64::from_be_bytes(h))
        }
        Err(ava_database::Error::NotFound) => Ok(0),
        Err(e) => Err(Error::Database(e)),
    }
}
