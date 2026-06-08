// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-intmath` — SAE special-case checked integer math.
//!
//! Rust port of the Go reference `vms/saevm/intmath/intmath.go`. Provides the
//! bounded (saturating) `u64` arithmetic and the overflow-aware `mul_div`
//! routines the SAE gas-as-time accounting relies on (`specs/11` §6,
//! `specs/21` §6/§8).
//!
//! # Mul-div intermediate width
//!
//! The Go reference computes `a*b` in 128 bits via `math/bits.Mul64` /
//! `bits.Div64`. The full `u128` product of two `u64`s is *exactly*
//! representable: `(2^64−1)^2 = 2^128 − 2^65 + 1 < 2^128`, and the ceil
//! adjustment `prod + (den−1)` is at most `2^128 − 2^64 − 1 < 2^128`. A plain
//! `u128` intermediate is therefore provably sufficient — no `U256` (and no
//! `ruint` dependency) is required. See the project report folded into
//! `specs/21` §6/§8.
//!
//! All arithmetic here is `checked_*`/`saturating_*`/`wrapping_*`/`div_ceil`;
//! there are no bare operators, no floats, and no `unwrap`/`panic` in the
//! library body. The single documented exception is [`ceil_div`], which panics
//! on a zero denominator exactly as Go's `bits.Div64` does.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]

use std::fmt;

/// Returned by [`mul_div_floor`] / [`mul_div_ceil`] when the quotient would not
/// fit in a `u64`, or when the denominator is zero.
///
/// Mirrors Go's `intmath.ErrOverflow`. A leaf utility carries its own minimal
/// unit error rather than pulling in `thiserror`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overflow;

impl fmt::Display for Overflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("overflow")
    }
}

impl std::error::Error for Overflow {}

/// Returns `min(a + b, ceil)` without overflowing.
///
/// Port of `BoundedAdd`: if the addition overflows `u64`, the result is clamped
/// to `ceil` (an overflowing sum is necessarily `> ceil`).
#[must_use]
pub fn bounded_add(a: u64, b: u64, ceil: u64) -> u64 {
    a.checked_add(b).map_or(ceil, |sum| sum.min(ceil))
}

/// Returns `max(a - b, floor)` without underflowing.
///
/// Port of `BoundedSubtract`: if `b > a` the (would-be) difference underflows,
/// so the result is clamped to `floor`.
#[must_use]
pub fn bounded_sub(a: u64, b: u64, floor: u64) -> u64 {
    a.checked_sub(b).map_or(floor, |diff| diff.max(floor))
}

/// Returns `min(a * b, ceil)` without overflowing.
///
/// Port of `BoundedMultiply`: an overflowing product is necessarily `> ceil`,
/// so it is clamped to `ceil`.
#[must_use]
pub fn bounded_multiply(a: u64, b: u64, ceil: u64) -> u64 {
    a.checked_mul(b).map_or(ceil, |prod| prod.min(ceil))
}

/// Returns `ceil(num / den)`, the rounded-up quotient.
///
/// Port of `CeilDiv`.
///
/// # Panics
///
/// Panics if `den == 0`, matching the documented behaviour of Go's
/// `bits.Div64` (a zero denominator is a programming error, not a runtime
/// condition the SAE call sites pass).
#[must_use]
pub fn ceil_div(num: u64, den: u64) -> u64 {
    num.div_ceil(den)
}

/// Returns `floor((a * b) / den)`, computing `a * b` in 128 bits so it never
/// overflows even when `a * b >= 2^64`.
///
/// Port of `MulDiv`. Returns [`Overflow`] if the quotient would not fit in a
/// `u64`, or if `den == 0`.
///
/// # Errors
///
/// Returns [`Overflow`] when `den == 0` or when `floor((a*b)/den) > u64::MAX`.
pub fn mul_div_floor(a: u64, b: u64, den: u64) -> Result<u64, Overflow> {
    // The full product of two u64s always fits in a u128, so `wrapping_mul`
    // here is exact (and lint-clean / panic-free).
    let prod = u128::from(a).wrapping_mul(u128::from(b));
    let quo = prod.checked_div(u128::from(den)).ok_or(Overflow)?;
    u64::try_from(quo).map_err(|_| Overflow)
}

/// Returns `ceil((a * b) / den)`, the rounded-up counterpart of
/// [`mul_div_floor`].
///
/// Port of `MulDivCeil` (the quotient component; Go additionally returns the
/// remainder's complement `den - rem - 1`, which the SAE call sites do not
/// use). Computes `a * b` in 128 bits, then `div_ceil` in 128 bits — both the
/// product and the ceil adjustment fit in a `u128` exactly (see crate docs).
///
/// # Errors
///
/// Returns [`Overflow`] when `den == 0` or when `ceil((a*b)/den) > u64::MAX`.
pub fn mul_div_ceil(a: u64, b: u64, den: u64) -> Result<u64, Overflow> {
    let den = u128::from(den);
    if den == 0 {
        return Err(Overflow);
    }
    let prod = u128::from(a).wrapping_mul(u128::from(b));
    // `den != 0` so `div_ceil` cannot panic; `prod + (den-1)` stays within u128.
    let quo = prod.div_ceil(den);
    u64::try_from(quo).map_err(|_| Overflow)
}
