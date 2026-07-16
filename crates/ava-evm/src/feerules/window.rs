// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Apricot Phase 3 rolling gas window + base-fee update (spec 21 §4a, spec 10
//! §7.1). Mirrors coreth `plugin/evm/upgrade/ap3/window.go` and
//! `plugin/evm/customheader/dynamic_fee_windower.go` (`baseFeeFromWindow`)
//! bit-for-bit.
//!
//! The window is a 10-second rolling buffer of gas consumed; the base fee moves
//! up or down based on how the window sum compares to a per-phase target. All
//! arithmetic is checked/saturating over `u64` and [`U256`] — no floating point
//! (spec 00 §6.1).

use ruint::aliases::U256;

/// `ap3.WindowLen` — the rolling window length in seconds (= number of slots).
pub const WINDOW_LEN: u64 = 10;

/// `ap3.WindowSize` — the byte length of a serialized window (`WindowLen × 8`).
pub const WINDOW_SIZE: usize = WINDOW_LEN as usize * 8;

/// `ap3.IntrinsicBlockGas` — gas always folded into the AP3 fee window for the
/// parent block (coreth `plugin/evm/upgrade/ap3/window.go:49`).
pub const INTRINSIC_BLOCK_GAS: u64 = 1_000_000;

/// `ap3.Window` — a 10-second rolling window of gas consumed.
///
/// Slot `9` (the last) is the "current second"; [`Window::shift`] ages the
/// buffer by dropping the oldest slots off the front.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Window(pub [u64; 10]);

impl Window {
    /// `Window.Add(amounts…)` — saturating-add each amount into the **last**
    /// slot (overflow saturates to `u64::MAX`; Go uses `math.SaturatingAdd`).
    pub fn add(&mut self, amounts: &[u64]) {
        let last = &mut self.0[9];
        for &a in amounts {
            *last = last.saturating_add(a);
        }
    }

    /// `Window.Shift(n)` — drop the oldest `n` slots and zero-fill the front.
    ///
    /// `n >= 10` clears the whole window (Go returns the zero window).
    pub fn shift(&mut self, n: u64) {
        let n = match usize::try_from(n) {
            Ok(n) if n < 10 => n,
            // `n >= 10` (or out of `usize` range) clears the window.
            _ => {
                *self = Window::default();
                return;
            }
        };
        let mut w = [0u64; 10];
        // Copy the surviving tail to the front; the newest slot stays newest.
        w[..10 - n].copy_from_slice(&self.0[n..]);
        *self = Window(w);
    }

    /// `Window.Sum()` — saturating sum of all slots (overflow saturates to
    /// `u64::MAX`).
    #[must_use]
    pub fn sum(&self) -> u64 {
        self.0.iter().fold(0u64, |acc, &v| acc.saturating_add(v))
    }

    /// `ap3.ParseWindow` — deserialize from at least [`WINDOW_SIZE`] bytes
    /// (10 × big-endian `u64`; slot 0 is the oldest). Extra trailing bytes are
    /// ignored. Returns `None` if `bytes.len() < WINDOW_SIZE`. Mirrors coreth
    /// `plugin/evm/upgrade/ap3/window.go:68` (`ParseWindow`).
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < WINDOW_SIZE {
            return None;
        }
        let mut w = [0u64; 10];
        for (i, slot) in w.iter_mut().enumerate() {
            let off = i * 8;
            *slot = u64::from_be_bytes(bytes[off..off + 8].try_into().ok()?);
        }
        Some(Window(w))
    }

    /// `Window.Bytes()` — 80-byte big-endian serialization (slot 0 first).
    /// Mirrors coreth `plugin/evm/upgrade/ap3/window.go:113` (`Bytes`).
    #[must_use]
    pub fn to_bytes(self) -> [u8; WINDOW_SIZE] {
        let mut out = [0u8; WINDOW_SIZE];
        for (i, v) in self.0.iter().enumerate() {
            let off = i * 8;
            out[off..off + 8].copy_from_slice(&v.to_be_bytes());
        }
        out
    }
}

/// Per-phase parameters for [`base_fee_from_window`]. The Go switch keys these
/// off `IsX(parent.Time)` (spec 21 §4a trap 6); the caller resolves the active
/// phase and supplies the matching constants here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BaseFeeParams {
    /// `parentGasTarget` — the window-sum target (`ap3.TargetGas` = 10M;
    /// `ap5.TargetGas`/`etna` = 15M).
    pub target_gas: u64,
    /// `BaseFeeChangeDenominator` — 12 (AP3) or 36 (AP5+).
    pub change_denominator: u64,
    /// Minimum base fee bound (wei): 75 gwei (AP3), 25 gwei (AP4/AP5), 1 gwei
    /// (Etna).
    pub min_base_fee: U256,
    /// Maximum base fee bound (wei): 225 gwei (AP3), 1000 gwei (AP4),
    /// `MaxUint256` (AP5+).
    pub max_base_fee: U256,
}

