// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The persisted X-Chain [`State`] base (`vms/avm/state/state.go`, specs 09 §5).
//!
//! `State` is the bottom of the diff stack: every accepted block's
//! [`Diff`](super::diff::Diff) is ultimately applied down to a `State`. It wraps
//! the chain DB in a [`VersionDb`] and partitions it into five
//! [`PrefixDb`] sub-stores (specs 09 §5):
//!
//! ```text
//! VMDB (VersionDb)
//! |- "utxo"      utxoID  -> utxo bytes
//! |- "tx"        txID    -> signed-tx bytes
//! |- "blockID"   height  -> blockID
//! |- "block"     blockID -> block bytes
//! '- "singleton" 0x00 initialized | 0x01 timestamp | 0x02 lastAccepted
//! ```
//!
//! UTXOs are stored as opaque codec bytes ([`UtxoBytes`]) keyed by
//! `UtxoId::input_id` (§5.1); txs as their cached signed bytes (parsed back via
//! the genesis codec, §5.3); blocks byte/id-level (no `StandardBlock` type yet).
//!
//! Mutations buffer in the [`VersionDb`] overlay until [`State::commit`] flushes
//! them to the base DB; [`State::abort`] discards them. The scalar singletons
//! (timestamp, last-accepted) are kept as in-memory fields written through to the
//! singleton store; [`State::load`] reads them back on reopen (the storage-layer
//! primitive — genesis seeding / `initialize_chain_state` is M5.11).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_database::{
    Batch, BatchOps, Database, KeyValueDeleter, KeyValueReader, KeyValueWriter, PrefixDb, VersionDb,
};
use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::state::chain::{Chain, ReadOnlyChain, UtxoBytes};

/// `utxoPrefix` — the UTXO sub-store namespace.
const UTXO_PREFIX: &[u8] = b"utxo";
/// `txPrefix` — the signed-tx sub-store namespace.
const TX_PREFIX: &[u8] = b"tx";
/// `blockIDPrefix` — the `height → blockID` index namespace.
const BLOCK_ID_PREFIX: &[u8] = b"blockID";
/// `blockPrefix` — the `blockID → block bytes` store namespace.
const BLOCK_PREFIX: &[u8] = b"block";
/// `singletonPrefix` — the singleton store namespace.
const SINGLETON_PREFIX: &[u8] = b"singleton";

/// `isInitializedKey` — singleton key for the initialized marker.
const IS_INITIALIZED_KEY: &[u8] = &[0x00];
/// `timestampKey` — singleton key for the chain timestamp.
const TIMESTAMP_KEY: &[u8] = &[0x01];
/// `lastAcceptedKey` — singleton key for the last-accepted block id.
const LAST_ACCEPTED_KEY: &[u8] = &[0x02];

/// The persisted X-Chain state base (`state.state`).
///
/// Generic over the base [`Database`] backend so the same code serves the
/// in-memory `MemDb` (tests, bootstrap) and the on-disk `RocksDb` (production).
/// The chain DB is wrapped in a [`VersionDb`], so mutations buffer until
/// [`commit`](State::commit) and a `Diff` can be applied transactionally.
pub struct State<D: Database> {
    /// The version DB over the chain base; commit/abort act on its overlay.
    db: Arc<VersionDb<D>>,

    // ----- five prefix sub-stores over `db` (specs 09 §5) -----
    utxo_db: PrefixDb<VersionDb<D>>,
    tx_db: PrefixDb<VersionDb<D>>,
    block_id_db: PrefixDb<VersionDb<D>>,
    block_db: PrefixDb<VersionDb<D>>,
    singleton_db: PrefixDb<VersionDb<D>>,

    // ----- scalar singletons (in-memory, written through to `singleton_db`) -----
    last_accepted: Id,
    timestamp: SystemTime,
}

