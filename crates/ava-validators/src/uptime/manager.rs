// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Uptime manager + calculator (port of `snow/uptime/manager.go`).
//!
//! [`UptimeManager`] tracks per-node connectivity and, reading time through an
//! injected [`Clock`], accrues each node's online duration into an
//! [`UptimeState`]. It is **off** the determinism-critical path: uptime feeds
//! reward calculations, not block decisions (`specs/06` §6.3).
//!
//! The [`Tracker`] half (`start_tracking`/`stop_tracking`/`connect`/`disconnect`)
//! mutates connection bookkeeping (`&mut self`); the [`Calculator`] half
//! (`calculate_uptime`/`calculate_uptime_percent*`) is read-only (`&self`) so it
//! can be shared behind the [`super::locked::LockedCalculator`] adapter.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ava_database::Error as DbError;
use ava_types::node_id::NodeId;
use ava_utils::clock::Clock;

use super::error::{Error, Result};
use super::state::{UptimeState, floor_to_secs};

/// Read-only uptime queries (Go `uptime.Calculator`).
pub trait Calculator: Send + Sync {
    /// Returns the node's accrued uptime and the instant it was computed at
    /// (Go `CalculateUptime`).
    ///
    /// # Errors
    /// Surfaces a [`DbError::NotFound`](ava_database::Error::NotFound) (wrapped in
    /// [`Error::Database`]) for non-validators, or any backend failure.
    fn calculate_uptime(&self, node_id: NodeId) -> Result<(Duration, SystemTime)>;

    /// Returns the node's uptime as a fraction of `[start_time, now]`, where
    /// `start_time` is read from the state (Go `CalculateUptimePercent`).
    ///
    /// # Errors
    /// As [`Calculator::calculate_uptime`].
    fn calculate_uptime_percent(&self, node_id: NodeId) -> Result<f64>;

    /// As [`Calculator::calculate_uptime_percent`] but from an explicit
    /// `start_time` (expected floored to the nearest second). Go
    /// `CalculateUptimePercentFrom`.
    ///
    /// # Errors
    /// As [`Calculator::calculate_uptime`].
    fn calculate_uptime_percent_from(&self, node_id: NodeId, start_time: SystemTime)
    -> Result<f64>;
}

/// The uptime manager: tracker + calculator over a [`UptimeState`] and an
/// injected [`Clock`] (Go `uptime.manager`).
pub struct UptimeManager<S: UptimeState> {
    clock: Arc<dyn Clock>,
    state: S,
    /// node_id -> connected-at instant (Go `connections`).
    connections: BTreeMap<NodeId, SystemTime>,
    /// Whether tracking has begun; gates uptime writes (Go `startedTracking`).
    started_tracking: bool,
}

impl<S: UptimeState> UptimeManager<S> {
    /// Creates a manager over `state`, reading time through `clock`
    /// (Go `NewManager`).
    pub fn new(state: S, clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            state,
            connections: BTreeMap::new(),
            started_tracking: false,
        }
    }

    /// Whether tracking has started (Go `StartedTracking`).
    #[must_use]
    pub fn started_tracking(&self) -> bool {
        self.started_tracking
    }

    /// Begins tracking `node_ids`, flushing each node's pre-tracking uptime to
    /// the state (Go `StartTracking`).
    ///
    /// # Errors
    /// [`Error::AlreadyStartedTracking`] if already tracking; any backend failure.
    pub fn start_tracking(&mut self, node_ids: &[NodeId]) -> Result<()> {
        if self.started_tracking {
            return Err(Error::AlreadyStartedTracking);
        }
        for &node_id in node_ids {
            self.update_uptime(node_id)?;
        }
        self.started_tracking = true;
        Ok(())
    }

    /// Stops tracking `node_ids`, flushing each node's accrued uptime
    /// (Go `StopTracking`).
    ///
    /// # Errors
    /// [`Error::NotStartedTracking`] if not tracking; any backend failure.
    pub fn stop_tracking(&mut self, node_ids: &[NodeId]) -> Result<()> {
        if !self.started_tracking {
            return Err(Error::NotStartedTracking);
        }
        for &node_id in node_ids {
            self.update_uptime(node_id)?;
        }
        self.started_tracking = false;
        Ok(())
    }

    /// Records that `node_id` connected now (Go `Connect`).
    ///
    /// # Errors
    /// Infallible today (matches Go's `error` return for signature parity).
    pub fn connect(&mut self, node_id: NodeId) -> Result<()> {
        self.connections.insert(node_id, self.clock.unix_time());
        Ok(())
    }

    /// Whether `node_id` is currently connected (Go `IsConnected`).
    #[must_use]
    pub fn is_connected(&self, node_id: NodeId) -> bool {
        self.connections.contains_key(&node_id)
    }

    /// Records that `node_id` disconnected, flushing its accrued uptime if we are
    /// tracking (Go `Disconnect`).
    ///
    /// # Errors
    /// Any backend failure while flushing.
    pub fn disconnect(&mut self, node_id: NodeId) -> Result<()> {
        let result = if self.started_tracking {
            self.update_uptime(node_id)
        } else {
            Ok(())
        };
        // Go uses `defer delete(...)`: the connection is dropped regardless of
        // the flush result.
        self.connections.remove(&node_id);
        result
    }

    /// Flushes `node_id`'s accrued uptime to the state (Go `updateUptime`). A
    /// non-validator (`NotFound`) is silently skipped.
    fn update_uptime(&self, node_id: NodeId) -> Result<()> {
        let (new_duration, new_last_updated) = match self.calculate_uptime(node_id) {
            Ok(v) => v,
            // We don't track the uptimes of non-validators.
            Err(Error::Database(DbError::NotFound)) => return Ok(()),
            Err(e) => return Err(e),
        };
        self.state
            .set_uptime(node_id, new_duration, new_last_updated)
            .map_err(Error::Database)
    }
}

