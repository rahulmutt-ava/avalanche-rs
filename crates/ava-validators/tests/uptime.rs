// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the uptime manager/calculator (M3.8), porting
//! `snow/uptime/manager_test.go`. Time is driven by an injected
//! [`ava_utils::clock::MockClock`] (Go `mockable.Clock`); durations are
//! hand-computed under `clock.advance`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_database::Error as DbError;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, MockClock};
use ava_validators::uptime::{Calculator, LockedCalculator, MemUptimeState, UptimeManager};

fn node(b: u8) -> NodeId {
    NodeId::from_slice(&[b; 20]).unwrap()
}

/// A whole-second instant, since the Go state floors to the nearest second.
fn at_secs(s: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(s)
}

/// The headline test: uptime accumulates across connect/disconnect under a
/// virtual clock, matching a hand-computed expectation. (Go
/// `TestConnectAndDisconnect`.)
#[test]
fn uptime_accumulates_on_connect_disconnect() {
    let n0 = node(0);
    let start = at_secs(1_000);

    let state = MemUptimeState::new();
    state.add_node(n0, start);

    let clock = Arc::new(MockClock::at(start));
    let mut up = UptimeManager::new(state.clone(), clock.clone() as Arc<dyn Clock>);

    up.start_tracking(&[n0]).unwrap();

    // Not connected yet: zero duration, lastUpdated == now.
    let (dur, last) = up.calculate_uptime(n0).unwrap();
    assert_eq!(dur, Duration::ZERO);
    assert_eq!(last, clock.unix_time());

    up.connect(n0).unwrap();

    // +1s connected.
    clock.advance(Duration::from_secs(1));
    let (dur, last) = up.calculate_uptime(n0).unwrap();
    assert_eq!(dur, Duration::from_secs(1));
    assert_eq!(last, clock.unix_time());

    up.disconnect(n0).unwrap();

    // +1s while disconnected does NOT accumulate.
    clock.advance(Duration::from_secs(1));
    let (dur, last) = up.calculate_uptime(n0).unwrap();
    assert_eq!(dur, Duration::from_secs(1));
    assert_eq!(last, clock.unix_time());
}

/// Go `TestStartTracking`: starting tracking accrues offline time since the
/// last update as uptime (node assumed online before tracking started).
#[test]
fn start_tracking_accrues_offline_time() {
    let n0 = node(1);
    let start = at_secs(2_000);

    let state = MemUptimeState::new();
    state.add_node(n0, start);

    let clock = Arc::new(MockClock::at(start));
    let mut up = UptimeManager::new(state.clone(), clock.clone() as Arc<dyn Clock>);

    clock.advance(Duration::from_secs(1));
    up.start_tracking(&[n0]).unwrap();

    let (dur, last) = up.calculate_uptime(n0).unwrap();
    assert_eq!(dur, Duration::from_secs(1));
    assert_eq!(last, clock.unix_time());
}

/// Go `TestConnectAndDisconnectBeforeTracking`: connect + disconnect entirely
/// before tracking, then StartTracking accrues the offline window.
#[test]
fn connect_disconnect_before_tracking() {
    let n0 = node(2);
    let start = at_secs(3_000);

    let state = MemUptimeState::new();
    state.add_node(n0, start);

    let clock = Arc::new(MockClock::at(start));
    let mut up = UptimeManager::new(state.clone(), clock.clone() as Arc<dyn Clock>);

    clock.advance(Duration::from_secs(1));
    up.connect(n0).unwrap();
    clock.advance(Duration::from_secs(1));
    up.disconnect(n0).unwrap();

    up.start_tracking(&[n0]).unwrap();

    let (dur, last) = up.calculate_uptime(n0).unwrap();
    assert_eq!(dur, Duration::from_secs(2));
    assert_eq!(last, clock.unix_time());
}

