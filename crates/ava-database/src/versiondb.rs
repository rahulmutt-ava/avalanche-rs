// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `versiondb` — an in-memory overlay over a base DB (04 §2.4, 27 §2.2/§2.3),
//! mirroring `database/versiondb/db.go`.
//!
//! Writes accumulate in an in-memory overlay (`mem`); reads consult `mem` first
//! (a tombstone ⇒ [`Error::NotFound`]), else the base. [`VersionDb::commit`]
//! flushes `mem` into one base batch, writes it atomically, then clears `mem`;
//! [`VersionDb::abort`] just clears `mem`. [`VersionDb::commit_batch`] returns
//! the batch *without* writing it — the primitive the crash-consistent atomic
//! accept boundary composes into one multi-DB write (27 §2.2/§2.3).
//!
//! Keys are **passthrough** (no rewrite — 04 §10.1): a versiondb sits *inside*
//! an already-namespaced prefixdb in the real key catalog.
//!
//! The merge iterator walks the sorted `mem` snapshot and the base iterator in
//! lockstep, preferring `mem` on key ties and skipping tombstones — the exact Go
//! `Next()` state machine (exhausted-mem, exhausted-base, `memKey < dbKey`,
//! `dbKey < memKey`, equal) is ported verbatim.
//!
//! `mem` is an internal overlay, not a serialization path, so a `HashMap` is
//! acceptable here: iteration sorts a snapshot of the matching keys before
//! walking, and the map is never serialized.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::batch::BatchOps;
use crate::error::{Error, Result};
use crate::traits::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, KeyValueDeleter,
    KeyValueReader, KeyValueWriter, WriteDelete,
};

/// An overlay entry: either a buffered value or a tombstone (`valueDelete`).
#[derive(Clone)]
struct ValueOrDelete {
    value: Vec<u8>,
    delete: bool,
}

/// The mutable state, taken to `None` on [`VersionDb::close`] (Go's `mem == nil`).
struct Inner<D: Database> {
    /// The in-memory overlay (key ⇒ value-or-tombstone).
    mem: HashMap<Vec<u8>, ValueOrDelete>,
    /// The base DB, shared so [`VersionDb::commit_batch`] can hand back an owned
    /// batch that writes to it.
    db: Arc<D>,
}

/// An overlay DB that buffers writes until [`VersionDb::commit`] (04 §2.4).
pub struct VersionDb<D: Database> {
    inner: RwLock<Option<Inner<D>>>,
}

impl<D: Database> VersionDb<D> {
    /// Wraps `db` with a fresh, empty overlay.
    pub fn new(db: D) -> Self {
        Self::new_arc(Arc::new(db))
    }

    /// Like [`VersionDb::new`] but over an already-`Arc`'d base.
    pub fn new_arc(db: Arc<D>) -> Self {
        Self {
            inner: RwLock::new(Some(Inner {
                mem: HashMap::new(),
                db,
            })),
        }
    }

    /// Changes the underlying base DB (`SetDatabase`).
    pub fn set_database(&self, new_db: D) -> Result<()> {
        self.set_database_arc(Arc::new(new_db))
    }

    /// Like [`VersionDb::set_database`] but over an already-`Arc`'d base.
    pub fn set_database_arc(&self, new_db: Arc<D>) -> Result<()> {
        let mut guard = self.inner.write();
        let inner = guard.as_mut().ok_or(Error::Closed)?;
        inner.db = new_db;
        Ok(())
    }

    /// Returns the underlying base DB (`GetDatabase`).
    pub fn get_database(&self) -> Result<Arc<D>> {
        let guard = self.inner.read();
        let inner = guard.as_ref().ok_or(Error::Closed)?;
        Ok(Arc::clone(&inner.db))
    }

    /// Writes all buffered ops to the base atomically, then clears the overlay
    /// (`Commit`).
    pub fn commit(&self) -> Result<()> {
        let mut guard = self.inner.write();
        let inner = guard.as_mut().ok_or(Error::Closed)?;

        // Build a base batch from the overlay and write it atomically.
        let mut batch = inner.db.new_batch();
        for (key, vd) in &inner.mem {
            if vd.delete {
                batch.delete(key)?;
            } else {
                batch.put(key, &vd.value)?;
            }
        }
        batch.write()?;
        inner.mem.clear();
        Ok(())
    }