impl<S: UptimeState> Calculator for UptimeManager<S> {
    fn calculate_uptime(&self, node_id: NodeId) -> Result<(Duration, SystemTime)> {
        let (up_duration, last_updated) =
            self.state.get_uptime(node_id).map_err(Error::Database)?;

        let now = self.clock.unix_time();

        // If time has gone backwards relative to the last update, don't double
        // count or delete any uptime.
        if now < last_updated {
            return Ok((up_duration, last_updated));
        }

        // If we haven't started tracking, assume the node was online since its
        // last update.
        if !self.started_tracking {
            let offline = now.duration_since(last_updated).unwrap_or(Duration::ZERO);
            return Ok((up_duration.saturating_add(offline), now));
        }

        // Tracking but not connected => offline since its last update.
        let Some(&time_connected) = self.connections.get(&node_id) else {
            return Ok((up_duration, now));
        };

        // Clamp the connect instant to the last update so no window is double
        // counted.
        let time_connected = if time_connected < last_updated {
            last_updated
        } else {
            time_connected
        };

        // Guard against time running backwards relative to the (clamped) connect.
        if now < time_connected {
            return Ok((up_duration, now));
        }

        let connected = now.duration_since(time_connected).unwrap_or(Duration::ZERO);
        Ok((up_duration.saturating_add(connected), now))
    }

    fn calculate_uptime_percent(&self, node_id: NodeId) -> Result<f64> {
        let start_time = self
            .state
            .get_start_time(node_id)
            .map_err(Error::Database)?;
        self.calculate_uptime_percent_from(node_id, start_time)
    }

    fn calculate_uptime_percent_from(
        &self,
        node_id: NodeId,
        start_time: SystemTime,
    ) -> Result<f64> {
        let (up_duration, now) = self.calculate_uptime(node_id)?;
        let best_possible = now
            .duration_since(floor_to_secs(start_time))
            .unwrap_or(Duration::ZERO);
        if best_possible.is_zero() {
            return Ok(1.0);
        }
        // Off the determinism path (rewards accounting), so float division
        // mirrors Go directly (`specs/06` §6.3).
        Ok(div_durations(up_duration, best_possible))
    }
}

/// `up / best` as an `f64`, mirroring Go's `float64(up) / float64(best)` over
/// nanosecond counts.
fn div_durations(up: Duration, best: Duration) -> f64 {
    let up_ns = duration_nanos_f64(up);
    let best_ns = duration_nanos_f64(best);
    if best_ns == 0.0 {
        return 1.0;
    }
    up_ns / best_ns
}

/// A [`Duration`] as nanoseconds in `f64` (saturating the seconds*1e9 term, which
/// only matters for absurd >292-year spans that never occur in practice).
#[allow(clippy::cast_precision_loss)]
fn duration_nanos_f64(d: Duration) -> f64 {
    d.as_secs() as f64 * 1.0e9 + f64::from(d.subsec_nanos())
}
