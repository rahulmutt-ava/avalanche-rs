// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `prefixdb` — key namespacing over a base DB (04 §2.3, §10.1), mirroring
//! `database/prefixdb/db.go`.
//!
//! Partitions a parent DB by prepending a fixed 32-byte hashed prefix to every
//! key. The on-disk key is `SHA256(prefix) ‖ key`, so the namespacing must be
//! reproduced byte-for-byte for a Rust node to read a Go-written shared DB:
//!
//! - [`make_prefix`]`(p) = SHA256(p)` (`MakePrefix`) — used by [`PrefixDb::new`]
//!   when wrapping a non-prefixdb base, and always by [`PrefixDb::new_nested`].
//! - [`join_prefixes`]`(parent32, child) = SHA256(parent32 ‖ child)`
//!   (`JoinPrefixes`) — used when wrapping an existing [`PrefixDb`] so nested
//!   prefixes compress to a single 32-byte hash sharing the same base DB.
//!
//! `db_limit = increment(prefix)` bounds range `compact`. Iterators strip the
//! prefix from returned keys. A small byte-buffer pool ([`BytesPool`]) mirrors
//! Go's `utils.BytesPool` to avoid per-op allocation of the prefixed-key buffer.
//!
//! The inner DB is held behind an [`Arc`] so a join (constructing a `PrefixDb`
//! over an existing `PrefixDb`) can share the very same base, exactly as Go
//! threads `prefixDB.db` through.

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use sha2::{Digest, Sha256};

use crate::batch::BatchOps;
use crate::error::{Error, Result};
use crate::traits::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, IteratorError,
    KeyValueDeleter, KeyValueReader, KeyValueWriter, WriteDelete,
};

/// `MakePrefix(prefix) = hashing.ComputeHash256(prefix)` — the 32-byte hashed
/// namespace prefix (`database/prefixdb/db.go`).
pub fn make_prefix(prefix: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(prefix);
    hasher.finalize().to_vec()
}

/// `JoinPrefixes(first, second) = MakePrefix(first ‖ second)` — compresses a
/// nested prefix into a single 32-byte hash (`database/prefixdb/db.go`).
pub fn join_prefixes(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(first);
    hasher.update(second);
    hasher.finalize().to_vec()
}

/// Returns a copy of `orig` lexically incremented by one (`incrementByteSlice`):
/// add 1 to the last byte, carrying left on wraparound. All-`0xff` wraps to all
/// zeros (matching Go), which makes a range `[prefix, db_limit)` cover the whole
/// suffix space for that prefix.
fn increment_byte_slice(orig: &[u8]) -> Vec<u8> {
    let mut buf = orig.to_vec();
    for b in buf.iter_mut().rev() {
        *b = b.wrapping_add(1);
        if *b != 0 {
            break;
        }
    }
    buf
}

/// A tiny reusable byte-buffer pool, mirroring Go's `utils.BytesPool`: hands out
/// `Vec<u8>` of the requested length, recycling capacity to avoid per-op
/// allocation of the prefixed-key scratch buffer. Bounded so it cannot grow
/// without limit.
struct BytesPool {
    free: Mutex<Vec<Vec<u8>>>,
}

/// The maximum number of buffers retained by the pool.
const POOL_CAP: usize = 32;

impl BytesPool {
    fn new() -> Self {
        Self {
            free: Mutex::new(Vec::new()),
        }
    }

    /// Returns a buffer of exactly `len` bytes (contents unspecified).
    fn get(&self, len: usize) -> Vec<u8> {
        let mut buf = self.free.lock().pop().unwrap_or_default();
        buf.clear();
        buf.resize(len, 0);
        buf
    }

    /// Returns `buf` to the pool for reuse (dropped if the pool is full).
    fn put(&self, buf: Vec<u8>) {
        let mut free = self.free.lock();
        if free.len() < POOL_CAP {
            free.push(buf);
        }
    }
}

