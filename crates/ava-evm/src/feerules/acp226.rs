// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! ACP-226 dynamic minimum block delay (spec 10 §7.1, §17.3). Mirrors
//! `vms/evm/acp226/acp226.go` bit-for-bit.
//!
//! [`DelayExcess`] is a newtype over `u64` representing the "delay excess"
//! that drives the exponential minimum delay calculation. The delay is:
//!
//! ```text
//! Delay = MinDelayMilliseconds * e^(DelayExcess / ConversionRate)
//!       = CalculatePrice(M=1, excess, D=1<<20)
//! ```
//!
//! The excess can be moved up or down by at most `MaxDelayExcessDiff` = 200 per
//! block. `DesiredDelayExcess` binary-searches for the least `q` with
//! `Delay(q) >= desiredDelay`.
//!
//! All arithmetic is checked/saturating — no floats (spec 00 §6.1).

use super::{Gas, Price, calculate_price};

// ─── ACP-226 constants (`vms/evm/acp226/acp226.go`) ─────────────────────────

/// `MinDelayMilliseconds` (M) — minimum block delay in milliseconds.
pub const MIN_DELAY_MILLISECONDS: u64 = 1;
/// `ConversionRate` (D) = `1 << 20` = 1_048_576.
pub const CONVERSION_RATE: u64 = 1 << 20;
/// `MaxDelayExcessDiff` (Q) — maximum change in excess per block update.
pub const MAX_DELAY_EXCESS_DIFF: u64 = 200;

/// `InitialDelayExcess` — initial excess representing ~2000ms delay.
/// Formula: `ConversionRate * ln(2000) + 1`.
pub const INITIAL_DELAY_EXCESS: DelayExcess = DelayExcess(7_970_124);

/// `maxDelayExcess` = `ConversionRate * ln(MaxUint64 / MinDelayMilliseconds) + 1`.
/// Upper bound for the binary search in [`desired_delay_excess`].
const MAX_DELAY_EXCESS: u64 = 46_516_320;

// ─── DelayExcess ─────────────────────────────────────────────────────────────

/// `acp226.DelayExcess` — the excess driving the minimum block delay.
///
/// `Delay() = CalculatePrice(M=1, excess, D=1<<20)` in milliseconds.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct DelayExcess(pub u64);

impl DelayExcess {
    /// `Delay()` — minimum block delay in milliseconds.
    ///
    /// `= CalculatePrice(MinDelayMilliseconds=1, self, ConversionRate=1<<20)`
    #[must_use]
    pub fn delay(self) -> u64 {
        calculate_price(
            Price(MIN_DELAY_MILLISECONDS),
            Gas(self.0),
            Gas(CONVERSION_RATE),
        )
        .0
    }

    /// `UpdateDelayExcess(desired)` — move this excess toward `desired` by at
    /// most `MaxDelayExcessDiff` = 200.
    pub fn update(&mut self, desired: DelayExcess) {
        *self = calculate_delay_excess(*self, desired);
    }
}

// ─── DesiredDelayExcess ───────────────────────────────────────────────────────