impl<D: Database> State<D> {
    /// Builds a `State` over `base`, wiring the [`VersionDb`] and the five §5
    /// prefix sub-stores.
    ///
    /// The scalar singletons start uninitialized (`Id::EMPTY` / `UNIX_EPOCH`);
    /// call [`load`](State::load) to read any persisted values back on reopen.
    ///
    /// # Errors
    /// Currently infallible, but returns [`Result`] to mirror Go's `New` (which
    /// can fail building metered caches) and to keep the signature stable.
    pub fn new(base: Arc<D>) -> Result<Self> {
        let db = Arc::new(VersionDb::new_arc(base));
        Ok(Self {
            utxo_db: PrefixDb::new_arc(UTXO_PREFIX, Arc::clone(&db)),
            tx_db: PrefixDb::new_arc(TX_PREFIX, Arc::clone(&db)),
            block_id_db: PrefixDb::new_arc(BLOCK_ID_PREFIX, Arc::clone(&db)),
            block_db: PrefixDb::new_arc(BLOCK_PREFIX, Arc::clone(&db)),
            singleton_db: PrefixDb::new_arc(SINGLETON_PREFIX, Arc::clone(&db)),
            db,
            last_accepted: Id::EMPTY,
            timestamp: UNIX_EPOCH,
        })
    }

    /// Loads the persisted scalar singletons (timestamp, last-accepted) from the
    /// singleton store into the in-memory fields.
    ///
    /// This is the storage-layer read primitive; the post-linearization
    /// `InitializeChainState` (genesis seeding) is M5.11. Absent singletons are
    /// left at their defaults (a fresh chain).
    ///
    /// # Errors
    /// Returns [`Error::Database`] only on a non-`NotFound` read failure.
    pub fn load(&mut self) -> Result<()> {
        match self.singleton_db.get(LAST_ACCEPTED_KEY) {
            // A persisted id is always 32 bytes; a malformed value is treated as
            // unset (left at the default), never a hard error.
            Ok(bytes) => {
                if let Ok(id) = Id::from_slice(&bytes) {
                    self.last_accepted = id;
                }
            }
            Err(ava_database::error::Error::NotFound) => {}
            Err(e) => return Err(Error::Database(e)),
        }
        match self.singleton_db.get(TIMESTAMP_KEY) {
            Ok(bytes) => self.timestamp = decode_timestamp(&bytes),
            Err(ava_database::error::Error::NotFound) => {}
            Err(e) => return Err(Error::Database(e)),
        }
        Ok(())
    }

    /// `IsInitialized` — whether the singleton initialized marker is set.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the read fails.
    pub fn is_initialized(&self) -> Result<bool> {
        Ok(self.singleton_db.has(IS_INITIALIZED_KEY)?)
    }

    /// `SetInitialized` — set the singleton initialized marker.
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the write fails.
    pub fn set_initialized(&self) -> Result<()> {
        self.singleton_db.put(IS_INITIALIZED_KEY, &[])?;
        Ok(())
    }

    /// `Commit` — flush every buffered mutation to the base DB, then clear the
    /// overlay (Go `state.Commit` → `versiondb.Commit`).
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the underlying batch write fails.
    pub fn commit(&self) -> Result<()> {
        self.db.commit()?;
        Ok(())
    }

    /// `CommitBatch` — snapshot the uncommitted overlay ops into a [`BatchOps`]
    /// **without writing them**. The caller hands this to
    /// [`SharedMemory::apply`] so the state commit and the atomic-memory
    /// write share one underlying DB write (Go `state.CommitBatch`, 27 §2.2).
    ///
    /// # Errors
    /// Returns [`Error::Database`] if the versiondb is closed.
    pub fn commit_batch_ops(&self) -> Result<BatchOps> {
        let version_batch = self.db.commit_batch().map_err(Error::Database)?;
        let mut ops = BatchOps::new();
        version_batch.replay(&mut ops).map_err(Error::Database)?;
        Ok(ops)
    }

    /// `Abort` — discard every buffered (uncommitted) mutation.
    pub fn abort(&self) {
        self.db.abort();
    }

