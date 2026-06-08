// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Property and golden tests for `ava-saevm-intmath`.
//!
//! Mirrors the Go reference `vms/saevm/intmath/intmath.go` and pins the
//! worked "Tick accrual" example from `specs/11` §6 / `specs/21` §6.

use ava_saevm_intmath::{
    bounded_add, bounded_multiply, bounded_sub, ceil_div, mul_div_ceil, mul_div_floor,
};
use proptest::prelude::*;

// --- Golden vectors -------------------------------------------------------

#[test]
fn golden_tick_accrual_mul_div_floor() {
    // specs/11 §6 / specs/21 §6 worked example "Tick accrual":
    // Δexcess = ⌊1_000_000 · (2e6 − 1e6) / 2e6⌋ = ⌊1_000_000 · 1/2⌋ = 500_000.
    assert_eq!(mul_div_floor(1_000_000, 1_000_000, 2_000_000), Ok(500_000));
}

#[test]
fn golden_mul_div_ceil_rounds_up_by_one() {
    // 1·1/2 = 0.5: floor truncates to 0, ceil rounds up to 1.
    assert_eq!(mul_div_floor(1, 1, 2), Ok(0));
    assert_eq!(mul_div_ceil(1, 1, 2), Ok(1));
}

#[test]
fn golden_mul_div_exact_no_rounding() {
    // Exact division: floor == ceil.
    assert_eq!(mul_div_floor(6, 4, 3), Ok(8));
    assert_eq!(mul_div_ceil(6, 4, 3), Ok(8));
}

#[test]
fn golden_mul_div_overflow() {
    // (MaxU64 · MaxU64) / 1 cannot fit in u64 → ErrOverflow.
    assert!(mul_div_floor(u64::MAX, u64::MAX, 1).is_err());
    assert!(mul_div_ceil(u64::MAX, u64::MAX, 1).is_err());
}

#[test]
fn golden_mul_div_den_zero() {
    // Division by zero is an error, not a panic.
    assert!(mul_div_floor(1, 1, 0).is_err());
    assert!(mul_div_ceil(1, 1, 0).is_err());
}

#[test]
fn golden_ceil_div() {
    assert_eq!(ceil_div(7, 2), 4);
    assert_eq!(ceil_div(8, 2), 4);
    assert_eq!(ceil_div(0, 5), 0);
}

#[test]
fn golden_bounded_ops() {
    assert_eq!(bounded_add(u64::MAX, 1, u64::MAX), u64::MAX);
    assert_eq!(bounded_add(2, 3, u64::MAX), 5);
    assert_eq!(bounded_add(10, 10, 15), 15);

    assert_eq!(bounded_sub(0, 1, 0), 0);
    assert_eq!(bounded_sub(10, 3, 0), 7);
    assert_eq!(bounded_sub(10, 3, 8), 8);

    assert_eq!(bounded_multiply(u64::MAX, 2, u64::MAX), u64::MAX);
    assert_eq!(bounded_multiply(3, 4, u64::MAX), 12);
    assert_eq!(bounded_multiply(3, 4, 10), 10);
}

// --- Property tests -------------------------------------------------------

proptest! {
    /// For `c >= 1` and `b <= c` (ratio ≤ 1), `mul_div_floor` never panics and
    /// the result is `<= a`. When `b < c` (ratio strictly < 1) and `a > 0` the
    /// result is strictly `< a`.
    #[test]
    fn mul_div_no_overflow(a in any::<u64>(), b in any::<u64>(), c in 1u64..=u64::MAX) {
        prop_assume!(b <= c);
        let r = mul_div_floor(a, b, c).expect("ratio <= 1 cannot overflow u64");
        prop_assert!(r <= a);
        if b < c && a > 0 {
            prop_assert!(r < a);
        }
        // Ceil never undershoots floor.
        let rc = mul_div_ceil(a, b, c).expect("ratio <= 1 cannot overflow u64");
        prop_assert!(rc >= r);
    }

    #[test]
    fn bounded_add_saturates_to_ceil(a in any::<u64>(), b in any::<u64>(), ceil in any::<u64>()) {
        let r = bounded_add(a, b, ceil);
        prop_assert!(r <= ceil);
        if let Some(sum) = a.checked_add(b) {
            if sum <= ceil {
                prop_assert_eq!(r, sum);
            } else {
                prop_assert_eq!(r, ceil);
            }
        } else {
            // Overflow ⇒ clamps to ceil.
            prop_assert_eq!(r, ceil);
        }
    }

    #[test]
    fn bounded_sub_floors_at_floor(a in any::<u64>(), b in any::<u64>(), floor in any::<u64>()) {
        let r = bounded_sub(a, b, floor);
        prop_assert!(r >= floor);
        if let Some(diff) = a.checked_sub(b) {
            if diff >= floor {
                prop_assert_eq!(r, diff);
            } else {
                prop_assert_eq!(r, floor);
            }
        } else {
            // Underflow ⇒ clamps to floor.
            prop_assert_eq!(r, floor);
        }
    }

    #[test]
    fn bounded_multiply_caps_at_ceil(a in any::<u64>(), b in any::<u64>(), ceil in any::<u64>()) {
        let r = bounded_multiply(a, b, ceil);
        prop_assert!(r <= ceil);
        if let Some(prod) = a.checked_mul(b) {
            if prod <= ceil {
                prop_assert_eq!(r, prod);
            } else {
                prop_assert_eq!(r, ceil);
            }
        } else {
            prop_assert_eq!(r, ceil);
        }
    }
}
