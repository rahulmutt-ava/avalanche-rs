// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `dynamic::price` — [`PriceExponent`] encoding the ACP-283 minimum gas
//! price in wei (aAVAX).
//!
//! Port of Go `vms/saevm/cchain/dynamic/price.go` (`2750cc9e42`).

use ava_vm::components::gas::{Gas, Price, calculate_price};

use super::math::{search, toward};

/// Encodes the minimum gas price in wei (aAVAX).
///
/// Implements ACP-283, specified at:
/// <https://github.com/avalanche-foundation/ACPs/blob/main/ACPs/283-dynamic-minimum-gas-price/README.md>
///
/// The decoded value is `minimum · e^(self / K)` where `minimum = 1` wei and
/// `K = 415_828_534_307_635_077` (`MaxUint64 / ln(MaxUint64)`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct PriceExponent(pub u64);

/// Minimum gas price in wei. Port of Go `minimum gas.Price = 1`.
const PRICE_MINIMUM: u64 = 1;

/// Conversion rate `K = MaxUint64 / ln(MaxUint64)`.
///
/// Port of Go `conversionRate gas.Gas = 415_828_534_307_635_077`.
const PRICE_CONVERSION_RATE: u64 = 415_828_534_307_635_077;

/// `conversionRate * ln(2)` — the exponent delta that doubles the price.
///
/// Port of Go `diffToDouble = 288_230_376_151_711_744`.
const DIFF_TO_DOUBLE: u64 = 288_230_376_151_711_744;

/// Number of blocks in which the price can at most double.
///
/// Port of Go `blocksToDouble = 3_600`.
const BLOCKS_TO_DOUBLE: u64 = 3_600;

/// Per-block maximum exponent change: `diffToDouble / blocksToDouble`.
///
/// Port of Go `maxDiff = diffToDouble / blocksToDouble` (= `80_063_993_375_475`).
const PRICE_MAX_DIFF: u64 = DIFF_TO_DOUBLE / BLOCKS_TO_DOUBLE;

/// Upper bound for binary search in [`desired_price_exponent`].
///
/// Port of Go `maxExponent = math.MaxUint64 - 37`.
const PRICE_MAX_EXPONENT: u64 = u64::MAX - 37;

impl PriceExponent {
    /// Returns the minimum gas price in wei decoded from this exponent.
    ///
    /// `Price = minimum · e^(self / conversionRate)`
    ///
    /// Port of Go `(PriceExponent).Price() gas.Price`.
    #[must_use]
    pub fn price(self) -> Price {
        calculate_price(
            Price(PRICE_MINIMUM),
            Gas(self.0),
            Gas(PRICE_CONVERSION_RATE),
        )
    }

    /// Returns a new exponent moved at most one clamped step toward `desired`.
    ///
    /// If `desired` is `None`, returns `self` unchanged.
    ///
    /// The per-block cap is `diffToDouble / blocksToDouble` (≈ 80 trillion),
    /// meaning the price can at most double over 3 600 consecutive blocks.
    ///
    /// Port of Go `(PriceExponent).Toward(desired *PriceExponent) PriceExponent`.
    #[must_use]
    pub fn toward(self, desired: Option<PriceExponent>) -> PriceExponent {
        PriceExponent(toward(self.0, desired.map(|d| d.0), PRICE_MAX_DIFF))
    }
}

/// Calculates the smallest [`PriceExponent`] whose [`PriceExponent::price`]
/// value is `>= desired`.
///
/// Binary search avoids the rounding error of a floating-point solution.
///
/// Port of Go `DesiredPriceExponent(desired gas.Price) PriceExponent`.
#[must_use]
pub fn desired_price_exponent(desired: Price) -> PriceExponent {
    PriceExponent(search(PRICE_MAX_EXPONENT, |guess| {
        PriceExponent(guess).price() >= desired
    }))
}

#[cfg(test)]
mod tests {
    use ava_vm::components::gas::Price;

