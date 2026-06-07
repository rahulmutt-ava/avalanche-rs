// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`DynDb`] — adapts the object-safe [`Arc<dyn DynDatabase>`](DynDatabase) the
//! engine hands the VM at `initialize` to the typed [`Database`] surface the
//! P-Chain [`State`](crate::state::state::State) is generic over.
//!
//! `State<D: Database>` builds its prefix spaces over `Arc<D>`; the engine,
//! however, threads a single `Arc<dyn DynDatabase>` (object-safe facade) through
//! the chain. This thin newtype bridges the two by delegating every `&self`
//! [`Database`] op to the dyn facade and boxing the iterator/batch.

use ava_database::error::Result;
use ava_database::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, KeyValueDeleter,
    KeyValueReader, KeyValueWriter,
};
use std::sync::Arc;

/// A typed [`Database`] over an object-safe [`Arc<dyn DynDatabase>`](DynDatabase).
pub struct DynDb(Arc<dyn DynDatabase>);

impl DynDb {
    /// Wraps the engine-provided dyn database.
    #[must_use]
    pub fn new(db: Arc<dyn DynDatabase>) -> Self {
        Self(db)
    }
}

impl KeyValueReader for DynDb {
    fn has(&self, key: &[u8]) -> Result<bool> {
        self.0.has(key)
    }
    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        self.0.get(key)
    }
}

impl KeyValueWriter for DynDb {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.0.put(key, value)
    }
}

impl KeyValueDeleter for DynDb {
    fn delete(&self, key: &[u8]) -> Result<()> {
        self.0.delete(key)
    }
}

impl Compacter for DynDb {
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        self.0.compact(start, limit)
    }
}

impl Batcher for DynDb {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        self.0.new_batch()
    }
}

/// A cursor over [`DynDb`]: a thin wrapper delegating to the boxed
/// [`Iterator`](ava_database::Iterator) the dyn facade returns.
pub struct DynIter<'a>(BoxIter<'a>);

impl Iterator for DynIter<'_> {
    fn next(&mut self) -> bool {
        self.0.next()
    }
    fn error(&self) -> Result<()> {
        self.0.error()
    }
    fn key(&self) -> Option<&[u8]> {
        self.0.key()
    }
    fn value(&self) -> Option<&[u8]> {
        self.0.value()
    }
    fn release(&mut self) {
        self.0.release();
    }
}

impl Iteratee for DynDb {
    type Iter<'a>
        = DynIter<'a>
    where
        Self: 'a;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> Self::Iter<'_> {
        DynIter(self.0.new_iterator_with_start_and_prefix(start, prefix))
    }
}

impl Database for DynDb {
    fn close(&self) -> Result<()> {
        self.0.close()
    }
    fn health_check(&self) -> Result<serde_json::Value> {
        self.0.health_check()
    }
}
