// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/components/gas` — the ACP-103 dynamic-fee primitive (specs 07 §3.4).
//!
//! Pure integer math — **no floating point** (specs 00 §6.1). [`calculate_price`]
//! reproduces Go's `fakeExponential` fixed-point loop exactly; intermediate
//! values can reach ~`MaxUint192`, so the loop uses arbitrary-precision
//! [`num_bigint::BigUint`] (Go uses `uint256.Int`). The result is bit-identical
//! to Go for every input, which matters because SAE/EVM fee math is
//! consensus-affecting.

use num_bigint::BigUint;

use ava_utils::math as safemath;

use crate::error::{Error, Result};

/// The number of fee dimensions (`gas.NumDimensions`): bandwidth, DB-read,
/// DB-write, compute.
pub const NUM_DIMENSIONS: usize = 4;

/// `gas.Bandwidth` dimension index.
pub const BANDWIDTH: usize = 0;
/// `gas.DBRead` dimension index.
pub const DB_READ: usize = 1;
/// `gas.DBWrite` dimension index (includes deletes).
pub const DB_WRITE: usize = 2;
/// `gas.Compute` dimension index.
pub const COMPUTE: usize = 3;

/// `gas.Gas` — an amount of gas (newtype over `u64`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Gas(pub u64);

/// `gas.Price` — a gas price in nAVAX-per-gas (newtype over `u64`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Price(pub u64);

impl Gas {
    /// `Gas.Cost(price)` — converts gas to nAVAX (`gas * price`).
    ///
    /// # Errors
    /// Returns [`Error::Overflow`] if the product overflows `u64`.
    pub fn cost(self, price: Price) -> Result<u64> {
        safemath::mul(self.0, price.0).map_err(|_| Error::Overflow)
    }

    /// `Gas.AddOverTime(gasRate, duration)` — returns `g + gasRate * duration`,
    /// saturating at `u64::MAX` on overflow (Go returns `MaxUint64`).
    #[must_use]
    pub fn add_over_time(self, gas_rate: Gas, duration: u64) -> Gas {
        let added = match safemath::mul(gas_rate.0, duration) {
            Ok(v) => v,
            Err(_) => return Gas(u64::MAX),
        };
        match safemath::add(self.0, added) {
            Ok(v) => Gas(v),
            Err(_) => Gas(u64::MAX),
        }
    }

    /// `Gas.SubOverTime(gasRate, duration)` — returns `g - gasRate * duration`,
    /// saturating at `0` on underflow (Go returns `0`).
    #[must_use]
    pub fn sub_over_time(self, gas_rate: Gas, duration: u64) -> Gas {
        let to_remove = match safemath::mul(gas_rate.0, duration) {
            Ok(v) => v,
            Err(_) => return Gas(0),
        };
        match safemath::sub(self.0, to_remove) {
            Ok(v) => Gas(v),
            Err(_) => Gas(0),
        }
    }
}

/// `gas.State` — the dynamic-fee state (`capacity`, `excess`). Both fields are
/// `serialize:"true"` in Go.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct GasState {
    /// Available gas capacity for the current window.
    pub capacity: Gas,
    /// Accumulated excess gas, driving the price up via [`calculate_price`].
    pub excess: Gas,
}

impl GasState {
    /// `State.AdvanceTime(maxCapacity, capacityRate, targetRate, duration)` —
    /// refill capacity (capped at `max_capacity`) and decay excess over the
    /// duration.
    #[must_use]
    pub fn advance(
        self,
        max_capacity: Gas,
        capacity_rate: Gas,
        target_rate: Gas,
        duration: u64,
    ) -> GasState {
        GasState {
            capacity: self
                .capacity
                .add_over_time(capacity_rate, duration)
                .min(max_capacity),
            excess: self.excess.sub_over_time(target_rate, duration),
        }
    }

    /// `State.ConsumeGas(gas)` — remove `gas` from capacity and add it to excess.
    ///
    /// # Errors
    /// Returns [`Error::InsufficientCapacity`] if `gas > capacity`. The excess is
    /// saturated at `u64::MAX` rather than erroring (matching Go).
    pub fn consume(self, gas: Gas) -> Result<GasState> {
        let new_capacity =
            safemath::sub(self.capacity.0, gas.0).map_err(|_| Error::InsufficientCapacity)?;
        // Excess saturates at MaxUint64 rather than erroring.
        let new_excess = safemath::add(self.excess.0, gas.0).unwrap_or(u64::MAX);
        Ok(GasState {
            capacity: Gas(new_capacity),
            excess: Gas(new_excess),
        })
    }
}

