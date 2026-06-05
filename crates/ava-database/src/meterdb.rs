// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `meterdb` — a Prometheus-metering wrapper over any [`Database`] (04 §2.5),
//! mirroring `database/meterdb/db.go`.
//!
//! Wraps any backend and, on every method, increments a `calls` counter, adds
//! the elapsed time (nanoseconds) to a `duration` gauge, and adds the byte size
//! to a `size` counter — each labelled by the `method` label whose value set is
//! byte-exact with Go (`has`, `get`, `put`, …, `batch_*`, `iterator_*`). The
//! metric names (`calls`/`duration`/`size`) and label values are guarded by a
//! Go-extracted golden (`tests/golden_meterdb_metrics.rs`).
//!
//! Keys are passthrough (04 §10.1): metering never rewrites key/value bytes, so
//! a `MeterDb<Inner>` is byte-for-byte equivalent to `Inner` on disk.

use std::time::Instant;

use prometheus::{CounterVec, GaugeVec, Opts, Registry};

use crate::error::{Error, Result};
use crate::traits::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, KeyValueDeleter,
    KeyValueReader, KeyValueWriter, WriteDelete,
};

/// The label name carried by every metered series (`meterdb.methodLabel`).
const METHOD_LABEL: &str = "method";

// Method-label values, byte-exact with Go's `database/meterdb` var block.
const HAS: &str = "has";
const GET: &str = "get";
const PUT: &str = "put";
const DELETE: &str = "delete";
const NEW_BATCH: &str = "new_batch";
const NEW_ITERATOR: &str = "new_iterator";
const COMPACT: &str = "compact";
const CLOSE: &str = "close";
const HEALTH_CHECK: &str = "health_check";
const BATCH_PUT: &str = "batch_put";
const BATCH_DELETE: &str = "batch_delete";
const BATCH_SIZE: &str = "batch_size";
const BATCH_WRITE: &str = "batch_write";
const BATCH_RESET: &str = "batch_reset";
const BATCH_REPLAY: &str = "batch_replay";
const BATCH_INNER: &str = "batch_inner";
const ITERATOR_NEXT: &str = "iterator_next";
const ITERATOR_ERROR: &str = "iterator_error";
const ITERATOR_KEY: &str = "iterator_key";
const ITERATOR_VALUE: &str = "iterator_value";
const ITERATOR_RELEASE: &str = "iterator_release";

/// The shared metric set (`calls`/`duration`/`size`), each a vector over the
/// `method` label. Cloned (cheap `Arc`-backed handles) into batches/iterators so
/// they meter against the same series as their parent DB.
#[derive(Clone)]
struct Metrics {
    /// Number of calls to the database (`calls`).
    calls: CounterVec,
    /// Time spent in database calls, in nanoseconds (`duration`).
    duration: GaugeVec,
    /// Size of data passed in database calls (`size`).
    size: CounterVec,
}

impl Metrics {
    /// Registers the three metric vectors against `reg` (matching Go's
    /// `errors.Join(reg.Register(...))`). The metric names are bare
    /// `calls`/`duration`/`size` to match `database/meterdb` exactly.
    fn new(reg: &Registry) -> Result<Self> {
        let calls = CounterVec::new(
            Opts::new("calls", "number of calls to the database"),
            &[METHOD_LABEL],
        )
        .map_err(to_other)?;
        let duration = GaugeVec::new(
            Opts::new("duration", "time spent in database calls (ns)"),
            &[METHOD_LABEL],
        )
        .map_err(to_other)?;
        let size = CounterVec::new(
            Opts::new("size", "size of data passed in database calls"),
            &[METHOD_LABEL],
        )
        .map_err(to_other)?;

        reg.register(Box::new(calls.clone())).map_err(to_other)?;
        reg.register(Box::new(duration.clone())).map_err(to_other)?;
        reg.register(Box::new(size.clone())).map_err(to_other)?;

        Ok(Self {
            calls,
            duration,
            size,
        })
    }

