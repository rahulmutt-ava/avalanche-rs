// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Apricot Phase 4 block gas cost (spec 21 §4b, spec 10 §7.1). Mirrors coreth
//! `plugin/evm/upgrade/ap4/cost.go` (`ap4.BlockGasCost`) +
//! `plugin/evm/customheader/block_gas_cost.go` (`BlockGasCostWithStep`,
//! `BlockGasCost`) bit-for-bit.
//!
//! A per-block surcharge that rises when blocks come faster than the target
//! block rate and falls when slower. All arithmetic is checked/saturating over
//! `u64` — no floating point (spec 00 §6.1).

/// `ap4.TargetBlockRate` — target seconds between blocks.
pub const TARGET_BLOCK_RATE: u64 = 2;
/// `ap4.MinBlockGasCost` — lower clamp bound.
pub const MIN_BLOCK_GAS_COST: u64 = 0;
/// `ap4.MaxBlockGasCost` — upper clamp bound.
pub const MAX_BLOCK_GAS_COST: u64 = 1_000_000;
/// `ap4.BlockGasCostStep` — AP4 step (cost change per second of deviation).
pub const BLOCK_GAS_COST_STEP_AP4: u64 = 50_000;
/// `ap5.BlockGasCostStep` — AP5 step.
pub const BLOCK_GAS_COST_STEP_AP5: u64 = 200_000;

/// `BlockGasCostWithStep(parentCost, step, timeElapsed)` — compute the block
/// gas cost from the parent cost, step and elapsed seconds (spec 21 §4b).
///
/// `parent_cost == None` is the AP3/AP4 boundary (new network): returns
/// `MinBlockGasCost` (= 0). Otherwise:
///
/// ```text
/// deviation = |TargetBlockRate - timeElapsed|
/// change    = step * deviation            (overflow -> u64::MAX)
/// cost = if timeElapsed > 2 { parentCost - change (underflow -> 0) }
///        else               { parentCost + change (overflow  -> MAX) }
/// clamp(cost, 0, 1_000_000)
/// ```
#[must_use]
pub fn ap4_block_gas_cost(parent_cost: Option<u64>, step: u64, time_elapsed: u64) -> u64 {
    let parent = match parent_cost {
        None => return MIN_BLOCK_GAS_COST,
        Some(p) => p,
    };

    // `safemath.AbsDiff(TargetBlockRate, timeElapsed)`.
    let deviation = TARGET_BLOCK_RATE.abs_diff(time_elapsed);
    // `safemath.Mul`; on overflow Go falls back to MaxUint64.
    let change = step.saturating_mul(deviation);

    let cost = if time_elapsed > TARGET_BLOCK_RATE {
        // Slower than target => cheaper; underflow defaults to MinBlockGasCost.
        parent.checked_sub(change).unwrap_or(MIN_BLOCK_GAS_COST)
    } else {
        // Faster than (or equal to) target => costlier; overflow defaults to
        // MaxBlockGasCost.
        parent.checked_add(change).unwrap_or(MAX_BLOCK_GAS_COST)
    };

    // clamp(cost, MinBlockGasCost, MaxBlockGasCost).
    cost.clamp(MIN_BLOCK_GAS_COST, MAX_BLOCK_GAS_COST)
}

/// `BlockGasCost(config, parent, timestamp)` — the fork-gated wrapper.
///
/// In **Granite** the mechanism is retired and this returns `Some(0)` outright
/// (`granite` arg true). Pre-AP4 the cost is `nil` in Go; the caller decides
/// that gate, so this helper assumes AP4+ is active and only models the Granite
/// override + the underlying [`ap4_block_gas_cost`].
#[must_use]
pub fn block_gas_cost(
    parent_cost: Option<u64>,
    step: u64,
    time_elapsed: u64,
    granite: bool,
) -> u64 {
    if granite {
        return 0;
    }
    ap4_block_gas_cost(parent_cost, step, time_elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_target_unchanged() {
        // Spec 21 §4 worked example 1: parent 100k, dt 2 => deviation 0 => 100k.
        assert_eq!(
            ap4_block_gas_cost(Some(100_000), BLOCK_GAS_COST_STEP_AP4, 2),
            100_000
        );
    }

    #[test]
    fn faster_increases() {
        // Spec 21 §4 worked example 2: parent 100k, dt 1 => +50k => 150k.
        assert_eq!(
            ap4_block_gas_cost(Some(100_000), BLOCK_GAS_COST_STEP_AP4, 1),
            150_000
        );
        // dt 0 => deviation 2 => +100k => 200k.
        assert_eq!(
            ap4_block_gas_cost(Some(100_000), BLOCK_GAS_COST_STEP_AP4, 0),
            200_000
        );
    }

    #[test]
    fn slower_decreases_and_clamps_to_zero() {
        // Spec 21 §4 worked example 3: parent 100k, dt 10 => change 50k*8=400k
        // => sat_sub(100k, 400k) = 0.
        assert_eq!(
            ap4_block_gas_cost(Some(100_000), BLOCK_GAS_COST_STEP_AP4, 10),
            0
        );
        // dt 5 => change 50k*3=150k => 100k-150k underflow => 0.
        assert_eq!(
            ap4_block_gas_cost(Some(100_000), BLOCK_GAS_COST_STEP_AP4, 5),
            0
        );
    }

    #[test]
    fn upper_clamp() {
        // parent at max, faster => add saturates then clamps to MAX.
        assert_eq!(
            ap4_block_gas_cost(Some(900_000), BLOCK_GAS_COST_STEP_AP4, 0),
            1_000_000
        );
        // overflow on mul (huge step) still clamps to MAX on the add path.
        assert_eq!(ap4_block_gas_cost(Some(500_000), u64::MAX, 0), 1_000_000);
    }

    #[test]
    fn parent_none_is_zero() {
        assert_eq!(ap4_block_gas_cost(None, BLOCK_GAS_COST_STEP_AP4, 1), 0);
        assert_eq!(ap4_block_gas_cost(None, BLOCK_GAS_COST_STEP_AP4, 10), 0);
    }

    #[test]
    fn ap5_step() {
        // AP5 step 200k: parent 100k, dt 1 => +200k => 300k.
        assert_eq!(
            ap4_block_gas_cost(Some(100_000), BLOCK_GAS_COST_STEP_AP5, 1),
            300_000
        );
    }

    #[test]
    fn granite_is_zero() {
        assert_eq!(
            block_gas_cost(Some(100_000), BLOCK_GAS_COST_STEP_AP4, 1, true),
            0
        );
        // non-Granite delegates to ap4_block_gas_cost.
        assert_eq!(
            block_gas_cost(Some(100_000), BLOCK_GAS_COST_STEP_AP4, 1, false),
            150_000
        );
    }
}