/// A sub-database that prefixes every key with a fixed 32-byte hash (04 §2.3).
pub struct PrefixDb<D: Database> {
    /// All keys begin with this 32-byte hashed prefix.
    db_prefix: Vec<u8>,
    /// Lexically one greater than `db_prefix`; the end of this db's key range.
    db_limit: Vec<u8>,
    pool: BytesPool,
    /// `None` ⇒ closed (mirrors Go's `closed` flag; the lock guards close so the
    /// base is never observed mid-close).
    closed: RwLock<bool>,
    /// The underlying base DB, shared with any joined siblings.
    db: Arc<D>,
}

impl<D: Database> PrefixDb<D> {
    /// Wraps `db` under `MakePrefix(prefix)`. (The Go fast-path that *joins* when
    /// `db` is itself a prefixdb is exposed separately as [`PrefixDb::join`],
    /// since Rust generics distinguish the two at the type level.)
    pub fn new(prefix: &[u8], db: D) -> Self {
        Self::new_with_arc(make_prefix(prefix), Arc::new(db))
    }

    /// Like [`PrefixDb::new`] but over an already-`Arc`'d base, so several
    /// prefix views can share one base DB.
    pub fn new_arc(prefix: &[u8], db: Arc<D>) -> Self {
        Self::new_with_arc(make_prefix(prefix), db)
    }

    /// Wraps `db` under `MakePrefix(prefix)` without joining, matching Go's
    /// `NewNested` (always `SHA256(prefix)`, no compression).
    pub fn new_nested(prefix: &[u8], db: D) -> Self {
        Self::new_with_arc(make_prefix(prefix), Arc::new(db))
    }

    /// Constructs a nested view over `self`'s base by *joining* prefixes
    /// (`JoinPrefixes(self.db_prefix, prefix)`), sharing the same base DB —
    /// the Go fast-path taken when `New(prefix, db)` sees `db` is a prefixdb.
    pub fn join(&self, prefix: &[u8]) -> Self {
        Self::new_with_arc(join_prefixes(&self.db_prefix, prefix), Arc::clone(&self.db))
    }

    fn new_with_arc(db_prefix: Vec<u8>, db: Arc<D>) -> Self {
        let db_limit = increment_byte_slice(&db_prefix);
        Self {
            db_prefix,
            db_limit,
            pool: BytesPool::new(),
            closed: RwLock::new(false),
            db,
        }
    }

    /// Returns a copy of `key` prepended with this db's prefix, drawn from the
    /// buffer pool.
    fn prefixed(&self, key: &[u8]) -> Vec<u8> {
        // The pool hands back an empty buffer; extend rather than index-slice so
        // there is no panicking range expression (clippy::indexing_slicing).
        let mut buf = self.pool.get(0);
        buf.reserve(self.db_prefix.len().saturating_add(key.len()));
        buf.extend_from_slice(&self.db_prefix);
        buf.extend_from_slice(key);
        buf
    }

    fn is_closed(&self) -> bool {
        *self.closed.read()
    }
}

impl<D: Database> KeyValueReader for PrefixDb<D> {
    fn has(&self, key: &[u8]) -> Result<bool> {
        let _guard = self.closed.read();
        if *_guard {
            return Err(Error::Closed);
        }
        let pk = self.prefixed(key);
        let out = self.db.has(&pk);
        self.pool.put(pk);
        out
    }

    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        let _guard = self.closed.read();
        if *_guard {
            return Err(Error::Closed);
        }
        let pk = self.prefixed(key);
        let out = self.db.get(&pk);
        self.pool.put(pk);
        out
    }
}

impl<D: Database> KeyValueWriter for PrefixDb<D> {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let _guard = self.closed.read();
        if *_guard {
            return Err(Error::Closed);
        }
        let pk = self.prefixed(key);
        let out = self.db.put(&pk, value);
        self.pool.put(pk);
        out
    }
}

impl<D: Database> KeyValueDeleter for PrefixDb<D> {
    fn delete(&self, key: &[u8]) -> Result<()> {
        let _guard = self.closed.read();
        if *_guard {
            return Err(Error::Closed);
        }
        let pk = self.prefixed(key);
        let out = self.db.delete(&pk);
        self.pool.put(pk);
        out
    }
}