    /// Discards all buffered ops without writing (`Abort`).
    pub fn abort(&self) {
        if let Some(inner) = self.inner.write().as_mut() {
            inner.mem.clear();
        }
    }

    /// Returns a batch holding every uncommitted put/delete *without* writing it
    /// (`CommitBatch`). Calling [`Batch::write`] on the returned batch flushes
    /// them to the base; the caller is responsible for writing it before any
    /// future use of this DB (27 §2.2/§2.3). The overlay is left intact.
    pub fn commit_batch(&self) -> Result<VersionCommitBatch<D>> {
        let guard = self.inner.read();
        let inner = guard.as_ref().ok_or(Error::Closed)?;

        let mut ops = BatchOps::new();
        for (key, vd) in &inner.mem {
            if vd.delete {
                ops.delete(key);
            } else {
                ops.put(key, &vd.value);
            }
        }
        Ok(VersionCommitBatch {
            db: Arc::clone(&inner.db),
            ops,
        })
    }
}

impl<D: Database> KeyValueReader for VersionDb<D> {
    fn has(&self, key: &[u8]) -> Result<bool> {
        let guard = self.inner.read();
        let inner = guard.as_ref().ok_or(Error::Closed)?;
        if let Some(vd) = inner.mem.get(key) {
            return Ok(!vd.delete);
        }
        inner.db.has(key)
    }

    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        let guard = self.inner.read();
        let inner = guard.as_ref().ok_or(Error::Closed)?;
        if let Some(vd) = inner.mem.get(key) {
            if vd.delete {
                return Err(Error::NotFound);
            }
            return Ok(vd.value.clone());
        }
        inner.db.get(key)
    }
}

impl<D: Database> KeyValueWriter for VersionDb<D> {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let mut guard = self.inner.write();
        let inner = guard.as_mut().ok_or(Error::Closed)?;
        inner.mem.insert(
            key.to_vec(),
            ValueOrDelete {
                value: value.to_vec(),
                delete: false,
            },
        );
        Ok(())
    }
}

impl<D: Database> KeyValueDeleter for VersionDb<D> {
    fn delete(&self, key: &[u8]) -> Result<()> {
        let mut guard = self.inner.write();
        let inner = guard.as_mut().ok_or(Error::Closed)?;
        inner.mem.insert(
            key.to_vec(),
            ValueOrDelete {
                value: Vec::new(),
                delete: true,
            },
        );
        Ok(())
    }
}

impl<D: Database> Compacter for VersionDb<D> {
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        let guard = self.inner.read();
        let inner = guard.as_ref().ok_or(Error::Closed)?;
        inner.db.compact(start, limit)
    }
}

impl<D: Database> Batcher for VersionDb<D> {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        Box::new(VersionBatch {
            db: self,
            ops: BatchOps::new(),
        })
    }
}

impl<D: Database> Iteratee for VersionDb<D> {
    type Iter<'a>
        = VersionIterator<'a, D>
    where
        D: 'a;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> Self::Iter<'_> {
        let guard = self.inner.read();
        let Some(inner) = guard.as_ref() else {
            return VersionIterator {
                db: self,
                entries: Vec::new(),
                head: 0,
                base_entries: Vec::new(),
                base_head: 0,
                base_err: None,
                key: None,
                value: None,
                err: Some(Error::Closed),
            };
        };

        // Snapshot the overlay keys matching start+prefix, in sorted order.
        let mut entries: Vec<(Vec<u8>, ValueOrDelete)> = inner
            .mem
            .iter()
            .filter(|(k, _)| k.starts_with(prefix) && k.as_slice() >= start)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Snapshot the base iterator's entries too. The base (e.g. MemDb) already
        // yields a point-in-time snapshot, so materializing it here is equivalent
        // to holding the live iterator, and keeps the cursor free of the lock's
        // lifetime. Already-sorted by the base's key ordering.
        let mut base_it = inner.db.new_iterator_with_start_and_prefix(start, prefix);
        let mut base_entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        while base_it.next() {
            base_entries.push((
                base_it.key().unwrap_or(&[]).to_vec(),
                base_it.value().unwrap_or(&[]).to_vec(),
            ));
        }
        let base_err = base_it.error().err();

        VersionIterator {
            db: self,
            entries,
            head: 0,
            base_entries,
            base_head: 0,
            base_err,
            key: None,
            value: None,
            err: None,
        }
    }
}

