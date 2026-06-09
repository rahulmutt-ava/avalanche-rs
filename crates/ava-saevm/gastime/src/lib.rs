// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-gastime` — the SAE gas clock: Tau-pinned rate, excess scaling,
//! and the ACP-176 price (specs/11 §2.2, specs/21 §6).
//!
//! [`GasTime`] wraps a [`proxytime::Time<u64>`] (the proxy-unit clock measured
//! in gas) and layers the ACP-176/194 dynamic-fee state on top: the target `T`,
//! the excess `x`, and a [`GasPriceConfig`]. SAE measures the passage of time in
//! gas — consuming `target * TARGET_TO_RATE` gas equals one wall-clock second.
//!
//! This is a faithful Rust port of `vms/saevm/gastime/{gastime,acp176,config}.go`.
//!
//! # Boundary typing
//!
//! Public accessors that mirror the Go gas API expose [`Gas`] / [`Price`]
//! (the `ava_vm::components::gas` newtypes), while all internal state and
//! arithmetic is `u64` (matching the underlying `Time<u64>` proxy clock). The
//! `u64` <-> `Gas`/`Price` conversions happen only at the public boundary.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]

mod config;

use std::cmp::Ordering;

use ava_saevm_intmath::{bounded_add, bounded_multiply, bounded_sub, mul_div_ceil, mul_div_floor};
use ava_vm::components::gas::{Gas, Price, calculate_price};
use ruint::aliases::U256;

pub use crate::config::{DEFAULT_MIN_PRICE, DEFAULT_TARGET_TO_EXCESS_SCALING, GasPriceConfig};

/// The ratio between [`GasTime::target`] and the underlying proxy-clock rate
/// (`R = TARGET_TO_RATE * T`). Mirrors Go's `TargetToRate`.
pub const TARGET_TO_RATE: u64 = 2;

/// The minimum allowable target, to avoid division by zero. Values below this
/// are silently clamped. Mirrors Go's `MinTarget`.
pub const MIN_TARGET: u64 = 1;

/// The maximum allowable target, to avoid overflow of the associated proxy-clock
/// rate. Values above this are silently clamped. Mirrors Go's `MaxTarget`
/// (`MaxUint64 / TargetToRate`).
pub const MAX_TARGET: u64 = u64::MAX / TARGET_TO_RATE;

/// Number of nanoseconds in one second (the proxy-clock fractional denominator
/// when converting a wall-clock instant to gas).
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Clamps `target` to the allowable range `[MIN_TARGET, MAX_TARGET]`.
///
/// Mirrors Go's `clampTarget`.
#[must_use]
fn clamp_target(target: u64) -> u64 {
    target.clamp(MIN_TARGET, MAX_TARGET)
}

/// Returns the proxy-clock rate for `target` (`R = TARGET_TO_RATE * T`),
/// saturating at `u64::MAX`. `target` is assumed already clamped to
/// `[MIN_TARGET, MAX_TARGET]`, so the product never actually saturates.
///
/// Mirrors Go's `rateOf`.
#[must_use]
fn rate_of(target: u64) -> u64 {
    bounded_multiply(target, TARGET_TO_RATE, u64::MAX)
}

/// The K variable of ACP-103/176, i.e. `scaling * T`, capped at `u64::MAX`.
///
/// Mirrors Go's `Time.excessScalingFactor`.
#[must_use]
fn excess_scaling_factor_of(scaling: u64, target: u64) -> u64 {
    bounded_multiply(scaling, target, u64::MAX)
}

/// `calculatePrice(x, k)` — an integer approximation of `e^(x/k)`, i.e.
/// `calculate_price(Price(1), Gas(x), Gas(k))`.
///
/// Mirrors Go's `gastime.calculatePrice`.
#[must_use]
fn calculate_price_xk(x: u64, k: u64) -> u64 {
    calculate_price(Price(1), Gas(x), Gas(k)).0
}

