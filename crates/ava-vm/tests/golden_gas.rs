// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::gas_*` — golden vectors for the ACP-103 gas primitive (specs 07
//! §3.4, specs 02).
//!
//! Provenance: the `calculate_price` / `cost` / `add_over_time` /
//! `sub_over_time` / `consume` vectors are copied verbatim from the Go reference
//! tests `avalanchego/vms/components/gas/{gas_test.go,state_test.go}`
//! (`Test_CalculatePrice`, `Test_Gas_Cost`, `Test_Gas_AddOverTime`,
//! `Test_Gas_SubOverTime`, `Test_State_ConsumeGas`). The fixed-point loop is
//! integer-only (no floats — specs 00 §6.1).

#![allow(unused_crate_dependencies, clippy::unwrap_used, clippy::expect_used)]

use assert_matches::assert_matches;

use ava_vm::components::gas::{Gas, GasState, Price, calculate_price};
use ava_vm::error::Error;

/// `golden::gas_calculate_price` — the integer `fakeExponential` fixed-point
/// approximation of `min_price * e^(excess / K)`, diffed against Go.
#[test]
fn gas_calculate_price() {
    // (min_price, excess, K, expected) — verbatim from Go `Test_CalculatePrice`.
    let cases: &[(u64, u64, u64, u64)] = &[
        (1, 0, 1, 1),
        (1, 1, 1, 2),
        (1, 2, 1, 6),
        (1, 10_000, 10_000, 2),
        (1, 1_000_000, 10_000, u64::MAX),
        (10, 10_000_000, 1_000_000, 220_264),
        (u64::MAX, u64::MAX, 1, u64::MAX),
        (u32::MAX as u64, 1, 1, 11_674_931_546),
        (6_786_177_901_268_885_274, 1, 1, u64::MAX - 11),
        (6_786_177_901_268_885_274, u64::MAX, u64::MAX, u64::MAX - 1),
    ];

    for &(min_price, excess, k, expected) in cases {
        let got = calculate_price(Price(min_price), Gas(excess), Gas(k));
        assert_eq!(
            got,
            Price(expected),
            "calculate_price(min={min_price}, excess={excess}, k={k})"
        );
    }
}

/// `golden::gas_cost` — `Gas.Cost(price) == gas * price` (checked).
#[test]
fn gas_cost() {
    assert_eq!(Gas(40).cost(Price(100)).expect("cost"), 4000);
    // Overflow ⇒ Error::Overflow.
    assert_matches!(Gas(u64::MAX).cost(Price(2)), Err(Error::Overflow));
}

/// `golden::gas_add_sub_over_time` — saturating capacity refill / excess decay.
#[test]
fn gas_add_sub_over_time() {
    // AddOverTime: g + rate*duration, saturating at MaxU64.
    assert_eq!(Gas(5).add_over_time(Gas(1), 2), Gas(7));
    assert_eq!(Gas(5).add_over_time(Gas(u64::MAX), 2), Gas(u64::MAX));
    assert_eq!(Gas(u64::MAX).add_over_time(Gas(1), 2), Gas(u64::MAX));

    // SubOverTime: g - rate*duration, saturating at 0.
    assert_eq!(Gas(5).sub_over_time(Gas(1), 2), Gas(3));
    assert_eq!(Gas(5).sub_over_time(Gas(1), 10), Gas(0));
    assert_eq!(Gas(5).sub_over_time(Gas(u64::MAX), 2), Gas(0));
}

/// `golden::gas_state_advance_consume` — capacity/excess transitions.
#[test]
fn gas_state_advance_consume() {
    // ConsumeGas: capacity -= gas, excess += gas.
    let s = GasState {
        capacity: Gas(100),
        excess: Gas(10),
    };
    let consumed = s.consume(Gas(30)).expect("consume");
    assert_eq!(consumed.capacity, Gas(70));
    assert_eq!(consumed.excess, Gas(40));

    // Insufficient capacity ⇒ error.
    assert_matches!(s.consume(Gas(101)), Err(Error::InsufficientCapacity));

    // Excess saturates at MaxU64 (capacity still decremented).
    let s2 = GasState {
        capacity: Gas(10),
        excess: Gas(u64::MAX),
    };
    let consumed2 = s2.consume(Gas(5)).expect("consume sat");
    assert_eq!(consumed2.capacity, Gas(5));
    assert_eq!(consumed2.excess, Gas(u64::MAX));

    // AdvanceTime: capacity refilled (capped), excess decayed.
    let s3 = GasState {
        capacity: Gas(50),
        excess: Gas(100),
    };
    let advanced = s3.advance(Gas(80), Gas(10), Gas(5), 3);
    // capacity = min(50 + 10*3, 80) = 80; excess = 100 - 5*3 = 85.
    assert_eq!(advanced.capacity, Gas(80));
    assert_eq!(advanced.excess, Gas(85));
}
