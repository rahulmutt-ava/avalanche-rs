// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `rocksdb` — the on-disk default backend (04 §2.1), replacing avalanchego's
//! `leveldb` *and* `pebbledb`. Wraps the `rust-rocksdb` (`rocksdb`) crate.
//!
//! Per overview §4.4 RocksDB is the production backend (closest ordered/prefix/
//! batch/snapshot semantics to goleveldb + Pebble). Wrapper semantics:
//!
//! - `get` → [`Error::NotFound`] on `Ok(None)`; `has` via `get_pinned`
//!   (zero-copy) returning `is_some()`.
//! - **Iterators** are point-in-time: created over a RocksDB **snapshot** and
//!   the matching `(start, prefix)` range is collected into an owned buffer at
//!   creation, so they stay independent of later mutation (`TestIteratorSnapshot`).
//!   The wrapper applies the Go `start ≥` AND `HasPrefix` predicate itself
//!   (RocksDB prefix-seek alone is insufficient for arbitrary prefixes).
//! - **Batch** = `rust_rocksdb::WriteBatch`; `write()` is atomic.
//! - **Close** is gated by an `AtomicBool` so post-close ops return
//!   [`Error::Closed`] (Go returns `ErrClosed`, not a panic).
//! - **`health_check`** reports `rocksdb.estimate-live-data-size` as a JSON blob.
//!
//! This is the one `unsafe`-permitted backend (04 §7.6 / 00 §7.6): the FFI lives
//! entirely inside the `rocksdb` crate (`librocksdb-sys`), so this wrapper
//! itself contains **no** `unsafe` — the crate's `#![forbid(unsafe_code)]` still
//! holds. The audited `unsafe` is isolated in the external FFI crate.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rocksdb::{DB, IteratorMode, Options, ReadOptions, WriteBatch};
use tempfile::TempDir;

use crate::batch::BatchOps;
use crate::error::{Error, Result};
use crate::traits::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, KeyValueDeleter,
    KeyValueReader, KeyValueWriter, WriteDelete,
};

/// Tuning knobs mirroring avalanchego's leveldb/pebble JSON DB-config keys
/// (`database/leveldb/db.go`). Perf knobs, **not** protocol — surfaced through
/// `ava-config`.
#[derive(Clone, Debug)]
pub struct RocksDbConfig {
    /// Block-cache size in bytes (`BlockCacheCapacity`).
    pub block_cache_size: usize,
    /// Per-memtable write-buffer size in bytes (`WriteBuffer`).
    pub write_buffer_size: usize,
    /// Max open file descriptors (`HandleCap`; `-1` ⇒ unlimited).
    pub max_open_files: i32,
    /// Bits-per-key for the bloom filter (`FilterBitsPerKey`; `0` ⇒ disabled).
    pub bloom_filter_bits: i32,
}

impl Default for RocksDbConfig {
    fn default() -> Self {
        // Mirrors avalanchego's leveldb defaults (12 MiB cache, 12 MiB write
        // buffer, 1024 open files, 10-bit bloom).
        Self {
            block_cache_size: 12 * 1024 * 1024,
            write_buffer_size: 12 * 1024 * 1024,
            max_open_files: 1024,
            bloom_filter_bits: 10,
        }
    }
}

impl RocksDbConfig {
    /// Builds RocksDB `Options` from this config (create-if-missing, level
    /// compaction, LZ4 compression — matching the leveldb/pebble tuning shape).
    fn to_options(&self) -> Options {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_write_buffer_size(self.write_buffer_size);
        opts.set_max_open_files(self.max_open_files);
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);

        let mut block_opts = rocksdb::BlockBasedOptions::default();
        let cache = rocksdb::Cache::new_lru_cache(self.block_cache_size);
        block_opts.set_block_cache(&cache);
        if self.bloom_filter_bits > 0 {
            block_opts.set_bloom_filter(f64::from(self.bloom_filter_bits), false);
        }
        opts.set_block_based_table_factory(&block_opts);
        opts
    }
}

/// An on-disk RocksDB-backed [`Database`] (04 §2.1).
pub struct RocksDb {
    db: DB,
    closed: AtomicBool,
    /// Held so a temp directory created by [`RocksDb::open_temp`] outlives the
    /// DB (cleaned up on drop). `None` for a caller-supplied path.
    _tempdir: Option<TempDir>,
}

fn to_other<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(anyhow::anyhow!("{e}"))
}

