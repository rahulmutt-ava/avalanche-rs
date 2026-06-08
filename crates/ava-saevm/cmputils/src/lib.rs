// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-cmputils` — SAE comparison helper.
//!
//! Go's `vms/saevm/cmputils/` is a set of `go-cmp` option builders used only in
//! Go tests; it has no Rust analog (Rust uses `PartialEq`/`pretty_assertions`).
//! This crate is instead repurposed (per `plan/M7-saevm.md` Task M7.4) to hold
//! the **128-bit-widening fraction comparison helper** that `proxytime` (M7.5)
//! needs at runtime.
//!
//! The authoritative semantics come from
//! `vms/saevm/proxytime/proxytime.go::FractionalSecond.Compare`, which compares
//! `f.num/f.den` against `g.num/g.den` by cross-multiplying `f.num * g.den`
//! against `g.num * f.den` using Go's `bits.Mul64` (a 128-bit-wide product), so
//! fractions with differing denominators compare exactly with no precision loss.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]

use std::cmp::Ordering;

/// Compares the fractions `n1/d1` and `n2/d2` exactly via 128-bit
/// cross-multiplication.
///
/// Returns [`Ordering::Less`], [`Ordering::Equal`], or [`Ordering::Greater`]
/// according to whether `n1/d1` is less than, equal to, or greater than
/// `n2/d2`. Because the operands are non-negative, comparing the cross-products
/// `n1 * d2` and `n2 * d1` preserves order, and each product is widened to
/// `u128` so two fractions with different denominators compare exactly with no
/// precision loss.
///
/// This mirrors `proxytime.go::FractionalSecond.Compare` (`bits.Mul64` (Hi, Lo)
/// compare): `u64 * u64` always fits in `u128` (`(2^64 - 1)^2 < 2^128`), so the
/// widening multiply is exact and panic-free. `wrapping_mul` is used because it
/// cannot overflow `u128` here and is lint-clean under
/// `clippy::arithmetic_side_effects`.
#[must_use]
pub fn compare_fractions(n1: u64, d1: u64, n2: u64, d2: u64) -> Ordering {
    // Cross-multiply: n1*d2 vs n2*d1; each fits exactly in u128.
    let left = u128::from(n1).wrapping_mul(u128::from(d2));
    let right = u128::from(n2).wrapping_mul(u128::from(d1));
    left.cmp(&right)
}
