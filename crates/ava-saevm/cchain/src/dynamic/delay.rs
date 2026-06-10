// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `dynamic::delay` — [`DelayExponent`] encoding the ACP-226 minimum block
//! delay in milliseconds.
//!
//! Port of Go `vms/saevm/cchain/dynamic/delay.go` (`2750cc9e42`).

use ava_vm::components::gas::{Gas, Price, calculate_price};

use super::math::{search, toward};

/// Encodes the minimum block delay in milliseconds.
///
/// Implements ACP-226, specified at:
/// <https://github.com/avalanche-foundation/ACPs/blob/main/ACPs/226-dynamic-minimum-block-times/README.md>
///
/// The decoded value is `minimum · e^(self / K)` where `minimum = 1` ms and
/// `K = 1 << 20`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct DelayExponent(pub u64);

/// Minimum delay in milliseconds. Port of Go `minimum = 1`.
const DELAY_MINIMUM: u64 = 1;

/// Conversion rate `K = 2^20`. Port of Go `conversionRate = 1 << 20`.
const DELAY_CONVERSION_RATE: u64 = 1 << 20;

/// Per-block maximum exponent change. Port of Go `maxDiff = 200`.
const DELAY_MAX_DIFF: u64 = 200;

/// Upper bound for binary search in [`desired_delay_exponent`].
///
/// Port of Go `maxExponent = 46_516_320`
/// (`conversionRate * ln(MaxUint64 / minimum) + 1`).
const DELAY_MAX_EXPONENT: u64 = 46_516_320;

impl DelayExponent {
    /// Returns the minimum block delay in milliseconds.
    ///
    /// `Delay = minimum · e^(self / conversionRate)`
    ///
    /// Port of Go `(DelayExponent).Delay() uint64`.
    #[must_use]
    pub fn delay(self) -> u64 {
        calculate_price(
            Price(DELAY_MINIMUM),
            Gas(self.0),
            Gas(DELAY_CONVERSION_RATE),
        )
        .0
    }

    /// Returns a new exponent moved at most one clamped step toward `desired`.
    ///
    /// If `desired` is `None`, returns `self` unchanged.
    ///
    /// Per ACP-226, the per-block exponent change is capped at `200`.
    ///
    /// Port of Go `(DelayExponent).Toward(desired *DelayExponent) DelayExponent`.
    #[must_use]
    pub fn toward(self, desired: Option<DelayExponent>) -> DelayExponent {
        DelayExponent(toward(self.0, desired.map(|d| d.0), DELAY_MAX_DIFF))
    }
}

/// Calculates the smallest [`DelayExponent`] whose [`DelayExponent::delay`]
/// value is `>= desired`.
///
/// Binary search avoids the rounding error of a floating-point solution.
///
/// Port of Go `DesiredDelayExponent(desired uint64) DelayExponent`.
#[must_use]
pub fn desired_delay_exponent(desired: u64) -> DelayExponent {
    DelayExponent(search(DELAY_MAX_EXPONENT, |guess| {
        DelayExponent(guess).delay() >= desired
    }))
}

#[cfg(test)]
mod tests {
    use super::{DelayExponent, desired_delay_exponent};

    /// Golden-table test — `(DelayExponent).delay()` reader values.
    ///
    /// Transcribed from Go `delay_test.go` `delayReaderCases` (`2750cc9e42`).
    #[test]
    fn dynamic_delay_reader_golden() {
        let cases: &[(u64, u64, &str)] = &[
            (0, 1, "zero"),
            (726_820, 2, "smallest_change"),
            (200, 1, "max_step"),
            (4_828_872, 100, "100ms"),
            (6_516_490, 500, "500ms"),
            (7_243_307, 1000, "1000ms"),
            (7_970_124, 2000, "2000ms"),
            (8_930_925, 5000, "5000ms"),
            (9_657_742, 10000, "10000ms"),
            (11_536_538, 60000, "60000ms"),
            (13_224_156, 300_000, "300000ms"),
            (45_789_502, 9_223_368_741_047_657_702, "largest_int64"),
            (
                46_516_319,
                18_446_728_723_565_431_225,
                "second_largest_uint64",
            ),
            (46_516_320, u64::MAX, "largest_uint64"),
            (u64::MAX, u64::MAX, "saturated"),
        ];
        for &(exponent, want, name) in cases {
            assert_eq!(
                DelayExponent(exponent).delay(),
                want,
                "case {name}: exponent={exponent}"
            );
        }
    }

    /// Golden-table test — [`desired_delay_exponent`] inversion round-trip.
    ///
    /// Transcribed from Go `delay_test.go` `delayReaderCases`, skipping
    /// `skipDesired = true` cases (zero, `max_step`, saturated).
    #[test]
    fn dynamic_desired_delay_exponent_golden() {
        let cases: &[(u64, u64, &str)] = &[
            (0, 1, "zero"),
            (726_820, 2, "smallest_change"),
            (4_828_872, 100, "100ms"),
            (6_516_490, 500, "500ms"),
            (7_243_307, 1000, "1000ms"),
            (7_970_124, 2000, "2000ms"),
            (8_930_925, 5000, "5000ms"),
            (9_657_742, 10000, "10000ms"),
            (11_536_538, 60000, "60000ms"),
            (13_224_156, 300_000, "300000ms"),
            (45_789_502, 9_223_368_741_047_657_702, "largest_int64"),
            (
                46_516_319,
                18_446_728_723_565_431_225,
                "second_largest_uint64",
            ),
            (46_516_320, u64::MAX, "largest_uint64"),
        ];
        for &(want_exp, value, name) in cases {
            assert_eq!(
                desired_delay_exponent(value),
                DelayExponent(want_exp),
                "case {name}: value={value}"
            );
        }
    }

    /// `Toward` clamped-step golden table.
    ///
    /// Transcribed from Go `delay_test.go` `delayTowardCases` (`2750cc9e42`).
    #[test]
    fn dynamic_delay_toward_golden() {
        let cases: &[(u64, Option<u64>, u64, &str)] = &[
            (1234, None, 1234, "nil_unchanged"),
            (0, Some(0), 0, "no_change"),
            (50, Some(100), 100, "increase_within_cap"),
            (100, Some(50), 50, "decrease_within_cap"),
            (0, Some(200), 200, "increase_at_cap"),
            (200, Some(0), 0, "decrease_at_cap"),
            (0, Some(1000), 200, "increase_capped"),
            (1000, Some(0), 800, "decrease_capped"),
        ];
        for &(current, desired, want, name) in cases {
            assert_eq!(
                DelayExponent(current).toward(desired.map(DelayExponent)),
                DelayExponent(want),
                "case {name}"
            );
        }
    }
}
