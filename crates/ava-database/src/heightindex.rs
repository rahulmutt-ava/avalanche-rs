// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `heightindexdb` — the [`HeightIndex`] trait and its memdb/meterdb backends
//! (04 §2.9), mirroring `database/heightindexdb/{memdb,meterdb}`.
//!
//! A deliberately simpler interface than [`Database`](crate::traits::Database):
//! a `u64`-height keyed store with `put`/`get`/`has`/`sync`/`close`, used to
//! index block bytes by height (e.g. proposervm). It carries its **own**
//! conformance battery (`dbtest::run_heightindex_suite`), the Rust port of
//! `database/heightindexdb/dbtest`.

use std::collections::HashMap;
use std::time::Instant;

use parking_lot::RwLock;
use prometheus::{CounterVec, GaugeVec, Opts, Registry};

use crate::error::{Error, Result};

/// A height-indexed value store (04 §2.9). Heights are `u64`; a missing height
/// yields [`Error::NotFound`]; post-`close` ops yield [`Error::Closed`].
pub trait HeightIndex: Send + Sync {
    /// Stores `value` at `height` (overwriting any existing entry).
    fn put(&self, height: u64, value: &[u8]) -> Result<()>;
    /// Returns the value at `height`, or [`Error::NotFound`] when absent.
    fn get(&self, height: u64) -> Result<Vec<u8>>;
    /// Returns whether `height` is present.
    fn has(&self, height: u64) -> Result<bool>;
    /// Durably persists `[start, end]` (a no-op for the in-memory backend).
    fn sync(&self, start: u64, end: u64) -> Result<()>;
    /// Closes the index; subsequent ops return [`Error::Closed`].
    fn close(&self) -> Result<()>;
}

/// The in-memory [`HeightIndex`] backend (`HashMap<u64, Vec<u8>>`), mirroring
/// `database/heightindexdb/memdb`. `None` ⇒ closed.
#[derive(Default)]
pub struct HeightIndexMemDb {
    data: RwLock<Option<HashMap<u64, Vec<u8>>>>,
}

impl HeightIndexMemDb {
    /// Creates an empty in-memory height index.
    pub fn new() -> Self {
        Self {
            data: RwLock::new(Some(HashMap::new())),
        }
    }
}

impl HeightIndex for HeightIndexMemDb {
    fn put(&self, height: u64, value: &[u8]) -> Result<()> {
        let mut guard = self.data.write();
        let map = guard.as_mut().ok_or(Error::Closed)?;
        map.insert(height, value.to_vec());
        Ok(())
    }

    fn get(&self, height: u64) -> Result<Vec<u8>> {
        let guard = self.data.read();
        let map = guard.as_ref().ok_or(Error::Closed)?;
        map.get(&height).cloned().ok_or(Error::NotFound)
    }

    fn has(&self, height: u64) -> Result<bool> {
        let guard = self.data.read();
        let map = guard.as_ref().ok_or(Error::Closed)?;
        Ok(map.contains_key(&height))
    }

    fn sync(&self, _start: u64, _end: u64) -> Result<()> {
        let guard = self.data.read();
        guard.as_ref().ok_or(Error::Closed)?;
        Ok(())
    }

    fn close(&self) -> Result<()> {
        let mut guard = self.data.write();
        if guard.is_none() {
            return Err(Error::Closed);
        }
        *guard = None;
        Ok(())
    }
}

// Method-label values, byte-exact with Go's `heightindexdb/meterdb` var block.
const METHOD_LABEL: &str = "method";
const PUT: &str = "put";
const GET: &str = "get";
const HAS: &str = "has";
const SYNC: &str = "sync";
const CLOSE: &str = "close";

/// The Prometheus-metering [`HeightIndex`] backend, mirroring
/// `database/heightindexdb/meterdb`. Registers `calls`/`duration`/`size`
/// vectors (each over the `method` label) under an optional `namespace`.
pub struct HeightIndexMeterDb<H: HeightIndex> {
    inner: H,
    calls: CounterVec,
    duration: GaugeVec,
    size: CounterVec,
}