/// Returns an integer approximation of `ln(p) * k`.
///
/// If [`calculate_price_xk`] can produce `p`, returns the minimum excess `x`
/// with `calculate_price_xk(x, k) >= p`. Otherwise (overflow or integer
/// approximation) it returns the maximum excess producing a value `< p`.
///
/// Bit-for-bit port of Go's `excessForPrice` (lo/hi bisection + the
/// "honor the lower price" branch).
#[must_use]
fn excess_for_price(p: u64, k: u64) -> u64 {
    if p <= 1 {
        return 0;
    }
    // Binary search for the minimum x where calculate_price_xk(x, k) >= p.
    //
    // calculate_price_xk(0, k) == 1 and p > 1, so lo > 0.
    let mut lo: u64 = 1;
    let mut hi: u64 = u64::MAX;
    while lo < hi {
        // mid = lo + (hi - lo) / 2 (overflow-free midpoint).
        let half = hi.wrapping_sub(lo).wrapping_div(2);
        let mid = lo.wrapping_add(half);
        if calculate_price_xk(mid, k) >= p {
            hi = mid;
        } else {
            // lo = mid + 1; mid < hi <= u64::MAX so this cannot overflow.
            lo = mid.wrapping_add(1);
        }
    }
    // If calculate_price_xk can't generate p due to integer approximation,
    // honor the lower price expectation: return lo - 1 (lo >= 1 here).
    if calculate_price_xk(lo, k) > p {
        return lo.wrapping_sub(1);
    }
    lo
}