impl<D: Database> Compacter for PrefixDb<D> {
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        let _guard = self.closed.read();
        if *_guard {
            return Err(Error::Closed);
        }
        let prefixed_start = self.prefixed(start.unwrap_or(&[]));
        let result = match limit {
            // No upper bound: range-bound to this prefix's suffix space.
            None => self.db.compact(Some(&prefixed_start), Some(&self.db_limit)),
            Some(l) => {
                let prefixed_limit = self.prefixed(l);
                let r = self
                    .db
                    .compact(Some(&prefixed_start), Some(&prefixed_limit));
                self.pool.put(prefixed_limit);
                r
            }
        };
        self.pool.put(prefixed_start);
        result
    }
}

impl<D: Database> Batcher for PrefixDb<D> {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        Box::new(PrefixBatch {
            db: self,
            inner: self.db.new_batch(),
            ops: BatchOps::new(),
        })
    }
}

impl<D: Database> Iteratee for PrefixDb<D> {
    type Iter<'a>
        = PrefixIterator<'a, D>
    where
        D: 'a;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> Self::Iter<'_> {
        if self.is_closed() {
            return PrefixIterator {
                db: self,
                inner: Box::new(IteratorError::new(Error::Closed)),
                key: None,
                value: None,
                err: Some(Error::Closed),
            };
        }
        let prefixed_start = self.prefixed(start);
        let prefixed_prefix = self.prefixed(prefix);
        let inner: BoxIter<'_> = Box::new(
            self.db
                .new_iterator_with_start_and_prefix(&prefixed_start, &prefixed_prefix),
        );
        self.pool.put(prefixed_start);
        self.pool.put(prefixed_prefix);
        PrefixIterator {
            db: self,
            inner,
            key: None,
            value: None,
            err: None,
        }
    }
}

impl<D: Database> Database for PrefixDb<D> {
    fn close(&self) -> Result<()> {
        let mut guard = self.closed.write();
        if *guard {
            return Err(Error::Closed);
        }
        *guard = true;
        Ok(())
    }

    fn health_check(&self) -> Result<serde_json::Value> {
        let _guard = self.closed.read();
        if *_guard {
            return Err(Error::Closed);
        }
        self.db.health_check()
    }
}

impl<D: Database> DynDatabase for PrefixDb<D> {
    fn has(&self, key: &[u8]) -> Result<bool> {
        KeyValueReader::has(self, key)
    }
    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        KeyValueReader::get(self, key)
    }
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        KeyValueWriter::put(self, key, value)
    }
    fn delete(&self, key: &[u8]) -> Result<()> {
        KeyValueDeleter::delete(self, key)
    }
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        Batcher::new_batch(self)
    }
    fn new_iterator_with_start_and_prefix<'a>(
        &'a self,
        start: &[u8],
        prefix: &[u8],
    ) -> BoxIter<'a> {
        Box::new(Iteratee::new_iterator_with_start_and_prefix(
            self, start, prefix,
        ))
    }
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        Compacter::compact(self, start, limit)
    }
    fn close(&self) -> Result<()> {
        Database::close(self)
    }
    fn health_check(&self) -> Result<serde_json::Value> {
        Database::health_check(self)
    }
}

/// A batch over a [`PrefixDb`]: prefixes keys before buffering them onto the
/// inner DB's batch, while also recording the prefixed ops so [`Batch::replay`]
/// can strip the prefix back off (matching Go's `prefixdb.batch`).
struct PrefixBatch<'a, D: Database> {
    db: &'a PrefixDb<D>,
    inner: Box<dyn Batch + 'a>,
    /// Buffered ops with **prefixed** keys (for replay prefix-stripping).
    ops: BatchOps,
}

impl<D: Database> WriteDelete for PrefixBatch<'_, D> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let pk = self.db.prefixed(key);
        self.ops.put(&pk, value);
        let out = self.inner.put(&pk, value);
        self.db.pool.put(pk);
        out
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        let pk = self.db.prefixed(key);
        self.ops.delete(&pk);
        let out = self.inner.delete(&pk);
        self.db.pool.put(pk);
        out
    }
}