/// `gas.Dimensions` — per-dimension gas usage (`[u64; NUM_DIMENSIONS]`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Dimensions(pub [u64; NUM_DIMENSIONS]);

impl Dimensions {
    /// `Dimensions.Add(os...)` — element-wise checked sum.
    ///
    /// # Errors
    /// Returns [`Error::Overflow`] on any element overflow.
    #[allow(clippy::should_implement_trait)] // mirrors Go's `Dimensions.Add`.
    pub fn add(self, others: &[Dimensions]) -> Result<Dimensions> {
        let mut d = self.0;
        for o in others {
            for (acc, &v) in d.iter_mut().zip(o.0.iter()) {
                *acc = safemath::add(*acc, v).map_err(|_| Error::Overflow)?;
            }
        }
        Ok(Dimensions(d))
    }

    /// `Dimensions.Sub(os...)` — element-wise checked difference.
    ///
    /// # Errors
    /// Returns [`Error::Underflow`] on any element underflow.
    #[allow(clippy::should_implement_trait)] // mirrors Go's `Dimensions.Sub`.
    pub fn sub(self, others: &[Dimensions]) -> Result<Dimensions> {
        let mut d = self.0;
        for o in others {
            for (acc, &v) in d.iter_mut().zip(o.0.iter()) {
                *acc = safemath::sub(*acc, v).map_err(|_| Error::Underflow)?;
            }
        }
        Ok(Dimensions(d))
    }

    /// `Dimensions.ToGas(weights)` — the weighted dot product `d · weights`.
    ///
    /// # Errors
    /// Returns [`Error::Overflow`] on overflow.
    pub fn to_gas(self, weights: Dimensions) -> Result<Gas> {
        let mut res: u64 = 0;
        for (&d, &w) in self.0.iter().zip(weights.0.iter()) {
            let v = safemath::mul(d, w).map_err(|_| Error::Overflow)?;
            res = safemath::add(res, v).map_err(|_| Error::Overflow)?;
        }
        Ok(Gas(res))
    }
}

/// `gas.CalculatePrice(minPrice, excess, excessConversionConstant)` — the gas
/// price, defined as an integer approximation of
/// `minPrice * e^(excess / excessConversionConstant)`.
///
/// Reproduces Go's EIP-4844 `fakeExponential` fixed-point loop **exactly** (no
/// floats):
///
/// ```text
/// i = 1; output = 0; numeratorAccum = minPrice * denominator
/// while numeratorAccum > 0:
///     output += numeratorAccum
///     if output >= denominator * MaxUint64: return MaxUint64
///     numeratorAccum = (numeratorAccum * numerator) / denominator / i
///     i += 1
/// return output / denominator
/// ```
///
/// where `numerator = excess`, `denominator = excessConversionConstant`.
/// Intermediate values can reach ~`MaxUint192`, so [`BigUint`] is used; the
/// result is a `u64` (capped at `MaxUint64`).
// `BigUint` is arbitrary-precision, so its `+`/`*`/`/` cannot overflow; the
// `arithmetic_side_effects` lint is about machine-int wrap, which does not apply.
#[allow(clippy::arithmetic_side_effects)]
#[must_use]
pub fn calculate_price(min_price: Price, excess: Gas, excess_conversion_constant: Gas) -> Price {
    let zero = BigUint::from(0u64);
    let numerator = BigUint::from(excess.0);
    let denominator = BigUint::from(excess_conversion_constant.0);

    // `denominator == 0` would divide-by-zero. Go relies on K being non-zero;
    // mirror that contract by returning `min_price` (no excess scaling possible).
    if denominator == zero {
        return min_price;
    }

    let max_output = &denominator * BigUint::from(u64::MAX);

    let mut i = BigUint::from(1u64);
    let mut output = zero.clone();
    let mut numerator_accum = BigUint::from(min_price.0) * &denominator;

    while numerator_accum > zero {
        output += &numerator_accum;
        if output >= max_output {
            return Price(u64::MAX);
        }
        numerator_accum = (&numerator_accum * &numerator) / &denominator / &i;
        i += 1u64;
    }

    let result = output / &denominator;
    // `result <= MaxUint64` is guaranteed by the `output >= max_output` guard.
    Price(result.try_into().unwrap_or(u64::MAX))
}