/// Go `TestStopTrackingIncreasesUptime`: a stop persists accrued uptime that a
/// later StartTracking observes via the shared state.
#[test]
fn stop_tracking_persists_uptime() {
    let n0 = node(3);
    let start = at_secs(4_000);

    let state = MemUptimeState::new();
    state.add_node(n0, start);

    let clock = Arc::new(MockClock::at(start));
    let mut up = UptimeManager::new(state.clone(), clock.clone() as Arc<dyn Clock>);

    up.start_tracking(&[n0]).unwrap();
    up.connect(n0).unwrap();

    clock.advance(Duration::from_secs(1));
    up.stop_tracking(&[n0]).unwrap();

    // New manager over the same persisted state.
    let mut up = UptimeManager::new(state.clone(), clock.clone() as Arc<dyn Clock>);
    up.start_tracking(&[n0]).unwrap();

    let (dur, last) = up.calculate_uptime(n0).unwrap();
    assert_eq!(dur, Duration::from_secs(1));
    assert_eq!(last, clock.unix_time());
}

/// Go `TestUnrelatedNodeDisconnect`: an unrelated node's connect/disconnect does
/// not perturb n0's uptime.
#[test]
fn unrelated_node_disconnect_is_isolated() {
    let n0 = node(4);
    let n1 = node(5);
    let start = at_secs(5_000);

    let state = MemUptimeState::new();
    state.add_node(n0, start);

    let clock = Arc::new(MockClock::at(start));
    let mut up = UptimeManager::new(state.clone(), clock.clone() as Arc<dyn Clock>);

    up.start_tracking(&[n0]).unwrap();
    up.connect(n0).unwrap();
    up.connect(n1).unwrap();

    clock.advance(Duration::from_secs(1));
    let (dur, _) = up.calculate_uptime(n0).unwrap();
    assert_eq!(dur, Duration::from_secs(1));

    up.disconnect(n1).unwrap();

    clock.advance(Duration::from_secs(1));
    let (dur, _) = up.calculate_uptime(n0).unwrap();
    assert_eq!(dur, Duration::from_secs(2));
}

/// Go `TestCalculateUptimeNonValidator`: a non-validator yields NotFound.
#[test]
fn calculate_uptime_percent_non_validator_is_not_found() {
    let n0 = node(6);
    let start = at_secs(6_000);

    let state = MemUptimeState::new();
    let clock = Arc::new(MockClock::at(start));
    let up = UptimeManager::new(state, clock as Arc<dyn Clock>);

    let err = up.calculate_uptime_percent_from(n0, start).unwrap_err();
    assert!(matches!(err.as_db_error(), Some(DbError::NotFound)));
}

/// Go `TestCalculateUptimePercentageDivBy0`: zero best-possible window -> 1.0.
#[test]
fn calculate_uptime_percent_div_by_zero_is_one() {
    let n0 = node(7);
    let start = at_secs(7_000);

    let state = MemUptimeState::new();
    state.add_node(n0, start);

    let clock = Arc::new(MockClock::at(start));
    let up = UptimeManager::new(state, clock as Arc<dyn Clock>);

    let pct = up.calculate_uptime_percent_from(n0, start).unwrap();
    assert_eq!(pct, 1.0);
}

/// The `LockedCalculator` returns "still bootstrapping" until a calculator is
/// installed, then forwards.
#[tokio::test]
async fn locked_calculator_gates_then_forwards() {
    let n0 = node(8);
    let start = at_secs(8_000);

    let state = MemUptimeState::new();
    state.add_node(n0, start);

    let clock = Arc::new(MockClock::at(start));
    let mut inner = UptimeManager::new(state.clone(), clock.clone() as Arc<dyn Clock>);
    inner.start_tracking(&[n0]).unwrap();
    inner.connect(n0).unwrap();
    clock.advance(Duration::from_secs(1));

    let locked = LockedCalculator::new();

    // Before SetCalculator: bootstrapping.
    let err = locked.calculate_uptime(n0).await.unwrap_err();
    assert!(err.is_still_bootstrapping());

    locked.set_calculator(Arc::new(inner));

    let (dur, _) = locked.calculate_uptime(n0).await.unwrap();
    assert_eq!(dur, Duration::from_secs(1));
}
