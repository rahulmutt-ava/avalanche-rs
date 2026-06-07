// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking-reward golden + property tests (`golden::reward_vectors`,
//! `prop::reward_monotone`).
//!
//! Provenance of `tests/vectors/platformvm/reward_grid.json`: generated from the
//! Go reference `github.com/ava-labs/avalanchego` —
//! `vms/platformvm/reward.NewCalculator(cfg).Calculate` over a
//! `(Δt, stake, supply)` grid plus the three `specs/21-fee-economics-math.md` §3
//! worked examples (full-period ≈192 AVAX, zero-duration = 0, near-cap clamp),
//! and `reward.Split` over a set of `(total, shares)` inputs. This makes the
//! golden table a true differential oracle, not a self-consistency check.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use serde::Deserialize;

use ava_platformvm::reward::{Calculator, Config, PERCENT_DENOMINATOR, split};

const REWARD_GRID_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/vectors/platformvm/reward_grid.json"
));

#[derive(Debug, Deserialize)]
struct GoldenConfig {
    max_consumption_rate: u64,
    min_consumption_rate: u64,
    minting_period_ns: u64,
    supply_cap: u64,
    percent_denominator: u64,
}

#[derive(Debug, Deserialize)]
struct RewardVec {
    #[serde(default)]
    name: String,
    duration_ns: u64,
    stake: u64,
    supply: u64,
    expected_reward: u64,
}

#[derive(Debug, Deserialize)]
struct SplitVec {
    total: u64,
    shares: u32,
    amount_from_shares: u64,
    remainder_amount: u64,
}

#[derive(Debug, Deserialize)]
struct Golden {
    config: GoldenConfig,
    grid: Vec<RewardVec>,
    worked_examples: Vec<RewardVec>,
    splits: Vec<SplitVec>,
}

fn load() -> Golden {
    serde_json::from_str(REWARD_GRID_JSON).expect("reward_grid.json must parse")
}

fn config_from_golden(c: &GoldenConfig) -> Config {
    Config {
        max_consumption_rate: c.max_consumption_rate,
        min_consumption_rate: c.min_consumption_rate,
        minting_period: c.minting_period_ns,
        supply_cap: c.supply_cap,
    }
}

mod golden {
    use super::*;

    /// Every `(Δt, stake, supply)` row in the Go-generated grid and worked
    /// examples reproduces Go's `reward.Calculate` output bit-exactly, and every
    /// `Split` row matches Go's `reward.Split`.
    #[test]
    fn reward_vectors() {
        let g = load();

        // The frozen config must match our canonical mainnet config and the
        // documented constants (specs 21 §3).
        let mainnet = Config::mainnet();
        assert_eq!(config_from_golden(&g.config), mainnet);
        assert_eq!(g.config.percent_denominator, PERCENT_DENOMINATOR);
        assert_eq!(PERCENT_DENOMINATOR, 1_000_000);

        let calc = Calculator::new(mainnet);

        let mut checked = 0usize;
        for v in g.grid.iter().chain(g.worked_examples.iter()) {
            let got = calc.calculate(v.duration_ns, v.stake, v.supply);
            assert_eq!(
                got, v.expected_reward,
                "reward mismatch for {} (Δt={}, stake={}, supply={}): got {}, want {}",
                v.name, v.duration_ns, v.stake, v.supply, got, v.expected_reward
            );
            checked += 1;
        }
        assert!(checked > 0, "grid must be non-empty");

        // Spec 21 §3 worked-example anchors, asserted explicitly so a corrupt
        // grid file cannot silently drop them.
        let full = g
            .worked_examples
            .iter()
            .find(|v| v.name == "full_period_~192avax")
            .expect("full-period worked example present");
        assert_eq!(
            calc.calculate(full.duration_ns, full.stake, full.supply),
            192_000_000_000,
            "full-period stake must mint exactly 192 AVAX"
        );

        let zero = g
            .worked_examples
            .iter()
            .find(|v| v.name == "zero_duration")
            .expect("zero-duration worked example present");
        assert_eq!(
            calc.calculate(zero.duration_ns, zero.stake, zero.supply),
            0,
            "zero-duration stake must mint nothing"
        );

        // Near-cap example: reward must never exceed remaining supply (the
        // min(remaining, ·) cap).
        let near = g
            .worked_examples
            .iter()
            .find(|v| v.name == "near_cap_clamp")
            .expect("near-cap worked example present");
        let remaining = mainnet.supply_cap - near.supply;
        assert!(
            calc.calculate(near.duration_ns, near.stake, near.supply) <= remaining,
            "near-cap reward must be clamped to remaining supply"
        );

        // Split vectors.
        for s in &g.splits {
            let (from_shares, remainder) = split(s.total, s.shares);
            assert_eq!(
                from_shares, s.amount_from_shares,
                "split fromShares mismatch (total={}, shares={})",
                s.total, s.shares
            );
            assert_eq!(
                remainder, s.remainder_amount,
                "split remainder mismatch (total={}, shares={})",
                s.total, s.shares
            );
            // Internal invariant: the two halves reconstruct the total.
            assert_eq!(from_shares + remainder, s.total);
        }
    }
}

mod prop {
    use proptest::prelude::*;

    use super::*;

    fn calc() -> Calculator {
        Calculator::new(Config::mainnet())
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 4096, ..ProptestConfig::default() })]

        /// Implementation-agnostic invariants of the reward calculator:
        /// * the reward never exceeds the remaining supply;
        /// * a zero staking duration yields zero reward;
        /// * the reward is non-decreasing in both `stake` and `Δt`.
        ///
        /// `supply` is kept in `(0, supply_cap)` (Go invariant: supply > 0 so the
        /// final `/supply` divide is well-defined, and supply <= cap). `Δt` is
        /// bounded by the minting period (valid stake durations).
        #[test]
        fn reward_monotone(
            dt in 0u64..=31_536_000_000_000_000u64,
            stake in 0u64..=1_000_000_000_000_000_000u64,
            supply in 1u64..720_000_000_000_000_000u64,
        ) {
            let c = calc();
            let cap = Config::mainnet().supply_cap;
            let remaining = cap - supply;

            let r = c.calculate(dt, stake, supply);
            prop_assert!(r <= remaining, "reward {r} exceeds remaining {remaining}");

            // Zero duration ⇒ zero reward.
            prop_assert_eq!(c.calculate(0, stake, supply), 0);

            // Non-decreasing in stake (hold dt, supply fixed).
            if stake < u64::MAX {
                let bigger_stake = stake.saturating_add(stake / 7 + 1);
                prop_assert!(
                    c.calculate(dt, bigger_stake, supply) >= r,
                    "reward must be non-decreasing in stake"
                );
            }

            // Non-decreasing in Δt up to the minting period (hold stake, supply).
            let bigger_dt = (dt + dt / 5 + 1).min(31_536_000_000_000_000u64);
            prop_assert!(
                c.calculate(bigger_dt, stake, supply) >= r,
                "reward must be non-decreasing in Δt"
            );
        }
    }
}