/// `DesiredDelayExcess(desiredDelay)` — binary search over `[0, maxDelayExcess)`
/// for the least excess `q` with `Delay(q) >= desiredDelay`.
#[must_use]
pub fn desired_delay_excess(desired_delay: u64) -> DelayExcess {
    let mut lo: u64 = 0;
    let mut hi: u64 = MAX_DELAY_EXCESS;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if DelayExcess(mid).delay() >= desired_delay {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    DelayExcess(lo)
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// `calculateDelayExcess(excess, desired)` — move `excess` toward `desired`
/// by at most `MaxDelayExcessDiff`.
fn calculate_delay_excess(excess: DelayExcess, desired: DelayExcess) -> DelayExcess {
    let change = excess.0.abs_diff(desired.0).min(MAX_DELAY_EXCESS_DIFF);
    if excess.0 < desired.0 {
        DelayExcess(excess.0 + change)
    } else {
        DelayExcess(excess.0 - change)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Delay golden vectors from Go readerTests ───────────────────────────────

    #[test]
    fn delay_golden_table() {
        // From vms/evm/acp226/acp226_test.go readerTests.
        // Format: (excess, expected_delay_ms)
        let cases: &[(u64, u64)] = &[
            (0, MIN_DELAY_MILLISECONDS),                        // zero
            (726_820, MIN_DELAY_MILLISECONDS + 1),              // small_excess_change
            (4_828_872, 100),                                   // 100ms_delay
            (6_516_490, 500),                                   // 500ms_delay
            (7_243_307, 1000),                                  // 1000ms_delay
            (INITIAL_DELAY_EXCESS.0, 2000),                     // 2000ms_delay_initial
            (8_930_925, 5000),                                  // 5000ms_delay
            (9_657_742, 10_000),                                // 10000ms_delay
            (11_536_538, 60_000),                               // 60000ms_delay
            (13_224_156, 300_000),                              // 300000ms_delay
            (45_789_502, 9_223_368_741_047_657_702),            // largest_int64_delay
            (MAX_DELAY_EXCESS - 1, 18_446_728_723_565_431_225), // second_largest_uint64_delay
            (MAX_DELAY_EXCESS, u64::MAX),                       // largest_uint64_delay
            (u64::MAX, u64::MAX),                               // largest_excess_delay
        ];

        for &(excess, expected_delay) in cases {
            let got = DelayExcess(excess).delay();
            assert_eq!(
                got, expected_delay,
                "delay mismatch for excess={excess}: got {got}, want {expected_delay}"
            );
        }
    }

    // ── UpdateDelayExcess golden vectors ──────────────────────────────────────

    #[test]
    fn update_delay_excess_golden_table() {
        // From vms/evm/acp226/acp226_test.go updateExcessTests.
        // Format: (initial, desired, expected_after_update)
        let cases: &[(u64, u64, u64)] = &[
            (0, 0, 0),                                             // no_change
            (0, MAX_DELAY_EXCESS_DIFF + 1, MAX_DELAY_EXCESS_DIFF), // max_increase
            (MAX_DELAY_EXCESS_DIFF, 0, 0),                         // inverse_max_increase
            (2 * MAX_DELAY_EXCESS_DIFF, 0, MAX_DELAY_EXCESS_DIFF), // max_decrease
            (
                MAX_DELAY_EXCESS_DIFF,
                2 * MAX_DELAY_EXCESS_DIFF,
                2 * MAX_DELAY_EXCESS_DIFF,
            ), // inverse_max_decrease
            (50, 100, 100),                                        // small_increase
            (100, 50, 50),                                         // small_decrease
            (0, 1000, MAX_DELAY_EXCESS_DIFF),                      // large_increase_capped
            (1000, 0, 1000 - MAX_DELAY_EXCESS_DIFF),               // large_decrease_capped
        ];

        for &(initial, desired, expected) in cases {
            let mut e = DelayExcess(initial);
            e.update(DelayExcess(desired));
            assert_eq!(
                e.0, expected,
                "update mismatch: initial={initial} desired={desired}"
            );
        }
    }

    // ── DesiredDelayExcess binary search round-trip ────────────────────────────

    #[test]
    fn desired_delay_excess_round_trip() {
        // From Go readerTests (non-skip entries): desired_delay_excess(delay) == excess.
        let cases: &[(u64, u64)] = &[
            (0, MIN_DELAY_MILLISECONDS),
            (726_820, MIN_DELAY_MILLISECONDS + 1),
            (4_828_872, 100),
            (6_516_490, 500),
            (7_243_307, 1000),
            (INITIAL_DELAY_EXCESS.0, 2000),
            (8_930_925, 5000),
            (9_657_742, 10_000),
            (11_536_538, 60_000),
            (13_224_156, 300_000),
            (45_789_502, 9_223_368_741_047_657_702),
            (MAX_DELAY_EXCESS - 1, 18_446_728_723_565_431_225),
            (MAX_DELAY_EXCESS, u64::MAX),
        ];
        for &(expected_excess, desired_delay) in cases {
            let got = desired_delay_excess(desired_delay);
            assert_eq!(
                got.0, expected_excess,
                "desired_delay_excess({desired_delay}): got {}, want {expected_excess}",
                got.0
            );
        }
    }

    // ── Edge case: max_initial_excess_change (excess=Q=200) ───────────────────

    #[test]
    fn delay_at_max_initial_excess_change() {
        // Go test: excess=MaxDelayExcessDiff, delay=1 (MinDelayMilliseconds)
        assert_eq!(
            DelayExcess(MAX_DELAY_EXCESS_DIFF).delay(),
            MIN_DELAY_MILLISECONDS
        );
    }
}