    use super::{DIFF_TO_DOUBLE, PRICE_MAX_DIFF, PriceExponent, desired_price_exponent};

    /// Per-block cap constant, verified against Go's `priceMaxDiff`.
    ///
    /// Go `price_test.go`: `const priceMaxDiff = 80_063_993_375_475`.
    #[test]
    fn dynamic_price_max_diff_constant() {
        assert_eq!(PRICE_MAX_DIFF, 80_063_993_375_475);
        // Sanity-check the derivation.
        assert_eq!(DIFF_TO_DOUBLE / 3_600, 80_063_993_375_475);
    }

    /// Golden-table test — `(PriceExponent).price()` reader values.
    ///
    /// Transcribed from Go `price_test.go` `priceReaderCases` (`2750cc9e42`).
    #[test]
    fn dynamic_price_reader_golden() {
        let cases: &[(u64, u64, &str)] = &[
            (0, 1, "minimum"),
            (1, 1, "shared_minimum"),
            (288_230_376_151_711_749, 2, "double"),
            (576_460_752_303_423_490, 4, "quadruple"),
            (864_691_128_455_135_233, 8, "octuple"),
            (1_914_961_168_676_647_261, 100, "hundred"),
            (5_744_883_506_029_941_780, 1_000_000, "million"),
            (11_529_215_046_068_469_736, 1u64 << 40, "2^40"),
            (u64::MAX - 37, u64::MAX, "largest_uint64"),
            (u64::MAX, u64::MAX, "saturated"),
        ];
        for &(exponent, want, name) in cases {
            assert_eq!(
                PriceExponent(exponent).price(),
                Price(want),
                "case {name}: exponent={exponent}"
            );
        }
    }

    /// Golden-table test — [`desired_price_exponent`] inversion round-trip.
    ///
    /// Transcribed from Go `price_test.go` `priceReaderCases`, skipping
    /// `skipDesired = true` cases (`shared_minimum`, saturated).
    #[test]
    fn dynamic_desired_price_exponent_golden() {
        let cases: &[(u64, u64, &str)] = &[
            (0, 1, "minimum"),
            (288_230_376_151_711_749, 2, "double"),
            (576_460_752_303_423_490, 4, "quadruple"),
            (864_691_128_455_135_233, 8, "octuple"),
            (1_914_961_168_676_647_261, 100, "hundred"),
            (5_744_883_506_029_941_780, 1_000_000, "million"),
            (11_529_215_046_068_469_736, 1u64 << 40, "2^40"),
            (u64::MAX - 37, u64::MAX, "largest_uint64"),
        ];
        for &(want_exp, value, name) in cases {
            assert_eq!(
                desired_price_exponent(Price(value)),
                PriceExponent(want_exp),
                "case {name}: value={value}"
            );
        }
    }

    /// `Toward` clamped-step golden table.
    ///
    /// Transcribed from Go `price_test.go` `priceTowardCases` (`2750cc9e42`).
    #[test]
    fn dynamic_price_toward_golden() {
        let max_diff = PRICE_MAX_DIFF;
        let cases: &[(u64, Option<u64>, u64, &str)] = &[
            (1u64 << 40, None, 1u64 << 40, "nil_unchanged"),
            (0, Some(0), 0, "no_change"),
            (1000, Some(2000), 2000, "increase_within_cap"),
            (2000, Some(1000), 1000, "decrease_within_cap"),
            (0, Some(max_diff), max_diff, "increase_at_cap"),
            (max_diff, Some(0), 0, "decrease_at_cap"),
            (0, Some(max_diff + 1), max_diff, "increase_capped"),
            (2 * max_diff, Some(0), max_diff, "decrease_capped"),
        ];
        for &(current, desired, want, name) in cases {
            assert_eq!(
                PriceExponent(current).toward(desired.map(PriceExponent)),
                PriceExponent(want),
                "case {name}"
            );
        }
    }
}
