// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared gas primitives — the ACP-103 exponential price (`calculate_price`),
//! the on-chain gas meter (`GasState`), and the complexity→gas dot product
//! (`dot_to_gas`).
//!
//! Port of Go `vms/components/gas/{gas,state,dimensions}.go`. This is the single
//! most consensus-critical arithmetic in the fee subsystem; the algorithm and
//! op-order are reproduced bit-exactly per `specs/21-fee-economics-math.md` §0
//! (`CalculatePrice`) and §1 (the ACP-103 dynamic gas meter).
//!
//! All arithmetic is checked integer arithmetic on fixed-width integers
//! (`u64` / `U256`); no floating point ever touches a value that goes into a
//! block, a state root, or a fee charge (specs `00` §6.1).

use ruint::aliases::U256;

use crate::error::{Error, Result};

/// The number of resource dimensions tracked per transaction.
///
/// `[Bandwidth, DBRead, DBWrite, Compute]` (`gas.NumDimensions`).
pub const NUM_DIMENSIONS: usize = 4;

/// Index of the bandwidth dimension (`gas.Bandwidth`).
pub const BANDWIDTH: usize = 0;
/// Index of the DB-read dimension (`gas.DBRead`).
pub const DB_READ: usize = 1;
/// Index of the DB-write dimension (`gas.DBWrite`), which includes deletes.
pub const DB_WRITE: usize = 2;
/// Index of the compute dimension (`gas.Compute`).
pub const COMPUTE: usize = 3;

/// A transaction's resource use as a 4-vector `[Bandwidth, DBRead, DBWrite,
/// Compute]` (`gas.Dimensions`).
pub type Dimensions = [u64; NUM_DIMENSIONS];

/// Returns `complexity · weights`, the scalar gas of a complexity vector
/// (`gas.Dimensions.ToGas`).
///
/// Computed as a checked dot product `Σ_d complexity[d] · weights[d]` in the
/// fixed dimension order; any overflow yields [`Error::FeeOverflow`].
///
/// # Errors
///
/// Returns [`Error::FeeOverflow`] if any per-dimension product or the running
/// sum overflows `u64`.
pub fn dot_to_gas(complexity: Dimensions, weights: Dimensions) -> Result<u64> {
    let mut res: u64 = 0;
    for (&c, &w) in complexity.iter().zip(weights.iter()) {
        let term = c.checked_mul(w).ok_or(Error::FeeOverflow)?;
        res = res.checked_add(term).ok_or(Error::FeeOverflow)?;
    }
    Ok(res)
}

/// Returns the gas price as an integer approximation of
/// `min_price · e^(excess / k)` (`gas.CalculatePrice`, specs 21 §0).
///
/// This is the EIP-4844 `fake_exponential`, ported with the exact integer
/// op-order of the Go implementation. The intermediate values are bounded to
/// `MaxUint193`, so a 256-bit accumulator is sufficient and never itself
/// overflows. Any value greater than `u64::MAX` is clamped to `u64::MAX`.
///
/// Precision traps replicated verbatim from Go:
/// 1. The clamp test is `output >= max_output` (`>=`, not `>`), checked
///    *before* the next accumulator update, returning `u64::MAX`.
/// 2. The two divides are **separate**: `/k` then `/i` (NOT `/(k·i)`).
/// 3. The final result is `output / k` (the trailing `÷denominator`).
/// 4. `min_price == 0` ⇒ the accumulator starts at 0 ⇒ returns 0.
///
/// `k` (the excess conversion constant) must be `>= 1`; callers guarantee this.
/// A `k == 0` would divide by zero, so it is treated as `1` to stay panic-free.
#[must_use]
pub fn calculate_price(min_price: u64, excess: u64, k: u64) -> u64 {
    // Callers guarantee k >= 1 (specs 21 §0 trap 4). Guard to stay panic-free
    // and arithmetic_side_effects-clean rather than divide by zero.
    let k = k.max(1);

    let numerator = U256::from(excess);
    let denominator = U256::from(k);
    let max_u64 = U256::from(u64::MAX);

    let mut i = U256::from(1u64);
    let mut output = U256::ZERO;
    // accum = min_price * k  (range [0, MaxUint128]).
    let mut accum = U256::from(min_price).wrapping_mul(denominator);
    // max_output = k * MaxUint64  (range [0, MaxUint128]); clamp threshold.
    let max_output = denominator.wrapping_mul(max_u64);

    while accum > U256::ZERO {
        // output += accum  (range [0, MaxUint193]).
        output = output.wrapping_add(accum);
        if output >= max_output {
            return u64::MAX; // clamp (>=, not >)
        }
        // accum = accum * x / k / i  (/k THEN /i, as two separate divides).
        accum = accum.wrapping_mul(numerator);
        accum = accum.wrapping_div(denominator);
        accum = accum.wrapping_div(i);

        i = i.wrapping_add(U256::from(1u64));
    }
    // output < max_output = k·MaxUint64 ⇒ output/k <= MaxUint64, so this fits.
    let result = output.wrapping_div(denominator);
    result.to::<u64>()
}

/// The on-chain gas meter (`gas.State`): carried in P-Chain state and updated
/// every block (specs 21 §1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GasState {
    /// Remaining gas capacity in the current block window.
    pub capacity: u64,
    /// Accumulated excess gas, the input to the exponential price.
    pub excess: u64,
}

impl GasState {
    /// Returns `g + rate · duration`, saturating to `u64::MAX` on overflow
    /// (`gas.Gas.AddOverTime`).
    fn add_over_time(g: u64, rate: u64, duration: u64) -> u64 {
        match rate.checked_mul(duration) {
            Some(delta) => g.saturating_add(delta),
            None => u64::MAX,
        }
    }