impl BaseFeeParams {
    /// AP3 phase params: target 10M, denom 12, bounds [75 gwei, 225 gwei].
    #[must_use]
    pub fn ap3() -> Self {
        Self {
            target_gas: 10_000_000,
            change_denominator: 12,
            min_base_fee: gwei(75),
            max_base_fee: gwei(225),
        }
    }

    /// AP4 phase params: target 10M, denom 12, bounds [25 gwei, 1000 gwei].
    #[must_use]
    pub fn ap4() -> Self {
        Self {
            target_gas: 10_000_000,
            change_denominator: 12,
            min_base_fee: gwei(25),
            max_base_fee: gwei(1_000),
        }
    }

    /// AP5 phase params: target 15M, denom 36, bounds [25 gwei, MaxUint256].
    #[must_use]
    pub fn ap5() -> Self {
        Self {
            target_gas: 15_000_000,
            change_denominator: 36,
            min_base_fee: gwei(25),
            max_base_fee: U256::MAX,
        }
    }

    /// Etna phase params: target 15M, denom 36, bounds [1 gwei, MaxUint256].
    #[must_use]
    pub fn etna() -> Self {
        Self {
            target_gas: 15_000_000,
            change_denominator: 36,
            min_base_fee: gwei(1),
            max_base_fee: U256::MAX,
        }
    }
}

/// `n` gwei expressed in wei as a [`U256`] (1 gwei = 1e9 wei). No floats.
#[must_use]
pub fn gwei(n: u64) -> U256 {
    U256::from(n).saturating_mul(U256::from(1_000_000_000u64))
}

/// `baseFeeFromWindow` — compute the child block base fee from the rolling
/// window sum, the parent base fee, the elapsed time and per-phase params
/// (spec 21 §4a).
///
/// `time_elapsed` is `timestamp - parent.Time` in seconds (the caller must have
/// already rejected `timestamp < parent.Time`).
///
/// Traps replicated exactly:
/// 1. `total_gas == target` returns the parent base fee **early & unclamped**
///    (legacy behaviour — the fee can stay outside the nominal bounds).
/// 2. `delta` is floored at `1` (the fee always moves by ≥1 wei off-target).
/// 3. Only the **decrease** direction multiplies `delta` by `windowsElapsed`;
///    the increase does not.
/// 4. Two separate truncating divides: `/target` then `/denom`.
#[must_use]
pub fn base_fee_from_window(
    params: BaseFeeParams,
    window: &Window,
    parent_base_fee: U256,
    time_elapsed: u64,
) -> U256 {
    let total_gas = window.sum();
    let target = params.target_gas;

    // Trap 1: exact-target => unchanged, returned UNCLAMPED.
    if total_gas == target {
        return parent_base_fee;
    }

    let target_u = U256::from(target);
    let denom_u = U256::from(params.change_denominator);

    if total_gas > target {
        // Parent over target => base fee increases.
        let gap = U256::from(total_gas - target);
        // Trap 4: `* parentBaseFee`, then `/target`, then `/denom` (separate).
        let mut delta = gap
            .saturating_mul(parent_base_fee)
            .checked_div(target_u)
            .unwrap_or(U256::ZERO)
            .checked_div(denom_u)
            .unwrap_or(U256::ZERO);
        // Trap 2: floor delta at 1.
        if delta < U256::from(1u64) {
            delta = U256::from(1u64);
        }
        let unclamped = parent_base_fee.saturating_add(delta);
        clamp(unclamped, params.min_base_fee, params.max_base_fee)
    } else {
        // Parent under target => base fee decreases.
        let gap = U256::from(target - total_gas);
        let mut delta = gap
            .saturating_mul(parent_base_fee)
            .checked_div(target_u)
            .unwrap_or(U256::ZERO)
            .checked_div(denom_u)
            .unwrap_or(U256::ZERO);
        if delta < U256::from(1u64) {
            delta = U256::from(1u64);
        }
        // Trap 3: scale the decrease (only) by windows elapsed.
        let windows_elapsed = time_elapsed / WINDOW_LEN;
        if windows_elapsed > 1 {
            delta = delta.saturating_mul(U256::from(windows_elapsed));
        }
        let unclamped = parent_base_fee.saturating_sub(delta);
        clamp(unclamped, params.min_base_fee, params.max_base_fee)
    }
}

