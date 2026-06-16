// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Uptime [`UptimeState`] — the persistence surface the uptime [`super::manager`]
//! reads/writes through (port of `snow/uptime/state.go`).
//!
//! Go's `snow/uptime` ships only the `State` *interface* plus a `TestState`; the
//! concrete DB-backed store lives in `vms/platformvm`. We mirror that split:
//!
//! - [`UptimeState`] is the trait (`GetUptime`/`SetUptime`/`GetStartTime`),
//!   returning [`Error::NotFound`](ava_database::Error::NotFound) for nodes that
//!   are not current validators (Go `database.ErrNotFound`).
//! - [`MemUptimeState`] is the in-memory port of Go `TestState`, used by tests
//!   and as a non-persistent backing store. It is `Clone` + interior-mutable so a
//!   test can hold a handle while a manager mutates it (Go passes `*TestState`).
//! - [`DbUptimeState`] persists through an `Arc<dyn DynDatabase>`.
//!
//! # Time representation
//! All instants are floored to whole seconds before storage (Go
//! `time.Unix(t.Unix(), 0)`), matching the `SetUptime` invariant that
//! `lastUpdated` is truncated to the nearest second.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_database::{DynDatabase, Error as DbError, Result as DbResult};
use ava_types::node_id::NodeId;

/// Floor a [`SystemTime`] to whole seconds since the Unix epoch (Go
/// `time.Unix(t.Unix(), 0)`). Pre-epoch instants clamp to the epoch, matching the
/// clock's `unix()` clamp.
#[must_use]
pub fn floor_to_secs(t: SystemTime) -> SystemTime {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // `secs` is derived from a since-epoch duration, so re-adding it cannot
    // overflow in practice; fall back to the original instant if it ever did.
    UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap_or(t)
}

/// The persistence surface used by the uptime manager (Go `uptime.State`).
///
/// Every method returns [`DbError::NotFound`] for a `node_id` that is not a
/// current validator — the manager treats that as "don't track this node".
pub trait UptimeState: Send + Sync {
    /// Returns the accrued `up_duration` and `last_updated` instant of `node_id`.
    ///
    /// # Errors
    /// [`DbError::NotFound`] when `node_id` is not a current validator; any
    /// backend failure otherwise.
    fn get_uptime(&self, node_id: NodeId) -> DbResult<(Duration, SystemTime)>;

    /// Updates `up_duration` and `last_updated` of `node_id`.
    ///
    /// Invariant: `last_updated` is expected to be floored to the nearest second.
    ///
    /// # Errors
    /// [`DbError::NotFound`] when `node_id` is not a current validator; any
    /// backend failure otherwise.
    fn set_uptime(
        &self,
        node_id: NodeId,
        up_duration: Duration,
        last_updated: SystemTime,
    ) -> DbResult<()>;

    /// Returns the instant `node_id` started validating.
    ///
    /// # Errors
    /// [`DbError::NotFound`] when `node_id` is not a current validator; any
    /// backend failure otherwise.
    fn get_start_time(&self, node_id: NodeId) -> DbResult<SystemTime>;
}

/// A node's persisted uptime record.
#[derive(Clone, Copy, Debug)]
struct Record {
    up_duration: Duration,
    last_updated: SystemTime,
    start_time: SystemTime,
}

/// In-memory [`UptimeState`] — the port of Go `uptime.TestState`.
///
/// Clonable handles share one backing map (Go shares a `*TestState` pointer), so
/// a test can observe writes a manager makes. Optional injected read/write errors
/// mirror `TestState.dbReadError` / `dbWriteError` for failure-path tests.
#[derive(Clone, Default)]
pub struct MemUptimeState {
    inner: Arc<Mutex<MemInner>>,
}

#[derive(Default)]
struct MemInner {
    nodes: BTreeMap<NodeId, Record>,
    read_error: Option<InjectedError>,
    write_error: Option<InjectedError>,
}

/// A cloneable injected failure for the `MemUptimeState` failure-path tests
/// (Go `dbReadError` / `dbWriteError`). Only the sentinels are reproducible
/// without `anyhow`, which the realistic DB failure modes cover.
#[derive(Clone, Copy)]
pub enum InjectedError {
    /// Maps to [`DbError::Closed`].
    Closed,
    /// Maps to [`DbError::NotFound`].
    NotFound,
}

impl From<InjectedError> for DbError {
    fn from(e: InjectedError) -> Self {
        match e {
            InjectedError::Closed => DbError::Closed,
            InjectedError::NotFound => DbError::NotFound,
        }
    }
}

impl MemUptimeState {
    /// Creates an empty in-memory uptime state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `node_id` as a current validator starting at `start_time`
    /// (Go `TestState.AddNode`). `start_time` is floored to whole seconds.
    pub fn add_node(&self, node_id: NodeId, start_time: SystemTime) {
        let st = floor_to_secs(start_time);
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        g.nodes.insert(
            node_id,
            Record {
                up_duration: Duration::ZERO,
                last_updated: st,
                start_time: st,
            },
        );
    }

    /// Injects a read error returned by subsequent `get_*` calls (Go
    /// `dbReadError`). `None` clears it.
    pub fn set_read_error(&self, err: Option<InjectedError>) {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        g.read_error = err;
    }

