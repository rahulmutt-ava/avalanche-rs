// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Tests for the 128-bit-widening fraction comparison helper.
//!
//! Authoritative semantics: `proxytime.go::FractionalSecond.Compare` in the Go
//! reference tree (`vms/saevm/proxytime/proxytime.go`), which cross-multiplies
//! `f.num * g.den` against `g.num * f.den` via `bits.Mul64` (a 128-bit-wide
//! product). The Rust analog widens both `u64` operands to `u128`.

use std::cmp::Ordering;

use ava_saevm_cmputils::compare_fractions;

/// Widening reference: compute the cross-products in `u128` directly and compare.
/// `u64 * u64` always fits in `u128` (`(2^64-1)^2 < 2^128`), so this is exact.
fn widening_reference(n1: u64, d1: u64, n2: u64, d2: u64) -> Ordering {
    // `wrapping_mul` is exact here: `u64 * u64` always fits in `u128`.
    let left = u128::from(n1).wrapping_mul(u128::from(d2));
    let right = u128::from(n2).wrapping_mul(u128::from(d1));
    left.cmp(&right)
}

#[test]
fn cross_mul_compare_matches_widening() {
    // (n1, d1, n2, d2, expected)
    let cases: &[(u64, u64, u64, u64, Ordering)] = &[
        // Equal fractions, same denominator.
        (1, 3, 1, 3, Ordering::Equal),
        // Differing-denominator cases (the §2.1 cross-multiplication requirement).
        (1, 3, 1, 2, Ordering::Less),
        (2, 3, 1, 2, Ordering::Greater),
        (1, 2, 2, 4, Ordering::Equal),
        // Some more ordinary differing-denominator comparisons.
        (3, 4, 2, 3, Ordering::Greater),
        (2, 5, 3, 7, Ordering::Less),
        (0, 1, 0, 999, Ordering::Equal),
        // Large values near u64::MAX to exercise the full 128-bit width.
        (u64::MAX, 1, u64::MAX - 1, 1, Ordering::Greater),
        (u64::MAX, u64::MAX, u64::MAX, u64::MAX, Ordering::Equal),
        // A case where the full 128-bit product (not just the high word) decides.
        (u64::MAX, 2, u64::MAX, 3, Ordering::Greater),
        (u64::MAX - 1, u64::MAX, u64::MAX, u64::MAX, Ordering::Less),
    ];

    for &(n1, d1, n2, d2, expected) in cases {
        let got = compare_fractions(n1, d1, n2, d2);
        assert_eq!(
            got, expected,
            "compare_fractions({n1}, {d1}, {n2}, {d2}) = {got:?}, want {expected:?}",
        );
        // And it must agree with the inline 128-bit widening reference.
        assert_eq!(
            got,
            widening_reference(n1, d1, n2, d2),
            "compare_fractions({n1}, {d1}, {n2}, {d2}) disagrees with widening reference",
        );
    }
}

#[test]
fn antisymmetric() {
    // Swapping the two fractions must flip (or preserve Equal) the ordering.
    let cases: &[(u64, u64, u64, u64)] = &[(1, 3, 1, 2), (u64::MAX, 2, u64::MAX, 3), (5, 5, 5, 5)];
    for &(n1, d1, n2, d2) in cases {
        assert_eq!(
            compare_fractions(n1, d1, n2, d2).reverse(),
            compare_fractions(n2, d2, n1, d1),
        );
    }
}
