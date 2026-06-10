// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `dynamic::target` — [`TargetExponent`] encoding the ACP-176 target gas/s.
//!
//! Port of Go `vms/saevm/cchain/dynamic/target.go` (`2750cc9e42`).

use ava_vm::components::gas::{Gas, calculate_price};

use super::math::{search, toward};

/// Encodes the target gas per second.
///
/// Implements ACP-176, specified at:
/// <https://github.com/avalanche-foundation/ACPs/blob/main/ACPs/176-dynamic-evm-gas-limit-and-price-discovery-updates/README.md>
///
/// The decoded value is `minimum · e^(self / K)` where `minimum = 1_000_000`
/// gas and `K = 1 << 25`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct TargetExponent(pub u64);

/// Minimum target gas per second (gas). Port of Go `minimum = 1_000_000`.
const TARGET_MINIMUM: u64 = 1_000_000;

/// Conversion rate `K = 2^25`. Port of Go `conversionRate = 1 << 25`.
const TARGET_CONVERSION_RATE: u64 = 1 << 25;

/// Per-block maximum exponent change. Port of Go `maxDiff = 1 << 15`.
const TARGET_MAX_DIFF: u64 = 1 << 15;

/// Upper bound for the binary search in [`desired_target_exponent`].
///
/// Port of Go `maxExponent = 1_024_950_627`
/// (`conversionRate * ln(MaxUint64 / minimum) + 1`).
const TARGET_MAX_EXPONENT: u64 = 1_024_950_627;

impl TargetExponent {
    /// Returns the target gas per second decoded from this exponent.
    ///
    /// `Target = minimum · e^(self / conversionRate)`
    ///
    /// Port of Go `(TargetExponent).Target() gas.Gas`.
    #[must_use]
    pub fn target(self) -> Gas {
        Gas(calculate_price(
            ava_vm::components::gas::Price(TARGET_MINIMUM),
            Gas(self.0),
            Gas(TARGET_CONVERSION_RATE),
        )
        .0)
    }

    /// Returns a new exponent moved at most one clamped step toward `desired`.
    ///
    /// If `desired` is `None`, returns `self` unchanged.
    ///
    /// Per ACP-176, the per-block exponent change is capped at `1 << 15`.
    ///
    /// Port of Go `(TargetExponent).Toward(desired *TargetExponent) TargetExponent`.
    #[must_use]
    pub fn toward(self, desired: Option<TargetExponent>) -> TargetExponent {
        TargetExponent(toward(self.0, desired.map(|d| d.0), TARGET_MAX_DIFF))
    }
}

/// Calculates the smallest [`TargetExponent`] whose [`TargetExponent::target`]
/// value is `>= desired`.
///
/// Binary search avoids the rounding error of a floating-point solution.
///
/// Port of Go `DesiredTargetExponent(desired gas.Gas) TargetExponent`.
#[must_use]
pub fn desired_target_exponent(desired: Gas) -> TargetExponent {
    TargetExponent(search(TARGET_MAX_EXPONENT, |guess| {
        TargetExponent(guess).target() >= desired
    }))
}

#[cfg(test)]
mod tests {
    use ava_vm::components::gas::Gas;

    use super::{TargetExponent, desired_target_exponent};

    /// Golden-table test — `(TargetExponent).target()` reader values.
    ///
    /// Transcribed from Go `target_test.go` `targetReaderCases` (`2750cc9e42`).
    #[test]
    #[allow(clippy::too_many_lines)]
    fn dynamic_target_reader_golden() {
        let cases: &[(u64, u64, &str)] = &[
            (0, 1_000_000, "zero"),
            (33, 1_000_000, "largest_unchanged"),
            (34, 1_000_001, "smallest_change"),
            (1 << 15, 1_000_977, "max_step"),
            (13_605_152, 1_500_000, "1.5m"),
            (36_863_312, 3_000_000, "3m"),
            (60_121_472, 6_000_000, "6m"),
            (77_261_935, 10_000_000, "10m"),
            (154_523_870, 100_000_000, "100m"),
            (231_785_804, 1_000_000_000 - 24, "low_1b"),
            (231_785_805, 1_000_000_000 + 6, "high_1b"),
            (947_688_691, 1_844_674_384_269_701_322, "largest_capacity"),
            (1_001_692_466, 9_223_371_923_824_614_091, "largest_int64"),
            (
                1_024_950_626,
                18_446_743_882_783_898_031,
                "second_largest_uint64",
            ),
            (1_024_950_627, u64::MAX, "largest_uint64"),
            (u64::MAX, u64::MAX, "saturated"),
        ];
        for &(exponent, want, name) in cases {
            assert_eq!(
                TargetExponent(exponent).target(),
                Gas(want),
                "case {name}: exponent={exponent}"
            );
        }
    }

    /// Golden-table test — [`desired_target_exponent`] inversion round-trip.
    ///
    /// Transcribed from Go `target_test.go` `targetReaderCases`, skipping
    /// `skipDesired = true` cases.
    #[test]
    fn dynamic_desired_target_exponent_golden() {
        let cases: &[(u64, u64, &str)] = &[
            (0, 1_000_000, "zero"),
            (34, 1_000_001, "smallest_change"),
            (13_605_152, 1_500_000, "1.5m"),
            (36_863_312, 3_000_000, "3m"),
            (60_121_472, 6_000_000, "6m"),
            (77_261_935, 10_000_000, "10m"),
            (154_523_870, 100_000_000, "100m"),
            (231_785_804, 1_000_000_000 - 24, "low_1b"),
            (231_785_805, 1_000_000_000 + 6, "high_1b"),
            (947_688_691, 1_844_674_384_269_701_322, "largest_capacity"),
            (1_001_692_466, 9_223_371_923_824_614_091, "largest_int64"),
            (
                1_024_950_626,
                18_446_743_882_783_898_031,
                "second_largest_uint64",
            ),
            (1_024_950_627, u64::MAX, "largest_uint64"),
        ];
        for &(want_exp, value, name) in cases {
            assert_eq!(
                desired_target_exponent(Gas(value)),
                TargetExponent(want_exp),
                "case {name}: value={value}"
            );
        }
    }

    /// `Toward` clamped-step golden table.
    ///
    /// Transcribed from Go `target_test.go` `targetTowardCases` (`2750cc9e42`).
    #[test]
    fn dynamic_target_toward_golden() {
        let cases: &[(u64, Option<u64>, u64, &str)] = &[
            (13_605_152, None, 13_605_152, "nil_unchanged"),
            (0, Some(0), 0, "no_change"),
            (1000, Some(2000), 2000, "increase_within_cap"),
            (2000, Some(1000), 1000, "decrease_within_cap"),
            (0, Some(1 << 15), 1 << 15, "increase_at_cap"),
            (1 << 15, Some(0), 0, "decrease_at_cap"),
            (0, Some((1 << 15) + 1), 1 << 15, "increase_capped"),
            (1_000_000, Some(0), 1_000_000 - (1 << 15), "decrease_capped"),
        ];
        for &(current, desired, want, name) in cases {
            assert_eq!(
                TargetExponent(current).toward(desired.map(TargetExponent)),
                TargetExponent(want),
                "case {name}"
            );
        }
    }
}
