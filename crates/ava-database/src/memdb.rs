// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `memdb` — the in-memory reference backend (04 §2.2), mirroring
//! `database/memdb/db.go`.
//!
//! Storage is `parking_lot::RwLock<Option<BTreeMap<Vec<u8>, Vec<u8>>>>`: the
//! `Option` models Go's `db == nil` after `Close` (post-close ops return
//! [`Error::Closed`]). `BTreeMap` gives ordered iteration for free. `get`
//! clones the value (memory-safety contract). Iterators snapshot the relevant
//! range into a `Vec` so they stay independent of later mutation
//! (`TestIteratorSnapshot`).

use std::collections::BTreeMap;
use std::ops::Bound;

use parking_lot::RwLock;

use crate::batch::BatchOps;
use crate::error::{Error, Result};
use crate::traits::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, KeyValueDeleter,
    KeyValueReader, KeyValueWriter, WriteDelete,
};

/// An ephemeral, ordered key/value store. `None` ⇒ closed.
#[derive(Default)]
pub struct MemDb {
    db: RwLock<Option<BTreeMap<Vec<u8>, Vec<u8>>>>,
}

impl MemDb {
    /// Creates an empty in-memory database.
    pub fn new() -> Self {
        Self {
            db: RwLock::new(Some(BTreeMap::new())),
        }
    }

    /// Whether the database has been closed.
    fn is_closed(&self) -> bool {
        self.db.read().is_none()
    }

    /// Snapshots the entries with `prefix`, starting at `start` (inclusive),
    /// into a `Vec` independent of later mutation. Returns `Err(Closed)` if the
    /// DB is closed.
    fn snapshot(&self, start: &[u8], prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let guard = self.db.read();
        let map = guard.as_ref().ok_or(Error::Closed)?;

        // Iterate from max(start, prefix) — both lower bounds — and stop once a
        // key no longer has `prefix` (BTreeMap is sorted, so prefix-matching
        // keys are contiguous once reached).
        let lower: &[u8] = if start > prefix { start } else { prefix };
        let mut out = Vec::new();
        for (k, v) in map.range::<[u8], _>((Bound::Included(lower), Bound::Unbounded)) {
            if !k.starts_with(prefix) {
                // Past the prefix block: since keys are sorted and we started at
                // or after the prefix, the first non-matching key after a match
                // ends the run. But a key < prefix-start could still precede the
                // block, so only break once we've left the prefix range.
                if k.as_slice() < prefix {
                    continue;
                }
                break;
            }
            if k.as_slice() < start {
                continue;
            }
            out.push((k.clone(), v.clone()));
        }
        Ok(out)
    }
}

impl KeyValueReader for MemDb {
    fn has(&self, key: &[u8]) -> Result<bool> {
        let guard = self.db.read();
        let map = guard.as_ref().ok_or(Error::Closed)?;
        Ok(map.contains_key(key))
    }

    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        let guard = self.db.read();
        let map = guard.as_ref().ok_or(Error::Closed)?;
        map.get(key).cloned().ok_or(Error::NotFound)
    }
}

impl KeyValueWriter for MemDb {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let mut guard = self.db.write();
        let map = guard.as_mut().ok_or(Error::Closed)?;
        map.insert(key.to_vec(), value.to_vec());
        Ok(())
    }
}

impl KeyValueDeleter for MemDb {
    fn delete(&self, key: &[u8]) -> Result<()> {
        let mut guard = self.db.write();
        let map = guard.as_mut().ok_or(Error::Closed)?;
        map.remove(key);
        Ok(())
    }
}

impl Compacter for MemDb {
    fn compact(&self, _start: Option<&[u8]>, _limit: Option<&[u8]>) -> Result<()> {
        let guard = self.db.read();
        guard.as_ref().ok_or(Error::Closed)?;
        Ok(())
    }
}

impl Batcher for MemDb {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        Box::new(MemBatch {
            db: self,
            ops: BatchOps::new(),
        })
    }
}