fn to_other<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(anyhow::anyhow!("{e}"))
}

fn elapsed_ns(start: Instant) -> f64 {
    start.elapsed().as_nanos() as f64
}

impl<H: HeightIndex> HeightIndexMeterDb<H> {
    /// Wraps `inner`, registering metric vectors against `reg` under `namespace`.
    pub fn new(reg: &Registry, namespace: &str, inner: H) -> Result<Self> {
        let calls = CounterVec::new(
            Opts::new("calls", "number of calls to the database").namespace(namespace.to_string()),
            &[METHOD_LABEL],
        )
        .map_err(to_other)?;
        let duration = GaugeVec::new(
            Opts::new("duration", "time spent in database calls (ns)")
                .namespace(namespace.to_string()),
            &[METHOD_LABEL],
        )
        .map_err(to_other)?;
        let size = CounterVec::new(
            Opts::new("size", "size of data passed in database calls")
                .namespace(namespace.to_string()),
            &[METHOD_LABEL],
        )
        .map_err(to_other)?;

        reg.register(Box::new(calls.clone())).map_err(to_other)?;
        reg.register(Box::new(duration.clone())).map_err(to_other)?;
        reg.register(Box::new(size.clone())).map_err(to_other)?;

        Ok(Self {
            inner,
            calls,
            duration,
            size,
        })
    }

    fn observe(&self, method: &str, elapsed: f64, bytes: f64) {
        self.calls.with_label_values(&[method]).inc();
        self.duration.with_label_values(&[method]).add(elapsed);
        if bytes != 0.0 {
            self.size.with_label_values(&[method]).inc_by(bytes);
        }
    }

    fn observe_timed(&self, method: &str, elapsed: f64) {
        self.calls.with_label_values(&[method]).inc();
        self.duration.with_label_values(&[method]).add(elapsed);
    }
}

impl<H: HeightIndex> HeightIndex for HeightIndexMeterDb<H> {
    fn put(&self, height: u64, value: &[u8]) -> Result<()> {
        let start = Instant::now();
        let out = self.inner.put(height, value);
        self.observe(PUT, elapsed_ns(start), value.len() as f64);
        out
    }

    fn get(&self, height: u64) -> Result<Vec<u8>> {
        let start = Instant::now();
        let out = self.inner.get(height);
        let bytes = out.as_ref().map_or(0, Vec::len) as f64;
        self.observe(GET, elapsed_ns(start), bytes);
        out
    }

    fn has(&self, height: u64) -> Result<bool> {
        let start = Instant::now();
        let out = self.inner.has(height);
        self.observe_timed(HAS, elapsed_ns(start));
        out
    }

    fn sync(&self, start_height: u64, end_height: u64) -> Result<()> {
        let start = Instant::now();
        let out = self.inner.sync(start_height, end_height);
        self.observe_timed(SYNC, elapsed_ns(start));
        out
    }

    fn close(&self) -> Result<()> {
        let start = Instant::now();
        let out = self.inner.close();
        self.observe_timed(CLOSE, elapsed_ns(start));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memdb_put_get_has_close() {
        let db = HeightIndexMemDb::new();
        db.put(1, b"data").unwrap();
        assert_eq!(db.get(1).unwrap(), b"data");
        assert!(db.has(1).unwrap());
        assert!(matches!(db.get(2), Err(Error::NotFound)));
        assert!(!db.has(2).unwrap());
        db.sync(0, 10).unwrap();

        db.close().unwrap();
        assert!(matches!(db.put(1, b"x"), Err(Error::Closed)));
        assert!(matches!(db.get(1), Err(Error::Closed)));
        assert!(matches!(db.has(1), Err(Error::Closed)));
        assert!(matches!(db.sync(0, 1), Err(Error::Closed)));
        assert!(matches!(db.close(), Err(Error::Closed)));
    }
}