    /// A cheap **read-consistent snapshot** of this state as an immutable
    /// [`Arc<dyn Chain>`], for use as a [`Diff`](super::diff::Diff) parent.
    ///
    /// The snapshot shares the same underlying [`VersionDb`] handle (so the
    /// byte-valued sub-stores read the same buffered+committed data) and clones
    /// the in-memory scalar fields, matching Go where a verified block's diff
    /// parent is a frozen view.
    #[must_use]
    pub fn snapshot(&self) -> Arc<dyn Chain>
    where
        D: 'static,
    {
        let db = Arc::clone(&self.db);
        Arc::new(State {
            utxo_db: PrefixDb::new_arc(UTXO_PREFIX, Arc::clone(&db)),
            tx_db: PrefixDb::new_arc(TX_PREFIX, Arc::clone(&db)),
            block_id_db: PrefixDb::new_arc(BLOCK_ID_PREFIX, Arc::clone(&db)),
            block_db: PrefixDb::new_arc(BLOCK_PREFIX, Arc::clone(&db)),
            singleton_db: PrefixDb::new_arc(SINGLETON_PREFIX, Arc::clone(&db)),
            db,
            last_accepted: self.last_accepted,
            timestamp: self.timestamp,
        })
    }
}

impl<D: Database> ReadOnlyChain for State<D> {
    fn get_utxo(&self, utxo_id: Id) -> Result<UtxoBytes> {
        Ok(self.utxo_db.get(utxo_id.as_bytes())?)
    }

    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>> {
        Ok(self.tx_db.get(tx_id.as_bytes())?)
    }

    fn get_block_id_at_height(&self, height: u64) -> Option<Id> {
        let bytes = self.block_id_db.get(&height.to_be_bytes()).ok()?;
        Id::from_slice(&bytes).ok()
    }

    fn get_block(&self, blk_id: Id) -> Result<Vec<u8>> {
        Ok(self.block_db.get(blk_id.as_bytes())?)
    }

    fn get_last_accepted(&self) -> Id {
        self.last_accepted
    }

    fn get_timestamp(&self) -> SystemTime {
        self.timestamp
    }
}

impl<D: Database> Chain for State<D> {
    fn add_utxo(&mut self, id: Id, utxo: UtxoBytes) {
        // A versiondb put never fails observably (it buffers into the overlay);
        // swallow the (impossible-unless-closed) error to match Go's `AddUTXO`,
        // which records into an in-memory map.
        let _ = self.utxo_db.put(id.as_bytes(), &utxo);
    }

    fn delete_utxo(&mut self, id: Id) {
        let _ = self.utxo_db.delete(id.as_bytes());
    }

    fn add_tx(&mut self, tx_id: Id, bytes: Vec<u8>) {
        let _ = self.tx_db.put(tx_id.as_bytes(), &bytes);
    }

    fn add_block(&mut self, blk_id: Id, height: u64, bytes: Vec<u8>) {
        let _ = self.block_db.put(blk_id.as_bytes(), &bytes);
        let _ = self
            .block_id_db
            .put(&height.to_be_bytes(), blk_id.as_bytes());
    }

    fn set_last_accepted(&mut self, blk_id: Id) {
        self.last_accepted = blk_id;
        let _ = self.singleton_db.put(LAST_ACCEPTED_KEY, blk_id.as_bytes());
    }

    fn set_timestamp(&mut self, t: SystemTime) {
        self.timestamp = t;
        let _ = self.singleton_db.put(TIMESTAMP_KEY, &encode_timestamp(t));
    }
}

/// Encodes a [`SystemTime`] as big-endian seconds since the Unix epoch
/// (`database.PutTimestamp` stores the Unix-second `int64`).
fn encode_timestamp(t: SystemTime) -> [u8; 8] {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    secs.to_be_bytes()
}

/// Decodes the 8-byte big-endian Unix-second timestamp written by
/// [`encode_timestamp`]. A short/garbled value decodes to [`UNIX_EPOCH`].
fn decode_timestamp(bytes: &[u8]) -> SystemTime {
    let mut buf = [0u8; 8];
    if bytes.len() == 8 {
        buf.copy_from_slice(bytes);
        UNIX_EPOCH
            .checked_add(Duration::from_secs(u64::from_be_bytes(buf)))
            .unwrap_or(UNIX_EPOCH)
    } else {
        UNIX_EPOCH
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ava_database::MemDb;

    #[test]
    fn fresh_state_has_empty_singletons() {
        let s = State::new(Arc::new(MemDb::new())).expect("state");
        assert_eq!(s.get_last_accepted(), Id::EMPTY);
        assert_eq!(s.get_timestamp(), UNIX_EPOCH);
        assert!(!s.is_initialized().expect("is_initialized"));
    }
}