impl Iteratee for MemDb {
    type Iter<'a> = MemIterator<'a>;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> MemIterator<'_> {
        match self.snapshot(start, prefix) {
            Ok(entries) => MemIterator {
                db: self,
                entries,
                pos: None,
                err: None,
            },
            Err(err) => MemIterator {
                db: self,
                entries: Vec::new(),
                pos: None,
                err: Some(err),
            },
        }
    }
}

impl Database for MemDb {
    fn close(&self) -> Result<()> {
        let mut guard = self.db.write();
        if guard.is_none() {
            return Err(Error::Closed);
        }
        *guard = None;
        Ok(())
    }

    fn health_check(&self) -> Result<serde_json::Value> {
        let guard = self.db.read();
        guard.as_ref().ok_or(Error::Closed)?;
        Ok(serde_json::Value::Null)
    }
}

impl DynDatabase for MemDb {
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

/// A write-only batch over a [`MemDb`]; buffers ops until [`Batch::write`].
struct MemBatch<'a> {
    db: &'a MemDb,
    ops: BatchOps,
}

impl WriteDelete for MemBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.ops.put(key, value);
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.ops.delete(key);
        Ok(())
    }
}

impl Batch for MemBatch<'_> {
    fn size(&self) -> usize {
        self.ops.size()
    }

    fn write(&mut self) -> Result<()> {
        let mut guard = self.db.db.write();
        let map = guard.as_mut().ok_or(Error::Closed)?;
        for op in &self.ops.ops {
            if op.delete {
                map.remove(&op.key);
            } else {
                map.insert(op.key.clone(), op.value.clone());
            }
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.ops.reset();
    }

    fn replay(&self, w: &mut dyn WriteDelete) -> Result<()> {
        self.ops.replay(w)
    }

    fn inner(&mut self) -> &mut dyn Batch {
        self
    }
}

/// A point-in-time snapshot cursor over a [`MemDb`].
///
/// Holds an owned `Vec` snapshot so it is independent of later mutation
/// (`TestIteratorSnapshot`). It re-checks the DB's closed state on each
/// [`Iterator::next`] (matching Go's memdb iterator, which short-circuits and
/// reports `ErrClosed` once the DB closes).
pub struct MemIterator<'a> {
    db: &'a MemDb,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
    /// `None` before the first `next`; `Some(i)` for the current index.
    pos: Option<usize>,
    err: Option<Error>,
}

impl Iterator for MemIterator<'_> {
    fn next(&mut self) -> bool {
        // A pre-set error (e.g. iterator created on a closed DB) yields nothing.
        if self.err.is_some() {
            self.entries.clear();
            self.pos = None;
            return false;
        }

        // Short-circuit if the DB was closed after this iterator was created:
        // report ErrClosed and stop (matches Go's memdb iterator).
        if self.db.is_closed() {
            self.entries.clear();
            self.pos = None;
            self.err = Some(Error::Closed);
            return false;
        }

        match self.pos {
            None => {
                if self.entries.is_empty() {
                    false
                } else {
                    self.pos = Some(0);
                    true
                }
            }
            Some(i) => {
                let next = i.saturating_add(1);
                if next < self.entries.len() {
                    self.pos = Some(next);
                    true
                } else {
                    // Exhausted: drop position so key/value report None.
                    self.pos = None;
                    self.entries.clear();
                    false
                }
            }
        }
    }

    fn error(&self) -> Result<()> {
        match &self.err {
            None => Ok(()),
            Some(Error::Closed) => Err(Error::Closed),
            Some(Error::NotFound) => Err(Error::NotFound),
            Some(Error::Other(e)) => Err(Error::Other(anyhow::anyhow!("{e}"))),
        }
    }

    fn key(&self) -> Option<&[u8]> {
        self.pos
            .and_then(|i| self.entries.get(i))
            .map(|(k, _)| k.as_slice())
    }

    fn value(&self) -> Option<&[u8]> {
        self.pos
            .and_then(|i| self.entries.get(i))
            .map(|(_, v)| v.as_slice())
    }

    fn release(&mut self) {
        self.entries.clear();
        self.pos = None;
    }
}
