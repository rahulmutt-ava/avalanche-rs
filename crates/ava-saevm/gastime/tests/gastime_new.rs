// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Price-based `GasTime::new` construction (upstream-delta M7.36, Go
//! `gastime_test.go::TestNewInitialState` / `TestMinAndStaticPrice`).
//!
//! `GasTime::new(at, target, starting_price, cfg)` derives the starting excess
//! from the price via `excess_for_price`, so `tm.price()` round-trips back to
//! the requested price within the integer-exponential approximation (the closest
//! representable value at-or-just-below the request, floored at `min_price`).

#![allow(clippy::arithmetic_side_effects)]

use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_vm::components::gas::{Gas, Price};

const SCALING: u64 = 87;

fn default_cfg() -> GasPriceConfig {
    GasPriceConfig::default()
}

#[test]
fn new_price_zero_yields_excess_zero() {
    // excess_for_price(0, K) == 0 (price <= 1 short-circuits), so a price-0
    // construction is identical to an excess-0 construction.
    let from_price = GasTime::new(0, 1_000_000, Price(0), default_cfg());
    let from_excess = GasTime::from_excess(0, 1_000_000, 0, default_cfg());
    assert_eq!(from_price.excess(), Gas(0), "new(price=0).excess()");
    assert_eq!(
        from_price.excess(),
        from_excess.excess(),
        "new(price=0) == from_excess(0)"
    );
    // min_price=1 default => price floors at 1.
    assert_eq!(from_price.price(), Price(1), "new(price=0).price()");
}

#[test]
fn new_price_is_maintained() {
    // Go `TestNewInitialState` "price is maintained": target=1e6/2, price=123_456
    // round-trips exactly.
    let target = 1_000_000 / 2;
    let tm = GasTime::new(0, target, Price(123_456), default_cfg());
    assert_eq!(tm.target(), Gas(target), "target");
    assert_eq!(
        tm.price(),
        Price(123_456),
        "price() round-trips the request"
    );
}

#[test]
fn new_scaling_in_constructor_not_applied_to_starting_price() {
    // Go `TestNewInitialState` "scaling in constructor not applied to starting
    // price": with a MaxUint64 base fee the resulting price saturates at
    // MaxUint64 (the excess is capped, not the scaling re-applied).
    let target = 50 / 2;
    let tm = GasTime::new(100, target, Price(u64::MAX), default_cfg());
    assert_eq!(tm.target(), Gas(target), "target");
    assert_eq!(tm.price(), Price(u64::MAX), "MaxUint64 base fee caps price");
}

#[test]
fn new_price_round_trips_within_rounding() {
    // `excess_for_price` returns the minimum excess with `calculate_price >= p`,
    // OR (when `p` isn't exactly representable by the integer exponential) the
    // maximum excess producing a value `< p` — Go's "honor the lower price"
    // branch. So `price()` round-trips to within one representable step of the
    // request: it never exceeds the request, and incrementing the excess by one
    // would meet-or-exceed it. We assert the weaker, always-true bound that the
    // round-trip price is close (within 0.1%) and never far above the request.
    let cfg = default_cfg();
    for &price in &[2u64, 100, 123_456, 1_000_000, 1u64 << 40] {
        let got = GasTime::new(0, 1_000_000, Price(price), cfg).price().0;
        // Never materially above the request (price() is the closest representable
        // value at-or-just-below p, floored at min_price=1).
        assert!(
            got <= price.max(cfg.min_price()),
            "new(price={price}).price()={got} must not exceed the request"
        );
        // And within one part in a thousand below (integer-exponential precision).
        let tolerance = price / 1000 + 2;
        assert!(
            got + tolerance >= price,
            "new(price={price}).price()={got} too far below request"
        );
    }
}

#[test]
fn new_high_min_price_no_overflow() {
    // Go `TestMinAndStaticPrice` "high_min_no_overflow": a near-max min_price with
    // a near-max requested price saturates at MaxUint64 without overflow.
    let cfg = GasPriceConfig::new(u64::MAX / 2, SCALING, false);
    let tm = GasTime::new(0, 1_000_000, Price(u64::MAX), cfg);
    assert_eq!(tm.price(), Price(u64::MAX), "saturates at MaxUint64");
}

#[test]
fn new_static_pricing_forces_min_price() {
    // Go `TestMinAndStaticPrice` "static_pricing_returns_min": under static
    // pricing the derived excess is forced to zero (then raised by
    // enforce_min_excess to the minimum satisfying min_price), so regardless of
    // the requested price, price() == min_price.
    let cfg = GasPriceConfig::new(123_456, SCALING, true);
    let tm = GasTime::new(0, 1_000_000, Price(u64::MAX), cfg);
    assert_eq!(
        tm.price(),
        Price(123_456),
        "static pricing returns min_price"
    );
}

#[test]
fn new_price_below_min_floors_to_min() {
    // A requested price below min_price still yields at least min_price.
    let cfg = GasPriceConfig::new(200, SCALING, false);
    let tm = GasTime::new(0, 1_000_000, Price(50), cfg);
    assert!(tm.price().0 >= 200, "price {} < min 200", tm.price().0);
}
