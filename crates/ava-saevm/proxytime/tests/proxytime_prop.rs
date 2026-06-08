// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Property and golden tests for `ava-saevm-proxytime`.
//!
//! Uses a concrete test `ProxyUnit` newtype `U(u64)` to exercise `Time<U>`.

use ava_saevm_proxytime::{ProxyUnit, Time};
use proptest::prelude::*;
use std::cmp::Ordering;

// ---------------------------------------------------------------------------
// Test ProxyUnit newtype
// ---------------------------------------------------------------------------

/// A minimal concrete `ProxyUnit` implementation for tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct U(u64);

impl From<U> for u128 {
    fn from(u: U) -> u128 {
        u128::from(u.0)
    }
}

impl From<u64> for U {
    fn from(v: u64) -> U {
        U(v)
    }
}

impl ProxyUnit for U {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A non-zero hertz value (proxy units per second).
fn hertz() -> impl Strategy<Value = U> {
    (1u64..=u64::MAX).prop_map(U)
}

/// A seconds/fraction pair compatible with a given hertz (fraction < hertz).
fn valid_time(hz: U) -> impl Strategy<Value = (u64, U)> {
    let denom = hz.0;
    (any::<u64>(), (0..denom).prop_map(U))
}

/// A valid `Time<U>` with unconstrained seconds and fraction.
fn arb_time() -> impl Strategy<Value = Time<U>> {
    hertz().prop_flat_map(|hz| valid_time(hz).prop_map(move |(sec, frac)| Time::new(sec, frac, hz)))
}

/// Two `Time<U>` values with the same hertz.
fn arb_two_times_same_rate() -> impl Strategy<Value = (Time<U>, Time<U>)> {
    hertz().prop_flat_map(|hz| {
        (valid_time(hz), valid_time(hz))
            .prop_map(move |((s1, f1), (s2, f2))| (Time::new(s1, f1, hz), Time::new(s2, f2, hz)))
    })
}

/// Two `Time<U>` values with potentially different hertz.
fn arb_two_times_diff_rate() -> impl Strategy<Value = (Time<U>, Time<U>)> {
    (hertz(), hertz()).prop_flat_map(|(hz1, hz2)| {
        (valid_time(hz1), valid_time(hz2))
            .prop_map(move |((s1, f1), (s2, f2))| (Time::new(s1, f1, hz1), Time::new(s2, f2, hz2)))
    })
}

// ---------------------------------------------------------------------------
// Golden tests
// ---------------------------------------------------------------------------

#[test]
fn golden_tick_basic() {
    // Start at 0 + 0/5; tick by 3 → 0 + 3/5.
    let mut t = Time::new(0, U(0), U(5));
    t.tick(U(3));
    let frac = t.fraction();
    assert_eq!(frac.numerator, U(3));
    assert_eq!(frac.denominator, U(5));
    assert_eq!(t.rate(), U(5));
}

#[test]
fn golden_tick_carry() {
    // Start at 0 + 3/5; tick by 3 → carries over to 1 + 1/5.
    let mut t = Time::new(0, U(3), U(5));
    t.tick(U(3));
    let frac = t.fraction();
    // 3 + 3 = 6; 6 / 5 = 1 rem 1 → second 1, fraction 1/5.
    assert_eq!(frac.numerator, U(1));
    assert_eq!(frac.denominator, U(5));
}

#[test]
fn golden_fast_forward_to_future() {
    let mut t = Time::new(10, U(2), U(10));
    // Advance to 12 + 5/10 (strictly in future).
    let (delta_secs, delta_frac) = t.fast_forward_to(12, U(5));
    // Advanced: 2 full seconds + 3/10.
    assert_eq!(delta_secs, 2);
    assert_eq!(delta_frac.numerator, U(3));
    assert_eq!(delta_frac.denominator, U(10));
    // Time is now at 12 + 5/10.
    let frac = t.fraction();
    assert_eq!(frac.numerator, U(5));
    assert_eq!(frac.denominator, U(10));
}

#[test]
fn golden_fast_forward_to_same_instant() {
    let mut t = Time::new(10, U(5), U(10));
    // Advance to same instant → returns 0 delta.
    let (delta_secs, delta_frac) = t.fast_forward_to(10, U(5));
    assert_eq!(delta_secs, 0);
    assert_eq!(delta_frac.numerator, U(0));
    // Time is unchanged.
    let frac = t.fraction();
    assert_eq!(frac.numerator, U(5));
}

#[test]
fn golden_fast_forward_to_past() {
    let mut t = Time::new(10, U(5), U(10));
    // Target is in the past → no-op.
    let (delta_secs, delta_frac) = t.fast_forward_to(9, U(5));
    assert_eq!(delta_secs, 0);
    assert_eq!(delta_frac.numerator, U(0));
    // Time unchanged.
    let frac = t.fraction();
    assert_eq!(frac.numerator, U(5));
}

#[test]
fn golden_set_rate_rescales() {
    // 3/6 = 1/2 second; rescale to hertz=10: ceil(3*10/6)=ceil(5)=5; so 5/10 = 1/2.
    let mut t = Time::new(0, U(3), U(6));
    t.set_rate(U(10));
    let frac = t.fraction();
    assert_eq!(frac.numerator, U(5));
    assert_eq!(frac.denominator, U(10));
}

#[test]
fn golden_set_rate_rounds_up() {
    // 1/3 second; rescale to hertz=2: ceil(1*2/3)=ceil(0.667)=1; so 1/2.
    let mut t = Time::new(0, U(1), U(3));
    t.set_rate(U(2));
    let frac = t.fraction();
    assert_eq!(frac.numerator, U(1));
    assert_eq!(frac.denominator, U(2));
}

#[test]
fn golden_compare_same_rate() {
    let t1 = Time::new(5, U(3), U(10));
    let t2 = Time::new(5, U(7), U(10));
    assert_eq!(t1.compare(&t2), Ordering::Less);
    assert_eq!(t2.compare(&t1), Ordering::Greater);
    assert_eq!(t1.compare(&t1.clone()), Ordering::Equal);
}

#[test]
fn golden_compare_diff_rates() {
    // t1 = 5 + 1/2; t2 = 5 + 2/4 (same fraction in different hertz).
    let t1 = Time::new(5, U(1), U(2));
    let t2 = Time::new(5, U(2), U(4));
    assert_eq!(t1.compare(&t2), Ordering::Equal);
}

#[test]
fn serialization_roundtrip() {
    let original = Time::new(12345, U(678), U(1000));
    let encoded = original.encode();
    let decoded = Time::<U>::decode(&encoded).expect("decode failed");
    assert_eq!(decoded.compare(&original), Ordering::Equal);
    assert_eq!(decoded.rate(), original.rate());
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    /// `tick` never makes time go backward (monotone property).
    #[test]
    fn tick_monotone(
        mut t in arb_time(),
        d in any::<u64>().prop_map(U),
    ) {
        let before = t.clone();
        t.tick(d);
        prop_assert!(
            t.compare(&before) != Ordering::Less,
            "tick made time go backward: before={before:?} d={d:?}"
        );
    }

    /// `fraction < hertz` invariant holds after any tick.
    #[test]
    fn fraction_invariant_after_tick(
        mut t in arb_time(),
        d in any::<u64>().prop_map(U),
    ) {
        t.tick(d);
        let frac = t.fraction();
        prop_assert!(
            frac.numerator < frac.denominator,
            "fraction invariant broken: {frac:?}"
        );
    }

    /// `fraction < hertz` invariant holds after set_rate.
    #[test]
    fn fraction_invariant_after_set_rate(
        mut t in arb_time(),
        new_hz in hertz(),
    ) {
        t.set_rate(new_hz);
        let frac = t.fraction();
        prop_assert!(
            frac.numerator < frac.denominator,
            "fraction invariant broken after set_rate: {frac:?}"
        );
    }

    /// After `set_rate`, the rescaled instant is >= the original (monotonicity,
    /// rounding-up semantics).
    #[test]
    fn set_rate_rounds_up_prop(
        mut t in arb_time(),
        new_hz in hertz(),
    ) {
        let before = t.clone();
        t.set_rate(new_hz);
        prop_assert!(
            t.compare(&before) != Ordering::Less,
            "set_rate moved time backward: before={before:?}"
        );
    }

    /// `fast_forward_to` a non-future target returns 0 and does not move.
    #[test]
    fn fast_forward_only_forward_no_advance(
        (t, target) in arb_two_times_same_rate(),
    ) {
        // Only apply when target is NOT strictly in the future of t.
        prop_assume!(target.compare(&t) != Ordering::Greater);
        let mut t_copy = t.clone();
        let target_frac = target.fraction();
        let (delta_secs, delta_frac) = t_copy.fast_forward_to(
            // We need to call fast_forward_to with (seconds, fraction) extracted.
            // To do that, we expose `unix_seconds` or compare the result.
            // Use helper to get seconds.
            target.unix_seconds(),
            target_frac.numerator,
        );
        prop_assert_eq!(delta_secs, 0, "expected no advance");
        prop_assert_eq!(delta_frac.numerator, U(0), "expected zero fraction delta");
        // Time must not have moved.
        prop_assert!(
            t_copy.compare(&t) == Ordering::Equal,
            "time moved when fast_forward_to was applied to non-future target"
        );
    }

    /// `fast_forward_to` a strictly future target moves forward.
    #[test]
    fn fast_forward_only_forward_advance(
        (t, target) in arb_two_times_same_rate(),
    ) {
        prop_assume!(target.compare(&t) == Ordering::Greater);
        let mut t_copy = t.clone();
        let target_frac = target.fraction();
        let (delta_secs, _delta_frac) = t_copy.fast_forward_to(
            target.unix_seconds(),
            target_frac.numerator,
        );
        // After forward, t_copy must equal target.
        prop_assert!(
            t_copy.compare(&target) == Ordering::Equal,
            "time did not advance to target"
        );
        // delta_secs must be > 0 OR (delta_secs == 0 and delta_frac > 0 means we
        // advanced sub-second). We only require that time moved forward, not that
        // delta_secs > 0 specifically.
        let _ = delta_secs;
    }

    /// `compare` against a u128 reference cross-multiply is consistent.
    ///
    /// This validates that `compare` across different hertz values matches a
    /// direct inline u128 cross-multiply reference.
    #[test]
    fn compare_consistent_across_rates(
        (t1, t2) in arb_two_times_diff_rate(),
    ) {
        let result = t1.compare(&t2);

        // Reference computation: compare seconds first, then fractions via u128.
        let s1 = t1.unix_seconds();
        let s2 = t2.unix_seconds();
        let expected = if s1 == s2 {
            let f1 = t1.fraction();
            let f2 = t2.fraction();
            // cross-multiply in u128
            let left = u128::from(f1.numerator).wrapping_mul(u128::from(f2.denominator));
            let right = u128::from(f2.numerator).wrapping_mul(u128::from(f1.denominator));
            left.cmp(&right)
        } else {
            s1.cmp(&s2)
        };

        prop_assert_eq!(result, expected);

        // Independent oracle: for the same-seconds case, `Time::compare` must
        // also agree with `cmputils::compare_fractions` (the u64 widening
        // helper), since our `U` ProxyUnit is u64-backed. This pins the
        // documented relationship to the inline implementation (specs/11 §2.1).
        if s1 == s2 {
            let f1 = t1.fraction();
            let f2 = t2.fraction();
            let oracle = ava_saevm_cmputils::compare_fractions(
                f1.numerator.0,
                f1.denominator.0,
                f2.numerator.0,
                f2.denominator.0,
            );
            prop_assert_eq!(result, oracle);
        }
    }
}
