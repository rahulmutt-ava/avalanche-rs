// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `corruptabledb` — poison-on-error wrapper (04 §2.6, 27 §6.1), mirroring
//! `database/corruptabledb/db.go`.
//!
//! Wraps any [`Database`]. On any error **other than** [`Error::Closed`] /
//! [`Error::NotFound`] (i.e. an [`Error::Other`]), it latches an initial error
//! under a lock and every subsequent op returns it — "closing the database to
//! avoid possible corruption." `Closed`/`NotFound` are normal control flow and
//! never poison (27 §6.1).
//!
//! The node wraps its base on-disk DB in this so a single IO fault halts writes
//! rather than corrupting state; a poisoned base DB fails *all* chains (it is the
//! shared base). Keys are passthrough (04 §10.1).

use parking_lot::RwLock;

use crate::error::{Error, Result};
use crate::traits::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, KeyValueDeleter,
    KeyValueReader, KeyValueWriter, WriteDelete,
};

/// A [`Database`] wrapper that latches the first non-sentinel error and refuses
/// all further operations (04 §2.6).
pub struct CorruptableDb<D: Database> {
    inner: D,
    /// The latched initial error, set once on the first [`Error::Other`].
    initial_error: RwLock<Option<Error>>,
}

impl<D: Database> CorruptableDb<D> {
    /// Wraps `inner`. The wrapper starts un-poisoned.
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            initial_error: RwLock::new(None),
        }
    }

    /// Returns the latched error if the DB has been poisoned (`corrupted`).
    fn check(&self) -> Result<()> {
        match self.initial_error.read().as_ref() {
            Some(err) => Err(clone_err(err)),
            None => Ok(()),
        }
    }

    /// Inspects a result: on an [`Error::Other`], latches an initial error (once)
    /// so future ops fail; [`Error::Closed`]/[`Error::NotFound`] pass through
    /// untouched (`handleError`). Returns the result unchanged.
    fn handle_error<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(Error::Other(e)) = &result {
            let mut guard = self.initial_error.write();
            if guard.is_none() {
                // Match Go's wrapping message so logs/RPC formatting stay aligned.
                *guard = Some(Error::Other(anyhow::anyhow!(
                    "closed to avoid possible corruption, init error: {e}"
                )));
            }
        }
        result
    }
}

impl<D: Database> KeyValueReader for CorruptableDb<D> {
    fn has(&self, key: &[u8]) -> Result<bool> {
        self.check()?;
        let out = self.inner.has(key);
        self.handle_error(out)
    }

    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        self.check()?;
        let out = self.inner.get(key);
        self.handle_error(out)
    }
}

impl<D: Database> KeyValueWriter for CorruptableDb<D> {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.check()?;
        let out = self.inner.put(key, value);
        self.handle_error(out)
    }
}

impl<D: Database> KeyValueDeleter for CorruptableDb<D> {
    fn delete(&self, key: &[u8]) -> Result<()> {
        self.check()?;
        let out = self.inner.delete(key);
        self.handle_error(out)
    }
}

impl<D: Database> Compacter for CorruptableDb<D> {
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        // Go's Compact does not pre-check corrupted(); it only handles the error.
        let out = self.inner.compact(start, limit);
        self.handle_error(out)
    }
}

impl<D: Database> Batcher for CorruptableDb<D> {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        Box::new(CorruptableBatch {
            db: self,
            inner: self.inner.new_batch(),
        })
    }
}

impl<D: Database> Iteratee for CorruptableDb<D> {
    type Iter<'a>
        = CorruptableIterator<'a, D>
    where
        D: 'a;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> Self::Iter<'_> {
        CorruptableIterator {
            db: self,
            inner: Box::new(self.inner.new_iterator_with_start_and_prefix(start, prefix)),
        }
    }
}

impl<D: Database> Database for CorruptableDb<D> {
    fn close(&self) -> Result<()> {
        let out = self.inner.close();
        self.handle_error(out)
    }

    fn health_check(&self) -> Result<serde_json::Value> {
        self.check()?;
        self.inner.health_check()
    }
}

impl<D: Database> DynDatabase for CorruptableDb<D> {
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

/// A batch over a [`CorruptableDb`]; [`Batch::write`] checks/handles poison
/// around the inner write (matching Go's `corruptabledb.batch`).
struct CorruptableBatch<'a, D: Database> {
    db: &'a CorruptableDb<D>,
    inner: Box<dyn Batch + 'a>,
}

impl<D: Database> WriteDelete for CorruptableBatch<'_, D> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.inner.put(key, value)
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.inner.delete(key)
    }
}

impl<D: Database> Batch for CorruptableBatch<'_, D> {
    fn size(&self) -> usize {
        self.inner.size()
    }

    fn write(&mut self) -> Result<()> {
        self.db.check()?;
        let out = self.inner.write();
        self.db.handle_error(out)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn replay(&self, w: &mut dyn WriteDelete) -> Result<()> {
        self.inner.replay(w)
    }

    fn inner(&mut self) -> &mut dyn Batch {
        self
    }
}

/// A cursor over a [`CorruptableDb`]: short-circuits when poisoned and feeds the
/// inner iterator's error through [`CorruptableDb::handle_error`] (matching Go's
/// `corruptabledb.iterator`).
pub struct CorruptableIterator<'a, D: Database> {
    db: &'a CorruptableDb<D>,
    inner: BoxIter<'a>,
}

