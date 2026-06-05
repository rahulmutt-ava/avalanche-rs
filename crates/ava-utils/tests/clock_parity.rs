// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use std::time::{Duration, UNIX_EPOCH};

use ava_utils::clock::{Clock, MAX_UNIX_SECS, MockClock};

#[test]
fn mock_clock_parity() {
    // MAX_UNIX_SECS matches Go mockable.MaxTime seconds.
    assert_eq!(MAX_UNIX_SECS, (1u64 << 63) - 62_135_596_801);

    let clock = MockClock::default();

    // `set` => faked.
    let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    clock.set(t);
    assert_eq!(clock.now(), t);
    assert_eq!(clock.unix(), 1_700_000_000);

    // `unix` clamps pre-epoch to 0.
    let pre = UNIX_EPOCH - Duration::from_secs(100);
    clock.set(pre);
    assert_eq!(clock.unix(), 0);

    // `unix_time` truncates sub-second.
    let frac = UNIX_EPOCH + Duration::from_millis(1_700_000_000_500);
    clock.set(frac);
    assert_eq!(clock.unix(), 1_700_000_000);
    assert_eq!(
        clock.unix_time(),
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    );

    // `advance` moves faked time forward by d.
    let base = UNIX_EPOCH + Duration::from_secs(1_000);
    clock.set(base);
    clock.advance(Duration::from_secs(60));
    assert_eq!(clock.now(), base + Duration::from_secs(60));
    assert_eq!(clock.unix(), 1_060);

    // `at` constructs already-faked.
    let c2 = MockClock::at(UNIX_EPOCH + Duration::from_secs(42));
    assert_eq!(c2.unix(), 42);

    // `since` saturates at zero and measures forward.
    let earlier = UNIX_EPOCH + Duration::from_secs(1_000);
    clock.set(UNIX_EPOCH + Duration::from_secs(1_100));
    assert_eq!(clock.since(earlier), Duration::from_secs(100));
    clock.set(UNIX_EPOCH + Duration::from_secs(900));
    assert_eq!(clock.since(earlier), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn mock_clock_monotonic_advance() {
    let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    let m0 = clock.monotonic();
    clock.advance(Duration::from_secs(5));
    let m1 = clock.monotonic();
    assert_eq!(m1.duration_since(m0), Duration::from_secs(5));
}
