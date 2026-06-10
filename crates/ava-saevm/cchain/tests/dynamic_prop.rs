// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Property tests for the `dynamic` exponent integrators (M7.34).
//!
//! Verifies the round-trip invariant: for any value `v`, the exponent returned
//! by `Desired*(v)` has a reader value `>= v`, and the exponent just below (if
//! it exists) has a reader value `< v`.

// The arithmetic in the proptest closures uses subtraction on arbitrary u64
// inputs but only in a controlled way.  Allow it here rather than cluttering
// the assertions with checked_ calls.
#![allow(clippy::arithmetic_side_effects)]

use ava_saevm_cchain::dynamic::{
    DelayExponent, PriceExponent, TargetExponent, desired_delay_exponent, desired_price_exponent,
    desired_target_exponent,
};
use ava_vm::components::gas::{Gas, Price};
use proptest::prelude::*;

proptest! {
    /// `desired_target_exponent(v).target() >= v`, and the exponent just below
    /// (if any) has `target() < v`.
    ///
    /// Named to match the `test(dynamic)` nextest filter.
    #[test]
    fn dynamic_desired_target_exponent_inverts_reader(v in 0u64..=u64::MAX) {
        let exp = desired_target_exponent(Gas(v));
        prop_assert!(exp.target() >= Gas(v),
            "desired={v}: exp={:?} has target={:?} < desired", exp, exp.target());
        if exp.0 > 0 {
            let prev = TargetExponent(exp.0 - 1);
            prop_assert!(prev.target() < Gas(v),
                "desired={v}: exp-1={:?} has target={:?} >= desired", prev, prev.target());
        }
    }

    /// `desired_delay_exponent(v).delay() >= v`, and the exponent just below
    /// (if any) has `delay() < v`.
    #[test]
    fn dynamic_desired_delay_exponent_inverts_reader(v in 0u64..=u64::MAX) {
        let exp = desired_delay_exponent(v);
        prop_assert!(exp.delay() >= v,
            "desired={v}: exp={:?} has delay={} < desired", exp, exp.delay());
        if exp.0 > 0 {
            let prev = DelayExponent(exp.0 - 1);
            prop_assert!(prev.delay() < v,
                "desired={v}: exp-1={:?} has delay={} >= desired", prev, prev.delay());
        }
    }

    /// `desired_price_exponent(v).price() >= v`, and the exponent just below
    /// (if any) has `price() < v`.
    #[test]
    fn dynamic_desired_price_exponent_inverts_reader(v in 0u64..=u64::MAX) {
        let exp = desired_price_exponent(Price(v));
        prop_assert!(exp.price() >= Price(v),
            "desired={v}: exp={:?} has price={:?} < desired", exp, exp.price());
        if exp.0 > 0 {
            let prev = PriceExponent(exp.0 - 1);
            prop_assert!(prev.price() < Price(v),
                "desired={v}: exp-1={:?} has price={:?} >= desired", prev, prev.price());
        }
    }
}