    /// Injects a write error returned by subsequent `set_uptime` calls (Go
    /// `dbWriteError`). `None` clears it.
    pub fn set_write_error(&self, err: Option<InjectedError>) {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        g.write_error = err;
    }
}

impl UptimeState for MemUptimeState {
    fn get_uptime(&self, node_id: NodeId) -> DbResult<(Duration, SystemTime)> {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let rec = g.nodes.get(&node_id).ok_or(DbError::NotFound)?;
        let value = (rec.up_duration, rec.last_updated);
        match g.read_error {
            Some(e) => Err(e.into()),
            None => Ok(value),
        }
    }

    fn set_uptime(
        &self,
        node_id: NodeId,
        up_duration: Duration,
        last_updated: SystemTime,
    ) -> DbResult<()> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let write_error = g.write_error;
        let Some(rec) = g.nodes.get_mut(&node_id) else {
            return Err(DbError::NotFound);
        };
        rec.up_duration = up_duration;
        rec.last_updated = floor_to_secs(last_updated);
        match write_error {
            Some(e) => Err(e.into()),
            None => Ok(()),
        }
    }

    fn get_start_time(&self, node_id: NodeId) -> DbResult<SystemTime> {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let rec = g.nodes.get(&node_id).ok_or(DbError::NotFound)?;
        let value = rec.start_time;
        match g.read_error {
            Some(e) => Err(e.into()),
            None => Ok(value),
        }
    }
}

/// `DynDatabase`-backed [`UptimeState`].
///
/// Layout: key = the 20-byte `node_id`; value = three big-endian `u64`s
/// (`up_duration_secs`, `last_updated_secs`, `start_time_secs`). A node must be
/// registered with [`DbUptimeState::add_node`] before tracking; an unregistered
/// key reads back as [`DbError::NotFound`], matching the validator contract.
pub struct DbUptimeState {
    db: Arc<dyn DynDatabase>,
}

/// Encoded record width: three big-endian `u64` seconds fields.
const RECORD_LEN: usize = 24;

impl DbUptimeState {
    /// Wraps `db` as an uptime store.
    #[must_use]
    pub fn new(db: Arc<dyn DynDatabase>) -> Self {
        Self { db }
    }

    /// Registers `node_id` as a current validator starting at `start_time`,
    /// persisting a zeroed uptime record (`start_time` floored to whole seconds).
    ///
    /// # Errors
    /// Propagates any backend write failure.
    pub fn add_node(&self, node_id: NodeId, start_time: SystemTime) -> DbResult<()> {
        let st = secs_since_epoch(floor_to_secs(start_time));
        self.write(node_id, 0, st, st)
    }

    fn write(
        &self,
        node_id: NodeId,
        up_secs: u64,
        last_secs: u64,
        start_secs: u64,
    ) -> DbResult<()> {
        let mut value = [0u8; RECORD_LEN];
        value[0..8].copy_from_slice(&up_secs.to_be_bytes());
        value[8..16].copy_from_slice(&last_secs.to_be_bytes());
        value[16..24].copy_from_slice(&start_secs.to_be_bytes());
        self.db.put(node_id.as_bytes(), &value)
    }

    fn read(&self, node_id: NodeId) -> DbResult<[u64; 3]> {
        let raw = self.db.get(node_id.as_bytes())?;
        if raw.len() != RECORD_LEN {
            // A record we did not write (or external corruption). Treat as
            // absent — uptime is off the determinism-critical path, so a
            // best-effort "not a tracked validator" is the safe degradation.
            return Err(DbError::NotFound);
        }
        let mut fields = [0u64; 3];
        for (i, field) in fields.iter_mut().enumerate() {
            let lo = i.saturating_mul(8);
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&raw[lo..lo.saturating_add(8)]);
            *field = u64::from_be_bytes(buf);
        }
        Ok(fields)
    }
}

impl UptimeState for DbUptimeState {
    fn get_uptime(&self, node_id: NodeId) -> DbResult<(Duration, SystemTime)> {
        let [up, last, _start] = self.read(node_id)?;
        Ok((
            Duration::from_secs(up),
            // Persisted second-count from a prior since-epoch duration; cannot
            // overflow in practice, clamp to the epoch if it ever did.
            UNIX_EPOCH
                .checked_add(Duration::from_secs(last))
                .unwrap_or(UNIX_EPOCH),
        ))
    }

    fn set_uptime(
        &self,
        node_id: NodeId,
        up_duration: Duration,
        last_updated: SystemTime,
    ) -> DbResult<()> {
        // Preserve the existing start time; NotFound for unregistered nodes.
        let [_up, _last, start] = self.read(node_id)?;
        self.write(
            node_id,
            up_duration.as_secs(),
            secs_since_epoch(floor_to_secs(last_updated)),
            start,
        )
    }

    fn get_start_time(&self, node_id: NodeId) -> DbResult<SystemTime> {
        let [_up, _last, start] = self.read(node_id)?;
        // Persisted second-count from a prior since-epoch duration; cannot
        // overflow in practice, clamp to the epoch if it ever did.
        Ok(UNIX_EPOCH
            .checked_add(Duration::from_secs(start))
            .unwrap_or(UNIX_EPOCH))
    }
}

/// Whole seconds since the Unix epoch (input already floored), clamped to 0.
fn secs_since_epoch(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