    /// Returns `g − rate · duration`, flooring at `0` on underflow
    /// (`gas.Gas.SubOverTime`).
    fn sub_over_time(g: u64, rate: u64, duration: u64) -> u64 {
        match rate.checked_mul(duration) {
            Some(delta) => g.saturating_sub(delta),
            None => 0,
        }
    }

    /// Adds `capacity_rate` to capacity and subtracts `target_rate` from excess
    /// over `duration` (`gas.State.AdvanceTime`).
    ///
    /// Capacity is capped at `max_capacity`; excess is floored at `0`.
    #[must_use]
    pub fn advance_time(
        self,
        max_capacity: u64,
        capacity_rate: u64,
        target_rate: u64,
        duration: u64,
    ) -> GasState {
        GasState {
            capacity: Self::add_over_time(self.capacity, capacity_rate, duration).min(max_capacity),
            excess: Self::sub_over_time(self.excess, target_rate, duration),
        }
    }

    /// Removes `gas` from capacity and adds `gas` to excess
    /// (`gas.State.ConsumeGas`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InsufficientCapacity`] if `gas > capacity`. If the
    /// excess would overflow it is capped at `u64::MAX` (no error).
    pub fn consume_gas(self, gas: u64) -> Result<GasState> {
        let capacity = self
            .capacity
            .checked_sub(gas)
            .ok_or(Error::InsufficientCapacity)?;
        Ok(GasState {
            capacity,
            excess: self.excess.saturating_add(gas), // capped at u64::MAX
        })
    }
}

#[cfg(test)]
mod golden {
    use super::*;
    use crate::txs::fee::dynamic_calculator::{
        K, MAX_CAPACITY, MAX_PER_SECOND, MIN_PRICE, TARGET_PER_SECOND, WEIGHTS,
    };

    /// `gas.CalculatePrice` — the 9-row table from specs 21 §0 (verbatim from
    /// `vms/components/gas/gas_test.go`), incl. the `MaxUint64 − 11` row that
    /// pins the truncation order and the `MaxUint64` clamp row.
    #[test]
    fn calculate_price() {
        // (min_price, excess, k, expected)
        let cases: &[(u64, u64, u64, u64)] = &[
            (1, 0, 1, 1),
            (1, 1, 1, 2),
            (1, 2, 1, 6),
            (1, 10_000, 10_000, 2),
            (1, 1_000_000, 10_000, u64::MAX), // clamped
            (10, 10_000_000, 1_000_000, 220_264),
            (u64::MAX, u64::MAX, 1, u64::MAX),
            (u64::from(u32::MAX), 1, 1, 11_674_931_546),
            // ≈ MaxUint64/e ⇒ MaxUint64 − 11 (= 18_446_744_073_709_551_604).
            (6_786_177_901_268_885_274, 1, 1, 18_446_744_073_709_551_604),
        ];
        for &(m, x, k, want) in cases {
            assert_eq!(
                super::calculate_price(m, x, k),
                want,
                "calculate_price({m}, {x}, {k})"
            );
        }
    }

    /// ACP-103 dynamic-fee worked examples from specs 21 §1: the price at
    /// `excess = 0` and `excess = K`, the corresponding fees for a
    /// `[600, 1, 1, 1000]` tx, and the advance-then-consume round trip.
    #[test]
    fn pchain_dynamic_fee() {
        // Example 1: price at excess = 0 is min_price = 1; tx gas = 6600.
        let price0 = super::calculate_price(MIN_PRICE, 0, K);
        assert_eq!(price0, 1);
        let complexity: Dimensions = [600, 1, 1, 1000];
        let gas = super::dot_to_gas(complexity, WEIGHTS).expect("no overflow");
        assert_eq!(gas, 6_600);
        assert_eq!(gas.checked_mul(price0).expect("no overflow"), 6_600);

        // Example 2: price after one doubling (excess = K) is 2; fee = 13200.
        let price_k = super::calculate_price(MIN_PRICE, K, K);
        assert_eq!(price_k, 2);
        assert_eq!(gas.checked_mul(price_k).expect("no overflow"), 13_200);

        // Example 3: advance-then-consume round trip.
        let state = GasState {
            capacity: MAX_CAPACITY,
            excess: 100_000,
        };
        let advanced = state.advance_time(MAX_CAPACITY, MAX_PER_SECOND, TARGET_PER_SECOND, 1);
        assert_eq!(
            advanced,
            GasState {
                capacity: 1_000_000, // min(1_000_000 + 100_000, 1_000_000)
                excess: 50_000,      // 100_000 - 50_000
            }
        );
        let consumed = advanced.consume_gas(6_600).expect("sufficient capacity");
        assert_eq!(
            consumed,
            GasState {
                capacity: 993_400,
                excess: 56_600,
            }
        );
    }
}

#[cfg(test)]
mod prop {
    use proptest::prelude::*;

    proptest! {
        /// `calculate_price` is monotone non-decreasing in `excess`, equals
        /// `min_price` at `excess = 0`, never panics, and is `<= u64::MAX`
        /// (specs 21 §9).
        #[test]
        fn price_monotone(
            min_price in 0u64..=u64::MAX,
            x0 in 0u64..=u64::MAX,
            dx in 0u64..=u64::MAX,
            k in 1u64..=u64::MAX,
        ) {
            // Equals min_price at excess = 0.
            prop_assert_eq!(super::calculate_price(min_price, 0, k), min_price);

            let x1 = x0.saturating_add(dx);
            let p0 = super::calculate_price(min_price, x0, k);
            let p1 = super::calculate_price(min_price, x1, k);
            // Non-decreasing in excess. (<= u64::MAX is implied by the type.)
            prop_assert!(p1 >= p0);
        }
    }
}