impl<D: Database> Iterator for CorruptableIterator<'_, D> {
    fn next(&mut self) -> bool {
        if self.db.check().is_err() {
            return false;
        }
        let val = self.inner.next();
        let _ = self.db.handle_error(self.inner.error());
        val
    }

    fn error(&self) -> Result<()> {
        self.db.check()?;
        self.db.handle_error(self.inner.error())
    }

    fn key(&self) -> Option<&[u8]> {
        self.inner.key()
    }

    fn value(&self) -> Option<&[u8]> {
        self.inner.value()
    }

    fn release(&mut self) {
        self.inner.release();
    }
}

/// Rebuilds an [`Error`] by value (mirrors [`crate::traits::IteratorError`]).
fn clone_err(err: &Error) -> Error {
    match err {
        Error::Closed => Error::Closed,
        Error::NotFound => Error::NotFound,
        Error::Other(e) => Error::Other(anyhow::anyhow!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::MemDb;

    /// A test-only failpoint DB wrapping a [`MemDb`]: when `fail` is set, `get`
    /// (and only `get`) returns an injected [`Error::Other`]; otherwise it
    /// passes through. Used to drive the poison latch.
    struct FailpointDb {
        inner: MemDb,
        fail: AtomicBool,
    }

    impl FailpointDb {
        fn new() -> Self {
            Self {
                inner: MemDb::new(),
                fail: AtomicBool::new(false),
            }
        }
        fn arm(&self) {
            self.fail.store(true, Ordering::SeqCst);
        }
        fn disarm(&self) {
            self.fail.store(false, Ordering::SeqCst);
        }
    }

    // `MemDb` implements both the typed trait family and `DynDatabase`, so the
    // forwarding calls below use UFCS to disambiguate.
    impl KeyValueReader for FailpointDb {
        fn has(&self, key: &[u8]) -> Result<bool> {
            KeyValueReader::has(&self.inner, key)
        }
        fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(Error::Other(anyhow::anyhow!("injected io fault")));
            }
            KeyValueReader::get(&self.inner, key)
        }
    }
    impl KeyValueWriter for FailpointDb {
        fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
            KeyValueWriter::put(&self.inner, key, value)
        }
    }
    impl KeyValueDeleter for FailpointDb {
        fn delete(&self, key: &[u8]) -> Result<()> {
            KeyValueDeleter::delete(&self.inner, key)
        }
    }
    impl Compacter for FailpointDb {
        fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
            Compacter::compact(&self.inner, start, limit)
        }
    }
    impl Batcher for FailpointDb {
        fn new_batch(&self) -> Box<dyn Batch + '_> {
            Batcher::new_batch(&self.inner)
        }
    }
    impl Iteratee for FailpointDb {
        type Iter<'a> = crate::memdb::MemIterator<'a>;
        fn new_iterator_with_start_and_prefix(
            &self,
            start: &[u8],
            prefix: &[u8],
        ) -> Self::Iter<'_> {
            Iteratee::new_iterator_with_start_and_prefix(&self.inner, start, prefix)
        }
    }
    impl Database for FailpointDb {
        fn close(&self) -> Result<()> {
            Database::close(&self.inner)
        }
        fn health_check(&self) -> Result<serde_json::Value> {
            Database::health_check(&self.inner)
        }
    }

    #[test]
    fn poison_latches_on_other() {
        let fp = FailpointDb::new();
        KeyValueWriter::put(&fp, b"k", b"v").unwrap();
        let db = CorruptableDb::new(fp);

        // Normal op succeeds.
        assert_eq!(KeyValueReader::get(&db, b"k").unwrap(), b"v");

        // NotFound does NOT poison.
        assert!(matches!(
            KeyValueReader::get(&db, b"absent"),
            Err(Error::NotFound)
        ));
        // Still usable.
        assert_eq!(KeyValueReader::get(&db, b"k").unwrap(), b"v");

        // Inject an Error::Other on the next get.
        db.inner.arm();
        assert!(matches!(
            KeyValueReader::get(&db, b"k"),
            Err(Error::Other(_))
        ));

        // Now the DB is poisoned: even after disarming the failpoint, every op
        // returns the latched error.
        db.inner.disarm();
        assert!(matches!(
            KeyValueReader::get(&db, b"k"),
            Err(Error::Other(_))
        ));
        assert!(matches!(
            KeyValueReader::has(&db, b"k"),
            Err(Error::Other(_))
        ));
        assert!(matches!(
            KeyValueWriter::put(&db, b"k", b"v2"),
            Err(Error::Other(_))
        ));
        assert!(matches!(
            KeyValueDeleter::delete(&db, b"k"),
            Err(Error::Other(_))
        ));
    }

    #[test]
    fn closed_and_not_found_do_not_latch() {
        let fp = FailpointDb::new();
        let db = CorruptableDb::new(fp);

        // A NotFound from an empty DB must not latch.
        assert!(matches!(
            KeyValueReader::get(&db, b"x"),
            Err(Error::NotFound)
        ));
        // A Closed from the inner DB must not latch either: close once, then a
        // subsequent op sees ErrClosed but the wrapper stays un-poisoned.
        Database::close(&db).unwrap();
        assert!(matches!(KeyValueReader::get(&db, b"x"), Err(Error::Closed)));
        // check() is still Ok (not poisoned) — the error came from the inner DB,
        // not the latch.
        assert!(db.check().is_ok());
    }
}