impl<D: Database> Database for VersionDb<D> {
    fn close(&self) -> Result<()> {
        let mut guard = self.inner.write();
        if guard.is_none() {
            return Err(Error::Closed);
        }
        *guard = None;
        Ok(())
    }

    fn health_check(&self) -> Result<serde_json::Value> {
        let guard = self.inner.read();
        let inner = guard.as_ref().ok_or(Error::Closed)?;
        inner.db.health_check()
    }
}

impl<D: Database> DynDatabase for VersionDb<D> {
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

/// A batch over a [`VersionDb`]; [`Batch::write`] flushes its ops into the
/// overlay (`mem`), not the base (matching Go's versiondb `batch`).
struct VersionBatch<'a, D: Database> {
    db: &'a VersionDb<D>,
    ops: BatchOps,
}

impl<D: Database> WriteDelete for VersionBatch<'_, D> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.ops.put(key, value);
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.ops.delete(key);
        Ok(())
    }
}

impl<D: Database> Batch for VersionBatch<'_, D> {
    fn size(&self) -> usize {
        self.ops.size()
    }

    fn write(&mut self) -> Result<()> {
        let mut guard = self.db.inner.write();
        let inner = guard.as_mut().ok_or(Error::Closed)?;
        for op in &self.ops.ops {
            inner.mem.insert(
                op.key.clone(),
                ValueOrDelete {
                    value: op.value.clone(),
                    delete: op.delete,
                },
            );
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

/// The unwritten batch returned by [`VersionDb::commit_batch`]: it owns an
/// `Arc` of the base DB plus the buffered overlay ops, and on [`Batch::write`]
/// replays them onto a fresh base batch and writes that atomically.
pub struct VersionCommitBatch<D: Database> {
    db: Arc<D>,
    ops: BatchOps,
}

impl<D: Database> WriteDelete for VersionCommitBatch<D> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.ops.put(key, value);
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.ops.delete(key);
        Ok(())
    }
}

impl<D: Database> Batch for VersionCommitBatch<D> {
    fn size(&self) -> usize {
        self.ops.size()
    }

    fn write(&mut self) -> Result<()> {
        let mut batch = self.db.new_batch();
        self.ops.replay(batch.as_mut())?;
        batch.write()
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

/// The merge cursor over a [`VersionDb`]: walks the sorted overlay snapshot and
/// the (also-snapshotted) base entries in lockstep, preferring the overlay on
/// key ties and skipping tombstones. Ports Go's `versiondb.iterator` `Next()`
/// state machine (exhausted-mem, exhausted-base, `memKey < dbKey`,
/// `dbKey < memKey`, equal) — both sides are snapshotted at creation, which is
/// equivalent to Go's live base iterator since the base yields a point-in-time
/// snapshot (04 §2.2/§2.4) and keeps the cursor free of the lock's lifetime.
pub struct VersionIterator<'a, D: Database> {
    db: &'a VersionDb<D>,
    /// The sorted overlay snapshot; `head` is the index of the next entry.
    entries: Vec<(Vec<u8>, ValueOrDelete)>,
    head: usize,
    /// The sorted base snapshot; `base_head` is the index of the next entry.
    base_entries: Vec<(Vec<u8>, Vec<u8>)>,
    base_head: usize,
    /// Any error observed while draining the base iterator at creation.
    base_err: Option<Error>,
    key: Option<Vec<u8>>,
    value: Option<Vec<u8>>,
    err: Option<Error>,
}

impl<D: Database> VersionIterator<'_, D> {
    /// Remaining overlay entries (Go's `len(it.keys)`).
    fn mem_len(&self) -> usize {
        self.entries.len().saturating_sub(self.head)
    }

    /// The next overlay key, if any (Go's `it.keys[0]`).
    fn mem_key(&self) -> Option<&[u8]> {
        self.entries.get(self.head).map(|(k, _)| k.as_slice())
    }