impl<D: Database> Batch for PrefixBatch<'_, D> {
    fn size(&self) -> usize {
        self.inner.size()
    }

    fn write(&mut self) -> Result<()> {
        if self.db.is_closed() {
            return Err(Error::Closed);
        }
        self.inner.write()
    }

    fn reset(&mut self) {
        self.ops.reset();
        self.inner.reset();
    }

    fn replay(&self, w: &mut dyn WriteDelete) -> Result<()> {
        let plen = self.db.db_prefix.len();
        for op in &self.ops.ops {
            let key = op.key.get(plen..).unwrap_or(&[]);
            if op.delete {
                w.delete(key)?;
            } else {
                w.put(key, &op.value)?;
            }
        }
        Ok(())
    }

    fn inner(&mut self) -> &mut dyn Batch {
        self
    }
}

/// A cursor over a [`PrefixDb`] that strips the prefix from returned keys
/// (matching Go's `prefixdb.iterator`).
pub struct PrefixIterator<'a, D: Database> {
    db: &'a PrefixDb<D>,
    inner: BoxIter<'a>,
    key: Option<Vec<u8>>,
    value: Option<Vec<u8>>,
    err: Option<Error>,
}

impl<D: Database> Iterator for PrefixIterator<'_, D> {
    fn next(&mut self) -> bool {
        if self.db.is_closed() {
            self.key = None;
            self.value = None;
            self.err = Some(Error::Closed);
            return false;
        }

        let has_next = self.inner.next();
        if has_next {
            let plen = self.db.db_prefix.len();
            let k = self.inner.key().unwrap_or(&[]);
            // Strip the prefix; defensively keep the whole key if it is shorter
            // than the prefix (matches Go's `len(key) >= prefixLen` guard).
            let stripped = if k.len() >= plen {
                k.get(plen..).unwrap_or(&[])
            } else {
                k
            };
            self.key = Some(stripped.to_vec());
            self.value = Some(self.inner.value().unwrap_or(&[]).to_vec());
        } else {
            self.key = None;
            self.value = None;
        }
        has_next
    }

    fn error(&self) -> Result<()> {
        if let Some(err) = &self.err {
            return Err(clone_err(err));
        }
        self.inner.error()
    }

    fn key(&self) -> Option<&[u8]> {
        self.key.as_deref()
    }

    fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    fn release(&mut self) {
        self.key = None;
        self.value = None;
        self.inner.release();
    }
}

/// Rebuilds an [`Error`] by value (the sentinels are unit variants; `Other`
/// re-wraps its message). Mirrors how [`IteratorError`] reconstructs errors.
fn clone_err(err: &Error) -> Error {
    match err {
        Error::Closed => Error::Closed,
        Error::NotFound => Error::NotFound,
        Error::Other(e) => Error::Other(anyhow::anyhow!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_prefix_is_sha256() {
        // SHA256("vm") — see the committed Go vector.
        assert_eq!(
            hex::encode(make_prefix(b"vm")),
            "5bce98f73f3ed0c837f2729ed9509b38ea66a156db7f653356cb6fe37b366e85"
        );
        assert_eq!(make_prefix(b"vm").len(), 32);
    }

    #[test]
    fn join_is_sha256_of_concat() {
        let parent = make_prefix(b"a");
        assert_eq!(
            hex::encode(join_prefixes(&parent, b"b")),
            "984f3f7d5798372d9f995b87369940292998ff35bd6bf414c1be64b2f9dfa7ca"
        );
    }

    #[test]
    fn increment_carries_and_wraps() {
        assert_eq!(increment_byte_slice(&[0x00]), vec![0x01]);
        assert_eq!(increment_byte_slice(&[0x01, 0xff]), vec![0x02, 0x00]);
        assert_eq!(increment_byte_slice(&[0xff, 0xff]), vec![0x00, 0x00]);
    }
}