/// `selectBigWithinBounds(lower, value, upper)` — clamp `value` to
/// `[lower, upper]`.
fn clamp(value: U256, lower: U256, upper: U256) -> U256 {
    if value < lower {
        lower
    } else if value > upper {
        upper
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_add_saturates_into_last_slot() {
        let mut w = Window::default();
        w.add(&[10, 20, 30]);
        assert_eq!(w.0[9], 60);
        assert_eq!(w.0[8], 0);

        let mut w = Window([0; 10]);
        w.0[9] = u64::MAX - 5;
        w.add(&[10]);
        assert_eq!(w.0[9], u64::MAX);
    }

    #[test]
    fn window_shift_ages_front_and_clears_on_overflow() {
        let mut w = Window([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        w.shift(3);
        assert_eq!(w.0, [3, 4, 5, 6, 7, 8, 9, 0, 0, 0]);

        let mut w = Window([1; 10]);
        w.shift(10);
        assert_eq!(w, Window::default());

        let mut w = Window([1; 10]);
        w.shift(11);
        assert_eq!(w, Window::default());

        let mut w = Window([1; 10]);
        w.shift(0);
        assert_eq!(w.0, [1; 10]);
    }

    #[test]
    fn window_sum_saturates() {
        let w = Window([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(w.sum(), 55);

        let w = Window([u64::MAX, u64::MAX, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(w.sum(), u64::MAX);
    }

    fn single_slot(total: u64) -> Window {
        let mut w = Window::default();
        w.0[9] = total;
        w
    }

    #[test]
    fn base_fee_exact_target_unchanged_and_unclamped() {
        // Spec 21 §4 worked example 1 (AP5 params): sum == target => unchanged.
        let p = BaseFeeParams::ap5();
        let w = single_slot(15_000_000);
        let parent = gwei(100);
        assert_eq!(base_fee_from_window(p, &w, parent, 2), parent);

        // Trap 1: even a parent base fee OUTSIDE the bounds is returned as-is.
        let out_of_bounds = U256::from(1u64); // below 25 gwei min
        assert_eq!(base_fee_from_window(p, &w, out_of_bounds, 2), out_of_bounds);
    }

    #[test]
    fn base_fee_increase_two_times_target() {
        // Spec 21 §4 worked example 2 (AP5 params): sum = 2*target,
        // parent = 100 gwei => delta = 100e9/36 = 2_777_777_777,
        // baseFee = 102_777_777_777.
        let p = BaseFeeParams::ap5();
        let w = single_slot(30_000_000);
        let got = base_fee_from_window(p, &w, gwei(100), 2);
        assert_eq!(got, U256::from(102_777_777_777u64));
    }

    #[test]
    fn base_fee_decrease_windows_elapsed_asymmetry() {
        // Spec 21 §4 worked example 3 (AP5 params): sum = 0, parent = 100 gwei,
        // timeElapsed = 25 => windowsElapsed = 2 => delta doubled =>
        // baseFee = 100e9 - 5_555_555_554 = 94_444_444_446.
        let p = BaseFeeParams::ap5();
        let w = single_slot(0);
        let got = base_fee_from_window(p, &w, gwei(100), 25);
        assert_eq!(got, U256::from(94_444_444_446u64));

        // Asymmetry: a *symmetric* over-target move at the same elapsed time
        // must NOT be scaled by windowsElapsed (only the decrease is).
        let w_up = single_slot(30_000_000);
        let up = base_fee_from_window(p, &w_up, gwei(100), 25);
        // increase = 100e9 + 2_777_777_777 (no windows scaling).
        assert_eq!(up, U256::from(102_777_777_777u64));
    }

    #[test]
    fn base_fee_delta_floored_at_one() {
        let p = BaseFeeParams::ap5();
        // Decrease path, one under target, parent within bounds:
        let w2 = single_slot(14_999_999);
        let parent2 = gwei(100);
        let got = base_fee_from_window(p, &w2, parent2, 2);
        // delta = (1 * 100e9)/15M/36 = 100e9/540M = 185 (truncated), floored
        // stays 185 (>1). windowsElapsed = 0 => no scaling.
        assert_eq!(got, gwei(100).saturating_sub(U256::from(185u64)));
    }

    #[test]
    fn base_fee_min_clamp_per_phase() {
        // Etna min is 1 gwei: a huge decrease clamps up to 1 gwei.
        let p = BaseFeeParams::etna();
        let w = single_slot(0);
        let got = base_fee_from_window(p, &w, gwei(1), 0);
        assert_eq!(got, gwei(1));

        // AP3 min is 75 gwei: drive below it, clamp to 75 gwei.
        let p3 = BaseFeeParams::ap3();
        let w3 = single_slot(0);
        let got3 = base_fee_from_window(p3, &w3, gwei(76), 0);
        assert_eq!(got3, gwei(75));
    }

    #[test]
    fn base_fee_max_clamp_ap3() {
        // AP3 max is 225 gwei: drive above it, clamp to 225 gwei.
        let p3 = BaseFeeParams::ap3();
        let w = single_slot(20_000_000); // 2x AP3 target (10M)
        let got = base_fee_from_window(p3, &w, gwei(224), 2);
        assert_eq!(got, gwei(225));
    }
}
