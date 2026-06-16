// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.18 offline arm 1 — load-generator determinism + integer rate pacing
//! (specs/02 §10.3). Pure Rust, runs every CI run (no feature, not `#[ignore]`).

#![allow(unused_crate_dependencies)]

use std::time::Duration;

use ava_load::generator::{LoadGenerator, PacingSchedule, TxKind};
use pretty_assertions::assert_eq;

/// Same seed ⇒ byte-identical descriptor stream; a different seed differs.
#[test]
fn stream_is_deterministic_in_the_seed() {
    let mut a = LoadGenerator::new(0xA11CE, 8);
    let mut b = LoadGenerator::new(0xA11CE, 8);

    let stream_a = a.take(64);
    let stream_b = b.take(64);
    assert_eq!(
        stream_a, stream_b,
        "LoadGenerator is a deterministic function of (seed, accounts)"
    );

    // Byte-exact: encoded descriptor bytes match too.
    let bytes_a: Vec<u8> = stream_a.iter().flat_map(|d| d.encode()).collect();
    let bytes_b: Vec<u8> = stream_b.iter().flat_map(|d| d.encode()).collect();
    assert_eq!(
        bytes_a, bytes_b,
        "encoded descriptor bytes are reproducible"
    );

    // A different seed produces a different stream (overwhelmingly likely; the
    // splitmix64 mix guarantees it for these seeds).
    let mut other = LoadGenerator::new(0xB0B, 8);
    let stream_other = other.take(64);
    assert_ne!(
        stream_a, stream_other,
        "a different seed derives a different stream"
    );
}

/// `descriptor_at` is a pure function of the index (cursor-free), matching the
/// cursor-advancing `next_descriptor`.
#[test]
fn descriptor_at_matches_sequential_pull() {
    let generator = LoadGenerator::new(0x5EED, 6);
    let mut cursor = LoadGenerator::new(0x5EED, 6);
    for i in 0..50 {
        assert_eq!(
            generator.descriptor_at(i),
            cursor.next_descriptor(),
            "descriptor_at(i) equals the i-th sequential pull"
        );
    }
}

/// Every descriptor targets a valid, *distinct* sender/recipient and the kind
/// follows the fixed round-robin cycle.
#[test]
fn descriptors_are_well_formed() {
    let generator = LoadGenerator::new(0xDEED, 5);
    let cycle = [TxKind::C, TxKind::X, TxKind::C, TxKind::P];
    for i in 0..200u64 {
        let d = generator.descriptor_at(i);
        assert!(d.from < 5, "from in range");
        assert!(d.to < 5, "to in range");
        assert_ne!(
            d.from, d.to,
            "sender and recipient are distinct (index {i})"
        );
        assert_eq!(d.index, i, "index is the stream position");
        assert_eq!(d.nonce, i, "nonce is the stream position");
        let expected = cycle
            .get(usize::try_from(i).unwrap_or(0) % cycle.len())
            .copied()
            .expect("cycle index in range");
        assert_eq!(
            d.kind, expected,
            "kind follows the deterministic round-robin cycle"
        );
        // The stream mixes all three chains.
    }
    let kinds: std::collections::BTreeSet<TxKind> =
        (0..4).map(|i| generator.descriptor_at(i).kind).collect();
    assert_eq!(
        kinds.len(),
        3,
        "the stream mixes C-, X- and P-chain transfers"
    );
}

/// A 2-account generator never produces a self-transfer (the `to`-offset edge
/// case where `accounts - 1 == 1`).
#[test]
fn two_account_stream_never_self_transfers() {
    let generator = LoadGenerator::new(0x2, 2);
    for i in 0..100 {
        let d = generator.descriptor_at(i);
        assert_ne!(
            d.from, d.to,
            "2-account stream never self-transfers (index {i})"
        );
    }
}

/// Integer rate pacing: `total_count = floor(rate * duration)`, the interval is
/// `1s / rate`, and deadlines are monotonic and capped at the run duration.
#[test]
fn pacing_math_is_exact_and_checked() {
    // 100 tx/s for 30s => 3000 tx.
    let sched = PacingSchedule::new(100, Duration::from_secs(30));
    assert_eq!(sched.total_count(), 3_000, "100 tps * 30s = 3000 tx");
    assert_eq!(
        sched.interval(),
        Some(Duration::from_millis(10)),
        "1s / 100 = 10ms spacing"
    );

    // Fractional-second duration uses millisecond precision.
    let sched = PacingSchedule::new(50, Duration::from_millis(2_500));
    assert_eq!(sched.total_count(), 125, "50 tps * 2.5s = 125 tx");

    // Zero rate => no pacing, no descriptors.
    let idle = PacingSchedule::new(0, Duration::from_secs(10));
    assert_eq!(idle.total_count(), 0, "zero rate emits nothing");
    assert_eq!(idle.interval(), None, "zero rate has no interval");
    assert_eq!(
        idle.deadline_of(5),
        Duration::ZERO,
        "zero rate deadline is the run start"
    );

    // Deadlines are monotonic and saturate at the run duration.
    let sched = PacingSchedule::new(10, Duration::from_secs(1));
    let d0 = sched.deadline_of(0);
    let d5 = sched.deadline_of(5);
    let d_far = sched.deadline_of(1_000_000);
    assert!(d0 <= d5, "deadlines are monotonic");
    assert_eq!(
        d_far,
        Duration::from_secs(1),
        "a deadline past the run end saturates at the duration"
    );
}

/// A hostile `(rate, duration)` saturates instead of panicking (no overflow,
/// no divide-by-zero).
#[test]
fn pacing_math_never_panics_on_extremes() {
    let huge = PacingSchedule::new(u32::MAX, Duration::from_secs(u64::from(u32::MAX)));
    // total_count must not panic; it saturates.
    let _ = huge.total_count();
    let _ = huge.interval();
    let _ = huge.deadline_of(u64::MAX);

    let zero_dur = PacingSchedule::new(1_000, Duration::ZERO);
    assert_eq!(zero_dur.total_count(), 0, "zero duration emits nothing");
}