    /// Records a call to `method` taking `elapsed_nanos` and moving `bytes`.
    fn observe(&self, method: &str, elapsed_nanos: f64, bytes: f64) {
        self.calls.with_label_values(&[method]).inc();
        self.duration
            .with_label_values(&[method])
            .add(elapsed_nanos);
        if bytes != 0.0 {
            self.size.with_label_values(&[method]).inc_by(bytes);
        }
    }

    /// Records a call carrying no byte-size dimension (timers only).
    fn observe_timed(&self, method: &str, elapsed_nanos: f64) {
        self.calls.with_label_values(&[method]).inc();
        self.duration
            .with_label_values(&[method])
            .add(elapsed_nanos);
    }
}

/// Elapsed nanoseconds since `start`, saturating into an `f64` (Go casts a
/// `time.Duration` — an `i64` ns count — to `float64`).
fn elapsed_ns(start: Instant) -> f64 {
    // `as_nanos` is u128; saturate into f64 the same way Go's float cast does.
    start.elapsed().as_nanos() as f64
}

fn to_other<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(anyhow::anyhow!("{e}"))
}

/// A [`Database`] that meters every operation under the Go metric names (04 §2.5).
pub struct MeterDb<D: Database> {
    db: D,
    metrics: Metrics,
}

impl<D: Database> MeterDb<D> {
    /// Wraps `db`, registering the `calls`/`duration`/`size` metric vectors
    /// against `reg`. Errors with [`Error::Other`] if registration fails (e.g.
    /// a duplicate registration under the same registry).
    pub fn new(reg: &Registry, db: D) -> Result<Self> {
        Ok(Self {
            db,
            metrics: Metrics::new(reg)?,
        })
    }

    /// Returns a reference to the wrapped database.
    pub fn inner(&self) -> &D {
        &self.db
    }
}

impl<D: Database> KeyValueReader for MeterDb<D> {
    fn has(&self, key: &[u8]) -> Result<bool> {
        let start = Instant::now();
        let out = self.db.has(key);
        self.metrics
            .observe(HAS, elapsed_ns(start), key.len() as f64);
        out
    }

    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        let start = Instant::now();
        let out = self.db.get(key);
        let value_len = out.as_ref().map_or(0, Vec::len);
        let bytes = key.len().saturating_add(value_len) as f64;
        self.metrics.observe(GET, elapsed_ns(start), bytes);
        out
    }
}

impl<D: Database> KeyValueWriter for MeterDb<D> {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let start = Instant::now();
        let out = self.db.put(key, value);
        let bytes = key.len().saturating_add(value.len()) as f64;
        self.metrics.observe(PUT, elapsed_ns(start), bytes);
        out
    }
}

impl<D: Database> KeyValueDeleter for MeterDb<D> {
    fn delete(&self, key: &[u8]) -> Result<()> {
        let start = Instant::now();
        let out = self.db.delete(key);
        self.metrics
            .observe(DELETE, elapsed_ns(start), key.len() as f64);
        out
    }
}

impl<D: Database> Compacter for MeterDb<D> {
    fn compact(&self, start_key: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        let start = Instant::now();
        let out = self.db.compact(start_key, limit);
        self.metrics.observe_timed(COMPACT, elapsed_ns(start));
        out
    }
}

impl<D: Database> Batcher for MeterDb<D> {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        let start = Instant::now();
        let inner = self.db.new_batch();
        self.metrics.observe_timed(NEW_BATCH, elapsed_ns(start));
        Box::new(MeterBatch {
            inner,
            metrics: self.metrics.clone(),
        })
    }
}

impl<D: Database> Iteratee for MeterDb<D> {
    type Iter<'a>
        = MeterIterator<'a>
    where
        D: 'a;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> Self::Iter<'_> {
        let t = Instant::now();
        let inner: BoxIter<'_> =
            Box::new(self.db.new_iterator_with_start_and_prefix(start, prefix));
        self.metrics.observe_timed(NEW_ITERATOR, elapsed_ns(t));
        MeterIterator {
            inner,
            metrics: self.metrics.clone(),
        }
    }
}

