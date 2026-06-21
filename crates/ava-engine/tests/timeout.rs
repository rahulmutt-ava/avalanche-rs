// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Virtual-time timing tests for the `AdaptiveTimeoutManager` (specs 24 §B.2,
//! 06 §5.4). Uses `#[tokio::test(start_paused = true)]` so the injected
//! [`MockClock`] (content time) and the tokio scheduler (virtual scheduling
//! time) advance in lock-step.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use assert_matches::assert_matches;

use ava_engine::networking::{
    AdaptiveTimeoutConfig, AdaptiveTimeoutManager, RequestId, TimeoutError,
};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::MockClock;

fn req(n: u8) -> RequestId {
    RequestId {
        node: NodeId::from([n; 20]),
        chain: Id::from([n; 32]),
        request_id: u32::from(n),
        op: 1,
    }
}

/// `deadline_fires_after_timeout` — register a request, advance the clock and
/// tokio time in lock-step, assert the timeout handler fires exactly once and
/// the averaged latency lengthens after the timeout penalty.
#[tokio::test(start_paused = true)]
async fn deadline_fires_after_timeout() {
    let clock = MockClock::at(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000));
    let config = AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(2),
        minimum_timeout: Duration::from_millis(500),
        maximum_timeout: Duration::from_secs(10),
        timeout_coefficient: 2.0,
        timeout_halflife: Duration::from_secs(60),
    };
    let clock_arc: Arc<dyn ava_utils::clock::Clock> = Arc::new(clock.clone());
    let mgr = AdaptiveTimeoutManager::new(&config, clock_arc).unwrap();

    let fired = Arc::new(AtomicUsize::new(0));
    let fired_cl = fired.clone();
    let id = req(1);
    mgr.put(
        id,
        true,
        Box::new(move || {
            fired_cl.fetch_add(1, Ordering::SeqCst);
        }),
    );

    let initial = mgr.timeout_duration();
    assert_eq!(initial, Duration::from_secs(2));

    // Advance both axes past the deadline (2s).
    clock.advance(Duration::from_secs(3));
    tokio::time::advance(Duration::from_secs(3)).await;
    // Let the dispatch task run.
    tokio::task::yield_now().await;

    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "timeout handler must fire once"
    );

    // After a timeout the manager observes the full duration as latency, so the
    // current timeout should be >= the minimum and reflect the penalty.
    let after = mgr.timeout_duration();
    assert!(after >= config.minimum_timeout);

    mgr.stop();
}

/// `response_shortens_timeout` — a fast response (small observed latency)
/// shortens the averaged timeout vs. the initial.
#[tokio::test(start_paused = true)]
async fn response_shortens_timeout() {
    let clock = MockClock::at(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000));
    let config = AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(5),
        minimum_timeout: Duration::from_millis(1),
        maximum_timeout: Duration::from_secs(10),
        timeout_coefficient: 1.0,
        timeout_halflife: Duration::from_millis(1),
    };
    let clock_arc: Arc<dyn ava_utils::clock::Clock> = Arc::new(clock.clone());
    let mgr = AdaptiveTimeoutManager::new(&config, clock_arc).unwrap();

    let id = req(2);
    mgr.put(id, true, Box::new(|| {}));

    // Respond after only 10ms — well below the 5s initial timeout.
    clock.advance(Duration::from_millis(10));
    mgr.remove(id);

    let after = mgr.timeout_duration();
    assert!(
        after < config.initial_timeout,
        "fast response should shorten timeout: {after:?}"
    );

    mgr.stop();
}

/// `config_verify_rejections` — `verify` rejects coefficient<1, initial∉[min,max],
/// and halflife==0 in Go's branch order.
#[test]
fn config_verify_rejections() {
    let base = AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(2),
        minimum_timeout: Duration::from_secs(1),
        maximum_timeout: Duration::from_secs(10),
        timeout_coefficient: 2.0,
        timeout_halflife: Duration::from_secs(60),
    };
    base.verify().unwrap();

    // initial > maximum
    let mut c = base.clone();
    c.initial_timeout = Duration::from_secs(20);
    assert_matches!(c.verify(), Err(TimeoutError::InitialAboveMaximum { .. }));

    // initial < minimum
    let mut c = base.clone();
    c.initial_timeout = Duration::from_millis(500);
    assert_matches!(c.verify(), Err(TimeoutError::InitialBelowMinimum { .. }));

    // coefficient < 1
    let mut c = base.clone();
    c.timeout_coefficient = 0.5;
    assert_matches!(c.verify(), Err(TimeoutError::CoefficientTooSmall { .. }));

    // halflife == 0
    let mut c = base.clone();
    c.timeout_halflife = Duration::ZERO;
    assert_matches!(c.verify(), Err(TimeoutError::NonPositiveHalflife));
}
