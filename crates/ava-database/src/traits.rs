// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `Database` trait family, mirroring `database/{database,batch,iterator,
//! common}.go` (04 §1.3).
//!
//! Load-bearing semantics preserved exactly (04 §1.1):
//! - **Ordered keys.** Iteration is in lexicographic byte order.
//! - **Memory safety.** `put` copies its args (so callers may mutate them
//!   afterwards); `get` returns an owned `Vec<u8>`.
//! - **`nil` ⇔ empty.** `&[]` is the empty key; `get` of an empty-valued key
//!   returns `Ok(Vec::new())`, never `Error::NotFound`.
//! - **Error model.** Post-`close` ops return [`Error::Closed`]; `get` of a
//!   missing key returns [`Error::NotFound`].

use crate::error::{Error, Result};

/// Reads a key/value store. `get` returns [`Error::NotFound`] when absent.
pub trait KeyValueReader {
    /// Returns whether `key` is present.
    fn has(&self, key: &[u8]) -> Result<bool>;
    /// Returns the value for `key`, or [`Error::NotFound`] when absent.
    fn get(&self, key: &[u8]) -> Result<Vec<u8>>;
}

/// Writes a key/value store.
pub trait KeyValueWriter {
    /// Stores `value` under `key`. Both args are copied; callers may mutate
    /// them after the call returns.
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()>;
}

/// Deletes from a key/value store.
pub trait KeyValueDeleter {
    /// Removes `key`. Deleting an absent key is not an error.
    fn delete(&self, key: &[u8]) -> Result<()>;
}

/// Compacts the underlying storage over a key range.
pub trait Compacter {
    /// Compacts `[start, limit)`. `start = None` ⇒ before all keys;
    /// `limit = None` ⇒ after all keys.
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()>;
}

/// A point-in-time, ordered cursor.
///
/// `Drop` is the implicit `Release()`, but an explicit [`Iterator::release`]
/// is kept for parity (idempotent). After exhaustion or a closed DB,
/// [`Iterator::next`] returns `false`; [`Iterator::error`] is read afterwards.
pub trait Iterator {
    /// Advances the cursor; returns `false` when exhausted.
    fn next(&mut self) -> bool;
    /// Returns any error observed during iteration (read after exhaustion).
    fn error(&self) -> Result<()>;
    /// Returns the current key, or `None` when done.
    fn key(&self) -> Option<&[u8]>;
    /// Returns the current value, or `None` when done.
    fn value(&self) -> Option<&[u8]>;
    /// Releases iterator resources. Idempotent.
    fn release(&mut self) {}
}

/// Constructs iterators over a store. The GAT `Iter<'a>` is convenient for
/// concrete backends but not object-safe — see [`DynDatabase`] for the boxed
/// facade (04 §1.3 object-safety note).
pub trait Iteratee {
    /// The concrete iterator type for this store.
    type Iter<'a>: Iterator
    where
        Self: 'a;

    /// Iterates the whole store in key order.
    fn new_iterator(&self) -> Self::Iter<'_> {
        self.new_iterator_with_start_and_prefix(&[], &[])
    }
    /// Iterates from `start` (inclusive) onward.
    fn new_iterator_with_start(&self, start: &[u8]) -> Self::Iter<'_> {
        self.new_iterator_with_start_and_prefix(start, &[])
    }
    /// Iterates only keys having `prefix`.
    fn new_iterator_with_prefix(&self, prefix: &[u8]) -> Self::Iter<'_> {
        self.new_iterator_with_start_and_prefix(&[], prefix)
    }
    /// Iterates keys having `prefix`, starting at `start` (inclusive).
    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> Self::Iter<'_>;
}

/// Object-safe Put/Delete target for [`Batch::replay`] and the
/// [`BatchOps`](crate::BatchOps) recorder.
///
/// Mirrors Go's `KeyValueWriterDeleter` as used by `Batch.Replay`. It takes
/// `&mut self` (a batch accumulates ops, so it mutates) — distinct from the
/// `&self` [`KeyValueWriter`]/[`KeyValueDeleter`] that the backing DB exposes.
pub trait WriteDelete {
    /// Stores `value` under `key`. Both args are copied.
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()>;
    /// Removes `key`.
    fn delete(&mut self, key: &[u8]) -> Result<()>;
}