impl<D: Database> Database for MeterDb<D> {
    fn close(&self) -> Result<()> {
        let start = Instant::now();
        let out = self.db.close();
        self.metrics.observe_timed(CLOSE, elapsed_ns(start));
        out
    }

    fn health_check(&self) -> Result<serde_json::Value> {
        let start = Instant::now();
        let out = self.db.health_check();
        self.metrics.observe_timed(HEALTH_CHECK, elapsed_ns(start));
        out
    }
}

impl<D: Database> DynDatabase for MeterDb<D> {
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

/// A metered batch wrapping the inner DB's batch (matching Go's `meterdb.batch`).
struct MeterBatch<'a> {
    inner: Box<dyn Batch + 'a>,
    metrics: Metrics,
}

impl WriteDelete for MeterBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let start = Instant::now();
        let out = self.inner.put(key, value);
        let bytes = key.len().saturating_add(value.len()) as f64;
        self.metrics.observe(BATCH_PUT, elapsed_ns(start), bytes);
        out
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        let start = Instant::now();
        let out = self.inner.delete(key);
        self.metrics
            .observe(BATCH_DELETE, elapsed_ns(start), key.len() as f64);
        out
    }
}

impl Batch for MeterBatch<'_> {
    fn size(&self) -> usize {
        let start = Instant::now();
        let size = self.inner.size();
        self.metrics.observe_timed(BATCH_SIZE, elapsed_ns(start));
        size
    }

    fn write(&mut self) -> Result<()> {
        let start = Instant::now();
        let out = self.inner.write();
        // Go records the post-write batch size against batch_write.
        let size = self.inner.size() as f64;
        self.metrics.observe(BATCH_WRITE, elapsed_ns(start), size);
        out
    }

    fn reset(&mut self) {
        let start = Instant::now();
        self.inner.reset();
        self.metrics.observe_timed(BATCH_RESET, elapsed_ns(start));
    }

    fn replay(&self, w: &mut dyn WriteDelete) -> Result<()> {
        let start = Instant::now();
        let out = self.inner.replay(w);
        self.metrics.observe_timed(BATCH_REPLAY, elapsed_ns(start));
        out
    }

    fn inner(&mut self) -> &mut dyn Batch {
        let start = Instant::now();
        self.metrics.observe_timed(BATCH_INNER, elapsed_ns(start));
        // Go returns `b.batch.Inner()`; we return self (the metered batch) so
        // the metered surface is preserved across `inner()` chaining, matching
        // how the conformance battery replays through `inner()`.
        self
    }
}

/// A metered iterator wrapping the inner DB's iterator (matching Go's
/// `meterdb.iterator`).
pub struct MeterIterator<'a> {
    inner: BoxIter<'a>,
    metrics: Metrics,
}

impl Iterator for MeterIterator<'_> {
    fn next(&mut self) -> bool {
        let start = Instant::now();
        let next = self.inner.next();
        let key_len = self.inner.key().map_or(0, <[u8]>::len);
        let value_len = self.inner.value().map_or(0, <[u8]>::len);
        let bytes = key_len.saturating_add(value_len) as f64;
        self.metrics
            .observe(ITERATOR_NEXT, elapsed_ns(start), bytes);
        next
    }

    fn error(&self) -> Result<()> {
        let start = Instant::now();
        let out = self.inner.error();
        self.metrics
            .observe_timed(ITERATOR_ERROR, elapsed_ns(start));
        out
    }

    fn key(&self) -> Option<&[u8]> {
        let start = Instant::now();
        let key = self.inner.key();
        self.metrics.observe_timed(ITERATOR_KEY, elapsed_ns(start));
        key
    }

    fn value(&self) -> Option<&[u8]> {
        let start = Instant::now();
        let value = self.inner.value();
        self.metrics
            .observe_timed(ITERATOR_VALUE, elapsed_ns(start));
        value
    }

    fn release(&mut self) {
        let start = Instant::now();
        self.inner.release();
        self.metrics
            .observe_timed(ITERATOR_RELEASE, elapsed_ns(start));
    }
}