/// Returns `oldX * newK / oldK` rounded **up** and saturated to `u64::MAX`.
///
/// Computed in [`U256`] so the intermediate `oldX * newK` (up to
/// `MaxUint64^2`) never overflows. Mirrors Go's `scaleExcess` (which uses
/// `uint256`), with `K = T * scaling`.
#[must_use]
fn scale_excess(
    old_x: u64,
    new_target: u64,
    new_scaling: u64,
    old_target: u64,
    old_scaling: u64,
) -> u64 {
    let new_k = U256::from(new_target).saturating_mul(U256::from(new_scaling));
    let old_k = U256::from(old_target).saturating_mul(U256::from(old_scaling));

    if old_k == U256::ZERO {
        // K is documented non-zero (target >= 1, scaling >= 1). Defensive: no
        // scaling possible, keep excess unchanged.
        return old_x;
    }

    // x = oldX * newK; round up by adding (oldK - 1) before the floor divide.
    let mut x = U256::from(old_x).saturating_mul(new_k);
    // oldK >= 1 here, so oldK - 1 cannot underflow.
    x = x.saturating_add(old_k.saturating_sub(U256::from(1u64)));
    let scaled = x.wrapping_div(old_k);

    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// The SAE gas clock: an instant in time whose passage is measured in
/// [`Gas`] consumption, tracking the ACP-176 dynamic-fee state.
///
/// Wraps a [`proxytime::Time<u64>`] (rate pinned to `R = TARGET_TO_RATE * T`)
/// plus the target `T`, excess `x`, and a [`GasPriceConfig`].
///
/// Port of `vms/saevm/gastime/gastime.go::Time`.
#[derive(Clone, Debug)]
pub struct GasTime {
    inner: ava_saevm_proxytime::Time<u64>,
    target: u64,
    excess: u64,
    config: GasPriceConfig,
}

impl GasTime {
    /// Creates a new [`GasTime`] at the given Unix `unix_seconds`.
    ///
    /// The consumption of `target * TARGET_TO_RATE` units of gas is equivalent
    /// to a tick of one second. `target` is clamped to
    /// `[MIN_TARGET, MAX_TARGET]`; the proxy-clock rate is pinned to
    /// `R = TARGET_TO_RATE * target`. Under static pricing the starting excess
    /// is forced to zero. After construction, [`enforce_min_excess`] is applied.
    ///
    /// Mirrors Go's `gastime.New` / `FromProxyTime`.
    ///
    /// [`enforce_min_excess`]: GasTime::enforce_min_excess
    #[must_use]
    pub fn new(
        unix_seconds: u64,
        target: u64,
        starting_excess: u64,
        config: GasPriceConfig,
    ) -> Self {
        let target = clamp_target(target);
        let rate = rate_of(target);
        let inner = ava_saevm_proxytime::Time::new(unix_seconds, 0, rate);

        let excess = if config.static_pricing() {
            0
        } else {
            starting_excess
        };

        let mut tm = Self {
            inner,
            target,
            excess,
            config,
        };
        tm.enforce_min_excess();
        tm
    }

    /// Reconstructs a [`GasTime`] from a settled block's gas-clock components.
    ///
    /// Mirrors Go's `hook.SettledGasTime`, which builds
    /// `proxytime.New(gas_unix, gas_numerator, SafeRateOfTarget(target))` and
    /// then `gastime.FromProxyTime(pt, excess, config)`. Unlike [`new`], the
    /// proxy clock starts with a non-zero sub-second fraction (`gas_numerator`)
    /// and the target is derived from the (clamped) rate (`rate / TARGET_TO_RATE`),
    /// matching `FromProxyTime`. Under static pricing the excess is forced to
    /// zero; afterwards [`enforce_min_excess`] is applied.
    ///
    /// [`new`]: GasTime::new
    /// [`enforce_min_excess`]: GasTime::enforce_min_excess
    #[must_use]
    pub fn from_settled(
        gas_unix: u64,
        gas_numerator: u64,
        target: u64,
        excess: u64,
        config: GasPriceConfig,
    ) -> Self {
        // SafeRateOfTarget(target) = rate_of(clamp_target(target)).
        let rate = rate_of(clamp_target(target));
        let inner = ava_saevm_proxytime::Time::new(gas_unix, gas_numerator, rate);

        // FromProxyTime derives the target from the (clamped) rate.
        // const divisor (TARGET_TO_RATE = 2, never 0); wrapping_div only to satisfy arithmetic_side_effects
        let target = rate.wrapping_div(TARGET_TO_RATE);

        let excess = if config.static_pricing() { 0 } else { excess };

        let mut tm = Self {
            inner,
            target,
            excess,
            config,
        };
        tm.enforce_min_excess();
        tm
    }

    /// Returns the `T` parameter of ACP-176.
    ///
    /// Mirrors Go's `Time.Target`.
    #[must_use]
    pub fn target(&self) -> Gas {
        Gas(self.target)
    }

    /// Returns the `x` variable of ACP-176.
    ///
    /// Mirrors Go's `Time.Excess`.
    #[must_use]
    pub fn excess(&self) -> Gas {
        Gas(self.excess)
    }

    /// Returns the proxy-clock rate (`R = TARGET_TO_RATE * T`), i.e. gas units
    /// per wall-clock second.
    ///
    /// Mirrors Go's `proxytime.Time.Rate`.
    #[must_use]
    pub fn rate(&self) -> u64 {
        self.inner.rate()
    }

    /// Returns the K variable of ACP-103/176 (`scaling * T`, capped at
    /// `u64::MAX`).
    ///
    /// Mirrors Go's `Time.excessScalingFactor`.
    #[must_use]
    pub fn excess_scaling_factor(&self) -> Gas {
        Gas(excess_scaling_factor_of(
            self.config.target_to_excess_scaling(),
            self.target,
        ))
    }

    /// Returns the price of a unit of gas (the "base fee"), determined by the
    /// ACP-176 exponential, floored at the configured minimum price.
    ///
    /// Mirrors Go's `Time.Price`.
    #[must_use]
    pub fn price(&self) -> Price {
        let k = excess_scaling_factor_of(self.config.target_to_excess_scaling(), self.target);
        let p = calculate_price_xk(self.excess, k);
        // When min_price can't be represented by e^(x/k), p may be too low.
        Price(p.max(self.config.min_price()))
    }

    /// Advances the gas clock before processing a block at wall-clock instant
    /// `(unix_seconds, nanos)`, decaying the excess by the gas skipped.
    ///
    /// The wall-clock instant is converted to a gas fraction via
    /// `mul_div_ceil(nanos, rate, NANOS_PER_SECOND)` (CEIL); the proxy clock is
    /// fast-forwarded; then the excess decays by `s*T + floor(f*T/R)` (each term
    /// bounded at 0), where `s` is the advanced whole seconds and `f` the
    /// advanced fractional numerator. This matches the ACP reduction of
    /// `-T*dt`. Finally [`enforce_min_excess`] is applied.
    ///
    /// Mirrors Go's `Time.BeforeBlock` / `FastForwardToTime` / `FastForwardTo`.
    ///
    /// [`enforce_min_excess`]: GasTime::enforce_min_excess
    pub fn before_block(&mut self, unix_seconds: u64, nanos: u32) {
        let rate = self.rate();
        // nanos is in [0, 1e9), so this never overflows / errors for valid
        // inputs; default to the full second's worth of gas on the impossible
        // error path rather than panicking.
        let gas_frac = mul_div_ceil(u64::from(nanos), rate, NANOS_PER_SECOND).unwrap_or(rate);

        let (sec, frac) = self.inner.fast_forward_to(unix_seconds, gas_frac);
        if sec == 0 && frac.numerator == 0 {
            return;
        }

        let r = self.rate();
        let t = self.target;

        // -sT : decay by s whole seconds' worth of target, bounded at 0.
        //   s*T computed via bounded_multiply (saturates), then bounded_sub.
        let s_t = bounded_multiply(sec, t, u64::MAX);
        self.excess = bounded_sub(self.excess, s_t, 0);

        // -fT/R : decay by the fractional remainder. T/R < 1 so this never
        // overflows; default to 0 on the impossible error path.
        let frac_decay = mul_div_floor(frac.numerator, t, r).unwrap_or(0);
        self.excess = bounded_sub(self.excess, frac_decay, 0);

        self.enforce_min_excess();
    }

    /// Advances the gas clock by `used` gas, updating the excess.
    ///
    /// The proxy clock ticks by `used`. Under dynamic pricing the excess
    /// increases by `floor(used * (R - T) / R)` (with `R = 2T` this is
    /// `floor(used / 2)`), bounded at `u64::MAX`. Under static pricing the
    /// excess is left unchanged (held at its minimum).
    ///
    /// Mirrors Go's `Time.Tick`.
    pub fn tick(&mut self, used: u64) {
        self.inner.tick(used);

        if self.config.static_pricing() {
            return;
        }

        let r = self.rate();
        let t = self.target;
        // R - T is safe (R = 2T >= T); ratio (R-T)/R < 1 so mul_div_floor never
        // overflows. Default to 0 on the impossible error path.
        let r_minus_t = r.saturating_sub(t);
        let quo = mul_div_floor(used, r_minus_t, r).unwrap_or(0);
        self.excess = bounded_add(self.excess, quo, u64::MAX);
    }

    /// Processes a block: ticks by `used`, then rescales the excess to the new
    /// target and re-pins the proxy-clock rate.
    ///
    /// After [`tick`], under dynamic pricing the excess is rescaled via
    /// [`scale_excess`] (round up, saturating, `K = T * scaling`). The target
    /// is then clamped, the proxy-clock rate re-pinned to `2 * new_target`, the
    /// new config stored, and [`enforce_min_excess`] applied. Under static
    /// pricing the excess is forced to zero.
    ///
    /// Mirrors Go's `Time.AfterBlock`.
    ///
    /// [`tick`]: GasTime::tick
    /// [`enforce_min_excess`]: GasTime::enforce_min_excess
    pub fn after_block(&mut self, used: u64, new_target: u64, config: GasPriceConfig) {
        let new_target = clamp_target(new_target);

        self.tick(used);

        if config.static_pricing() {
            self.excess = 0;
        } else {
            self.excess = scale_excess(
                self.excess,
                new_target,
                config.target_to_excess_scaling(),
                self.target,
                self.config.target_to_excess_scaling(),
            );
        }

        self.target = new_target;
        self.inner.set_rate(rate_of(new_target));
        self.config = config;
        self.enforce_min_excess();
    }

    /// Bounds the excess to be no less than `excess_for_price(min_price, K)`.
    ///
    /// Avoids the binary search in [`excess_for_price`] when the current excess
    /// already yields a price satisfying the minimum.
    ///
    /// Mirrors Go's `Time.enforceMinExcess`.
    pub fn enforce_min_excess(&mut self) {
        let k = excess_scaling_factor_of(self.config.target_to_excess_scaling(), self.target);
        if calculate_price_xk(self.excess, k) >= self.config.min_price() {
            return;
        }
        let min_excess = excess_for_price(self.config.min_price(), k);
        self.excess = self.excess.max(min_excess);
    }

    /// Compares `self` with `other` by their temporal ordering, delegating to
    /// the underlying proxy clock.
    ///
    /// Mirrors Go's `proxytime.Time.Compare`.
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        self.inner.compare(&other.inner)
    }
}