/// A write-only, replayable, resettable batch (04 §1.1 batch contract).
/// Atomic on [`Batch::write`].
///
/// A batch *is* a [`WriteDelete`] (its `put`/`delete` buffer ops), which lets
/// one batch be replayed onto another (`Batch::inner` + `replay`).
pub trait Batch: WriteDelete {
    /// Bytes queued for writing (keys + values + deleted keys).
    fn size(&self) -> usize;
    /// Flushes accumulated ops to the host DB atomically.
    fn write(&mut self) -> Result<()>;
    /// Drops un-written ops for reuse.
    fn reset(&mut self);
    /// Replays buffered ops, in order, onto `w`.
    fn replay(&self, w: &mut dyn WriteDelete) -> Result<()>;
    /// Returns the batch writing to the inner DB (or `self` if at the base).
    fn inner(&mut self) -> &mut dyn Batch;
}

/// Constructs batches for a store.
pub trait Batcher {
    /// Creates a write-only batch buffering changes until [`Batch::write`].
    fn new_batch(&self) -> Box<dyn Batch + '_>;
}

/// The full key/value database (04 §1.3).
///
/// `Send + Sync` so it can live behind `Arc<dyn DynDatabase>`, matching how Go
/// threads a single `database.Database` through chains.
pub trait Database:
    KeyValueReader + KeyValueWriter + KeyValueDeleter + Batcher + Iteratee + Compacter + Send + Sync
{
    /// Closes the database; subsequent ops return [`Error::Closed`].
    fn close(&self) -> Result<()>;
    /// Reports health as a JSON blob (`health.Checker.HealthCheck`).
    fn health_check(&self) -> Result<serde_json::Value>;
}

/// A boxed iterator used wherever Go passes `database.Database` by interface.
pub type BoxIter<'a> = Box<dyn Iterator + 'a>;

/// Object-safe DB used behind `Arc<dyn DynDatabase>` across the workspace
/// (04 §1.3). Backends implement both the typed [`Database`] (for monomorphized
/// hot paths) and this thin facade.
pub trait DynDatabase: Send + Sync {
    /// See [`KeyValueReader::has`].
    fn has(&self, key: &[u8]) -> Result<bool>;
    /// See [`KeyValueReader::get`].
    fn get(&self, key: &[u8]) -> Result<Vec<u8>>;
    /// See [`KeyValueWriter::put`].
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()>;
    /// See [`KeyValueDeleter::delete`].
    fn delete(&self, key: &[u8]) -> Result<()>;
    /// See [`Batcher::new_batch`].
    fn new_batch(&self) -> Box<dyn Batch + '_>;
    /// See [`Iteratee::new_iterator_with_start_and_prefix`].
    fn new_iterator_with_start_and_prefix<'a>(&'a self, start: &[u8], prefix: &[u8])
    -> BoxIter<'a>;
    /// See [`Compacter::compact`].
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()>;
    /// See [`Database::close`].
    fn close(&self) -> Result<()>;
    /// See [`Database::health_check`].
    fn health_check(&self) -> Result<serde_json::Value>;
}

/// `IteratorError` mirrors Go's `database.IteratorError`: an already-errored
/// iterator returned when, e.g., a closed DB is asked for a new iterator. Every
/// method reports the stored error and yields nothing.
pub struct IteratorError {
    /// The error this iterator reports.
    pub err: Error,
}

impl IteratorError {
    /// Creates an iterator that always reports `err`.
    pub fn new(err: Error) -> Self {
        Self { err }
    }
}

impl Iterator for IteratorError {
    fn next(&mut self) -> bool {
        false
    }
    fn error(&self) -> Result<()> {
        match &self.err {
            Error::Closed => Err(Error::Closed),
            Error::NotFound => Err(Error::NotFound),
            Error::Other(e) => Err(Error::Other(anyhow::anyhow!("{e}"))),
        }
    }
    fn key(&self) -> Option<&[u8]> {
        None
    }
    fn value(&self) -> Option<&[u8]> {
        None
    }
}