impl RocksDb {
    /// Opens (creating if absent) a RocksDB at `path` with default config.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_with_config(path, &RocksDbConfig::default())
    }

    /// Opens a RocksDB at `path` with an explicit [`RocksDbConfig`].
    pub fn open_with_config<P: AsRef<Path>>(path: P, config: &RocksDbConfig) -> Result<Self> {
        let opts = config.to_options();
        let db = DB::open(&opts, path).map_err(to_other)?;
        Ok(Self {
            db,
            closed: AtomicBool::new(false),
            _tempdir: None,
        })
    }

    /// Opens a RocksDB under a fresh temporary directory owned by the returned
    /// DB (cleaned up on drop). Used by the conformance battery (02 §7.2).
    pub fn open_temp() -> Result<Self> {
        let dir = tempfile::tempdir().map_err(to_other)?;
        let opts = RocksDbConfig::default().to_options();
        let db = DB::open(&opts, dir.path()).map_err(to_other)?;
        Ok(Self {
            db,
            closed: AtomicBool::new(false),
            _tempdir: Some(dir),
        })
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Collects, from a point-in-time snapshot, the `(key, value)` pairs that are
    /// `≥ start` AND have `prefix`, in ascending key order. Mirrors the Go
    /// `start ≥` + `HasPrefix` predicate; the owned `Vec` makes the iterator
    /// independent of later mutation (`TestIteratorSnapshot`).
    fn snapshot_range(&self, start: &[u8], prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let snapshot = self.db.snapshot();
        let mut read_opts = ReadOptions::default();
        read_opts.set_snapshot(&snapshot);

        // Seek to max(start, prefix) — both are lower bounds.
        let lower: &[u8] = if start > prefix { start } else { prefix };
        let mode = if lower.is_empty() {
            IteratorMode::Start
        } else {
            IteratorMode::From(lower, rocksdb::Direction::Forward)
        };

        let mut out = Vec::new();
        let iter = self.db.iterator_opt(mode, read_opts);
        for item in iter {
            let Ok((k, v)) = item else { break };
            if !k.starts_with(prefix) {
                // Past the contiguous prefix block (keys are sorted).
                break;
            }
            if k.as_ref() < start {
                continue;
            }
            out.push((k.to_vec(), v.to_vec()));
        }
        out
    }
}

impl KeyValueReader for RocksDb {
    fn has(&self, key: &[u8]) -> Result<bool> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        // Zero-copy presence check.
        Ok(self.db.get_pinned(key).map_err(to_other)?.is_some())
    }

    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        match self.db.get(key).map_err(to_other)? {
            Some(v) => Ok(v),
            None => Err(Error::NotFound),
        }
    }
}

impl KeyValueWriter for RocksDb {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        self.db.put(key, value).map_err(to_other)
    }
}

impl KeyValueDeleter for RocksDb {
    fn delete(&self, key: &[u8]) -> Result<()> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        self.db.delete(key).map_err(to_other)
    }
}

impl Compacter for RocksDb {
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        self.db.compact_range(start, limit);
        Ok(())
    }
}

impl Batcher for RocksDb {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        Box::new(RocksBatch {
            db: self,
            ops: BatchOps::new(),
        })
    }
}

impl Iteratee for RocksDb {
    type Iter<'a> = RocksIterator<'a>;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> RocksIterator<'_> {
        if self.is_closed() {
            return RocksIterator {
                db: self,
                entries: Vec::new(),
                pos: None,
                err: Some(Error::Closed),
            };
        }
        RocksIterator {
            db: self,
            entries: self.snapshot_range(start, prefix),
            pos: None,
            err: None,
        }
    }
}

impl Database for RocksDb {
    fn close(&self) -> Result<()> {
        // Compare-and-set so a second close returns ErrClosed (Go parity).
        if self
            .closed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::Closed);
        }
        // The underlying RocksDB handle is freed on drop; flagging closed is
        // sufficient for the post-close ErrClosed contract.
        Ok(())
    }

    fn health_check(&self) -> Result<serde_json::Value> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let live = self
            .db
            .property_int_value("rocksdb.estimate-live-data-size")
            .map_err(to_other)?
            .unwrap_or(0);
        Ok(serde_json::json!({ "estimate-live-data-size": live }))
    }
}

impl DynDatabase for RocksDb {
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

/// A write-only batch over a [`RocksDb`]; buffers ops, then writes them as one
/// atomic `WriteBatch` on [`Batch::write`].
struct RocksBatch<'a> {
    db: &'a RocksDb,
    ops: BatchOps,
}

impl WriteDelete for RocksBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.ops.put(key, value);
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.ops.delete(key);
        Ok(())
    }
}

impl Batch for RocksBatch<'_> {
    fn size(&self) -> usize {
        self.ops.size()
    }

    fn write(&mut self) -> Result<()> {
        if self.db.is_closed() {
            return Err(Error::Closed);
        }
        let mut wb = WriteBatch::default();
        for op in &self.ops.ops {
            if op.delete {
                wb.delete(&op.key);
            } else {
                wb.put(&op.key, &op.value);
            }
        }
        self.db.db.write(wb).map_err(to_other)
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

/// A point-in-time snapshot cursor over a [`RocksDb`] (`TestIteratorSnapshot`).
/// Holds an owned `Vec` snapshot, so it is independent of later mutation. It
/// re-checks the DB's closed state on each [`Iterator::next`] (matching Go,
/// which reports `ErrClosed` once the DB closes).
pub struct RocksIterator<'a> {
    db: &'a RocksDb,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
    /// `None` before the first `next`; `Some(i)` for the current index.
    pos: Option<usize>,
    err: Option<Error>,
}

impl Iterator for RocksIterator<'_> {
    fn next(&mut self) -> bool {
        if self.err.is_some() {
            self.entries.clear();
            self.pos = None;
            return false;
        }
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
