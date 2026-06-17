// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The SAE C-Chain warp message store (Go `cchain/warp/storage.go`).
//!
//! [`Storage`] persists and fetches warp [`UnsignedMessage`]s. It is the SAE
//! analog of coreth's `warp/backend.go` message store, with three layers checked
//! by [`Storage::get`] in order:
//!
//! 1. an in-memory cache populated on write/read,
//! 2. an in-memory `overrides` map of off-chain operator messages, and
//! 3. the backing database, keyed by the message ID.
//!
//! ## Parity constraint
//!
//! The backing DB is wrapped in a [`PrefixDb`] under the **flat** `"warp"`
//! prefix ([`PrefixDb::new`], NOT [`PrefixDb::new_nested`]) to keep the
//! underlying database structure byte-compatible during the coreth → SAE VM
//! transition. Coreth uses the same flat prefix, so this MUST be maintained
//! here as well.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_database::PrefixDb;
use ava_database::traits::{Batcher, Database, KeyValueReader};
use ava_types::id::Id;
use ava_warp::UnsignedMessage;
use parking_lot::Mutex;

use super::Error;

/// `dbPrefix` — coreth's warp DB prefix. MUST stay a flat [`PrefixDb::new`] (not
/// [`PrefixDb::new_nested`]) so the underlying DB structure stays byte-compatible
/// during the coreth → SAE VM transition.
const DB_PREFIX: &[u8] = b"warp";

/// `Storage` persists and fetches warp messages (Go `cchain/warp/storage.go`).
///
/// Generic over the backing [`Database`] so tests can use an in-memory DB while
/// the VM supplies its real K/V store.
pub struct Storage<D: Database> {
    /// The backing database under the flat `"warp"` prefix.
    db: PrefixDb<D>,
    /// In-memory cache of messages, populated on write and on a DB read. This is
    /// a pure performance optimization (no wire/parity concern).
    cache: Mutex<BTreeMap<Id, UnsignedMessage>>,
    /// Off-chain operator messages held in memory (Go `overrides`).
    overrides: BTreeMap<Id, UnsignedMessage>,
}

impl<D: Database> Storage<D> {
    /// `NewStorage(db, msgs...)` — a new store backed by `db`.
    ///
    /// `db` is shared (an `Arc`) so multiple stores can be constructed over the
    /// same underlying database (Go passes the `database.Database` by interface
    /// reference). The DB is wrapped in the flat `"warp"` [`PrefixDb`].
    ///
    /// `overrides` are optional off-chain messages to keep in memory; they are
    /// indexed by their [`UnsignedMessage::id`].
    ///
    /// # Errors
    /// Returns [`Error::Warp`] if any override message fails to compute its ID.
    pub fn new(db: Arc<D>, overrides: &[UnsignedMessage]) -> Result<Self, Error> {
        let mut overrides_map = BTreeMap::new();
        for m in overrides {
            let id = m.id().map_err(ava_warp::Error::Codec)?;
            overrides_map.insert(id, m.clone());
        }
        Ok(Self {
            db: PrefixDb::new_arc(DB_PREFIX, db),
            cache: Mutex::new(BTreeMap::new()),
            overrides: overrides_map,
        })
    }

    /// `Add(msgs...)` — writes `msgs` to storage.
    ///
    /// The bytes are written to the DB in a single batch; the cache is updated
    /// only after the DB write succeeds (to keep the cache consistent with the
    /// DB, matching Go).
    ///
    /// # Errors
    /// Returns [`Error::Warp`] if a message ID cannot be computed, or
    /// [`Error::Db`] if the batch write fails.
    pub fn add(&self, msgs: &[UnsignedMessage]) -> Result<(), Error> {
        let mut batch = self.db.new_batch();
        for m in msgs {
            let id = m.id().map_err(ava_warp::Error::Codec)?;
            // TODO(M7.38): The message bytes are never read back, only the ID
            // (Go notes the same). We could store just the ID to save space.
            let bytes = m.marshal().map_err(ava_warp::Error::Codec)?;
            batch.put(id.as_bytes(), &bytes)?;
        }
        batch.write()?;
        drop(batch);

        // Cache after the DB write has succeeded to keep the cache consistent.
        let mut cache = self.cache.lock();
        for m in msgs {
            let id = m.id().map_err(ava_warp::Error::Codec)?;
            cache.insert(id, m.clone());
        }
        Ok(())
    }

    /// `Get(id)` — the message with `id`, checking cache → overrides → DB.
    ///
    /// # Errors
    /// Returns [`Error::Db`] (carrying [`ava_database::Error::NotFound`]) if the
    /// message is not present in any layer, or [`Error::Warp`] if the stored
    /// bytes fail to parse.
    pub fn get(&self, id: Id) -> Result<UnsignedMessage, Error> {
        if let Some(m) = self.cache.lock().get(&id) {
            return Ok(m.clone());
        }
        if let Some(m) = self.overrides.get(&id) {
            return Ok(m.clone());
        }

        let bytes = self.db.get(id.as_bytes())?;
        let m = UnsignedMessage::parse(&bytes).map_err(ava_warp::Error::Codec)?;
        self.cache.lock().insert(id, m.clone());
        Ok(m)
    }
}
