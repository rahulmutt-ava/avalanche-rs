// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Frozen golden vectors for the SAE gas clock (specs/21 §6, `scaling=87`,
//! `min_price=1`). Derived from the Go reference `vms/saevm/gastime`; exact
//! Go-node differential parity is verified later (M7.29).

#![allow(clippy::arithmetic_side_effects)]

use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_vm::components::gas::{Gas, Price, calculate_price};

const TARGET: u64 = 1_000_000;
const SCALING: u64 = 87;
const K: u64 = TARGET * SCALING; // 87_000_000

fn default_cfg() -> GasPriceConfig {
    GasPriceConfig::default()
}

#[test]
fn vector1_price_at_excess_zero_and_e() {
    // target=1e6 => K=87e6; excess=0 => price=1.
    let tm = GasTime::new(0, TARGET, 0, default_cfg());
    assert_eq!(tm.target(), Gas(TARGET));
    assert_eq!(tm.excess_scaling_factor(), Gas(K));
    assert_eq!(tm.price(), Price(1));

    // excess = K = 87_000_000 => price = 2 (e^1 floored).
    let tm = GasTime::new(0, TARGET, K, default_cfg());
    assert_eq!(tm.excess(), Gas(K));
    assert_eq!(tm.price(), Price(2));
}

#[test]
fn vector2_tick_accrual() {
    // target=1e6 (R=2e6); tick(1_000_000) from excess 0 => excess=500_000.
    let mut tm = GasTime::new(0, TARGET, 0, default_cfg());
    assert_eq!(tm.rate(), 2 * TARGET);
    tm.tick(1_000_000);
    assert_eq!(tm.excess(), Gas(500_000));
    assert_eq!(tm.price(), Price(1));
}

#[test]
fn vector3_fast_forward_decay() {
    // target=1e6 (R=2e6), excess=1_000_000; advance s=1s, f=0
    // => excess decays by 1*T=1_000_000 => 0; enforce_min_excess keeps >= 0.
    let mut tm = GasTime::new(0, TARGET, 1_000_000, default_cfg());
    assert_eq!(tm.excess(), Gas(1_000_000));
    // Advance exactly one whole second (nanos = 0).
    tm.before_block(1, 0);
    assert_eq!(tm.excess(), Gas(0));
    // excess_for_price(1, 87e6) == 0, so the floor is respected.
    assert_eq!(tm.price(), Price(1));
}

#[test]
fn price_floor_respects_min_price() {
    // With a high min_price and zero excess, price() must floor at min_price.
    // calculate_price(Price(1), Gas(0), Gas(K)) == 1, so the floor dominates.
    let cfg = GasPriceConfig::new(200, SCALING, false);
    // Construct then assert: enforce_min_excess raises excess to satisfy 200.
    let tm = GasTime::new(0, TARGET, 0, cfg);
    assert!(tm.price().0 >= 200, "price {} < min 200", tm.price().0);
}

#[test]
fn price_matches_calculate_price_rows() {
    // A couple of §0 calculate_price rows reachable via price() (min_price=1
    // floor, so the exponential dominates for excess >= 0).
    for &(excess, _label) in &[(0u64, "e^0"), (K, "e^1"), (2 * K, "e^2")] {
        let tm = GasTime::new(0, TARGET, excess, default_cfg());
        let direct = calculate_price(Price(1), Gas(excess), Gas(K)).0;
        let expected = direct.max(1);
        assert_eq!(
            tm.price(),
            Price(expected),
            "excess={excess}: price() {} != calculate_price row {expected}",
            tm.price().0
        );
    }
}