    /// Advances past the head overlay entry, returning its value-or-delete.
    fn pop_mem(&mut self) -> Option<(Vec<u8>, ValueOrDelete)> {
        let entry = self.entries.get(self.head).cloned();
        if entry.is_some() {
            self.head = self.head.saturating_add(1);
        }
        entry
    }

    /// Whether the base snapshot is exhausted (Go's `it.exhausted`).
    fn base_exhausted(&self) -> bool {
        self.base_head >= self.base_entries.len()
    }

    /// The current base key, if any (Go's `it.Iterator.Key()`).
    fn base_key(&self) -> Option<&[u8]> {
        self.base_entries
            .get(self.base_head)
            .map(|(k, _)| k.as_slice())
    }

    /// The current base value, if any (Go's `it.Iterator.Value()`).
    fn base_value(&self) -> Option<&[u8]> {
        self.base_entries
            .get(self.base_head)
            .map(|(_, v)| v.as_slice())
    }

    /// Advances the base snapshot one step (Go's `it.Iterator.Next()`).
    fn advance_base(&mut self) {
        self.base_head = self.base_head.saturating_add(1);
    }

    fn is_db_closed(&self) -> bool {
        self.db.inner.read().is_none()
    }
}

impl<D: Database> Iterator for VersionIterator<'_, D> {
    fn next(&mut self) -> bool {
        // Short-circuit and set an error if the underlying DB was closed.
        if self.is_db_closed() {
            self.key = None;
            self.value = None;
            self.err = Some(Error::Closed);
            return false;
        }

        loop {
            if self.base_exhausted() && self.mem_len() == 0 {
                // Both exhausted.
                self.key = None;
                self.value = None;
                return false;
            } else if self.base_exhausted() {
                // Only overlay entries remain. `pop_mem` advances regardless; a
                // tombstone is skipped (loop continues).
                if let Some((next_key, next_value)) = self.pop_mem()
                    && !next_value.delete
                {
                    self.key = Some(next_key);
                    self.value = Some(next_value.value);
                    return true;
                }
            } else if self.mem_len() == 0 {
                // Only base entries remain.
                self.key = self.base_key().map(<[u8]>::to_vec);
                self.value = self.base_value().map(<[u8]>::to_vec);
                self.advance_base();
                return true;
            } else {
                // Both have entries: compare the heads.
                let mem_key = self.mem_key().unwrap_or(&[]).to_vec();
                let db_key = self.base_key().unwrap_or(&[]).to_vec();

                if mem_key < db_key {
                    // `pop_mem` advances regardless; a tombstone is skipped.
                    if let Some((k, v)) = self.pop_mem()
                        && !v.delete
                    {
                        self.key = Some(k);
                        self.value = Some(v.value);
                        return true;
                    }
                } else if db_key < mem_key {
                    self.key = Some(db_key);
                    self.value = self.base_value().map(<[u8]>::to_vec);
                    self.advance_base();
                    return true;
                } else {
                    // Equal keys: prefer the overlay; advance both. An overlay
                    // tombstone shadows the base value, so both are skipped.
                    let popped = self.pop_mem();
                    self.advance_base();
                    if let Some((k, v)) = popped
                        && !v.delete
                    {
                        self.key = Some(k);
                        self.value = Some(v.value);
                        return true;
                    }
                }
            }
        }
    }

    fn error(&self) -> Result<()> {
        if let Some(err) = &self.err {
            return Err(clone_err(err));
        }
        if let Some(err) = &self.base_err {
            return Err(clone_err(err));
        }
        Ok(())
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
        self.entries.clear();
        self.head = 0;
        self.base_entries.clear();
        self.base_head = 0;
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
    use super::*;
    use crate::MemDb;

    // UFCS shorthands: `VersionDb`/`MemDb` implement both the typed trait family
    // and the object-safe `DynDatabase`, so bare method calls are ambiguous.
    fn put<D: Database>(db: &D, k: &[u8], v: &[u8]) {
        KeyValueWriter::put(db, k, v).unwrap();
    }
    fn del<D: Database>(db: &D, k: &[u8]) {
        KeyValueDeleter::delete(db, k).unwrap();
    }
    fn get<D: Database>(db: &D, k: &[u8]) -> Result<Vec<u8>> {
        KeyValueReader::get(db, k)
    }
    fn has<D: Database>(db: &D, k: &[u8]) -> bool {
        KeyValueReader::has(db, k).unwrap()
    }

    /// Collects `(key, value)` from an iterator into a `Vec`.
    fn drain<I: Iterator>(mut it: I) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        while it.next() {
            out.push((
                it.key().unwrap_or(&[]).to_vec(),
                it.value().unwrap_or(&[]).to_vec(),
            ));
        }
        out
    }

    #[test]
    fn merge_iterator_ties() {
        let base = MemDb::new();
        // Base has a, b, c.
        put(&base, b"a", b"base-a");
        put(&base, b"b", b"base-b");
        put(&base, b"c", b"base-c");

        let vdb = VersionDb::new(base);
        // Overlay: shadow b (tie ⇒ prefer overlay), add d (mem-only),
        // tombstone c (delete shadows base), leave a from base.
        put(&vdb, b"b", b"mem-b");
        put(&vdb, b"d", b"mem-d");
        del(&vdb, b"c");

        let got = drain(vdb.new_iterator());
        assert_eq!(
            got,
            vec![
                (b"a".to_vec(), b"base-a".to_vec()), // base-only
                (b"b".to_vec(), b"mem-b".to_vec()),  // tie ⇒ overlay wins
                (b"d".to_vec(), b"mem-d".to_vec()),  // overlay-only (c skipped)
            ]
        );
    }

    #[test]
    fn merge_iterator_mem_only_and_base_only_ordering() {
        let base = MemDb::new();
        put(&base, b"m", b"base-m");
        let vdb = VersionDb::new(base);
        // Overlay key before and after the base key, exercising memKey<dbKey and
        // dbKey<memKey branches.
        put(&vdb, b"a", b"mem-a");
        put(&vdb, b"z", b"mem-z");

        let got = drain(vdb.new_iterator());
        assert_eq!(
            got,
            vec![
                (b"a".to_vec(), b"mem-a".to_vec()),
                (b"m".to_vec(), b"base-m".to_vec()),
                (b"z".to_vec(), b"mem-z".to_vec()),
            ]
        );
    }

    #[test]
    fn commit_flushes_and_clears_overlay() {
        let base_arc = Arc::new(MemDb::new());
        let vdb = VersionDb::new_arc(Arc::clone(&base_arc));
        put(&vdb, b"k", b"v");
        del(&vdb, b"gone");
        // Before commit the base is untouched.
        assert!(matches!(get(&*base_arc, b"k"), Err(Error::NotFound)));
        vdb.commit().unwrap();
        // After commit the base has the put and the overlay is cleared.
        assert_eq!(get(&*base_arc, b"k").unwrap(), b"v");
        // A fresh get goes straight to the base (overlay empty).
        assert_eq!(get(&vdb, b"k").unwrap(), b"v");
    }

    #[test]
    fn abort_discards_overlay() {
        let base = MemDb::new();
        let vdb = VersionDb::new(base);
        put(&vdb, b"k", b"v");
        vdb.abort();
        assert!(matches!(get(&vdb, b"k"), Err(Error::NotFound)));
    }

    #[test]
    fn commit_batch_is_unwritten_until_write() {
        let base_arc = Arc::new(MemDb::new());
        let vdb = VersionDb::new_arc(Arc::clone(&base_arc));
        put(&vdb, b"k", b"v");

        let mut batch = vdb.commit_batch().unwrap();
        // Not written yet: the base is still empty and the overlay intact.
        assert!(matches!(get(&*base_arc, b"k"), Err(Error::NotFound)));
        assert_eq!(get(&vdb, b"k").unwrap(), b"v"); // still served from overlay

        batch.write().unwrap();
        assert_eq!(get(&*base_arc, b"k").unwrap(), b"v");
    }

    #[test]
    fn tombstone_reads_as_not_found() {
        let base = MemDb::new();
        put(&base, b"k", b"v");
        let vdb = VersionDb::new(base);
        assert_eq!(get(&vdb, b"k").unwrap(), b"v");
        del(&vdb, b"k");
        assert!(matches!(get(&vdb, b"k"), Err(Error::NotFound)));
        assert!(!has(&vdb, b"k"));
    }
}
