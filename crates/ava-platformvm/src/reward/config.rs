// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking-reward [`Config`] — port of `vms/platformvm/reward/config.go`.
//!
//! The denominator used to express percentages and to scale the consumption
//! rates (`PercentDenominator == consumptionRateDenominator == 1_000_000`).

/// The denominator used to calculate percentages.
///
/// `vms/platformvm/reward/config.go::PercentDenominator`. It doubles as
/// `consumptionRateDenominator` (the magnitude offset that emulates floating
/// point fractions).
pub const PERCENT_DENOMINATOR: u64 = 1_000_000;

/// `1 MegaAvax = 1e15 nAVAX` (`utils/units`). Used to express [`Config::supply_cap`].
const MEGA_AVAX: u64 = 1_000_000_000_000_000;

/// Reward-calculator configuration (`reward.Config`).
///
/// The mainnet and Fuji values are identical (see [`Config::mainnet`]); both are
/// taken verbatim from `genesis/*.go` and `specs/21-fee-economics-math.md` §3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Rate to allocate funds if the validator's stake duration equals
    /// [`Config::minting_period`]. Scaled by [`PERCENT_DENOMINATOR`].
    pub max_consumption_rate: u64,

    /// Rate to allocate funds if the validator's stake duration is 0. Scaled by
    /// [`PERCENT_DENOMINATOR`].
    pub min_consumption_rate: u64,

    /// The period (in **nanoseconds**) over which the staking calculator runs.
    /// A validator's stake duration is not valid if it is larger than this.
    pub minting_period: u64,

    /// The target value that the reward calculation is asymptotic to (nAVAX).
    pub supply_cap: u64,
}

impl Config {
    /// The canonical Primary-Network reward config (identical on mainnet & Fuji).
    ///
    /// * `max_consumption_rate = 0.12 * 1e6 = 120_000`
    /// * `min_consumption_rate = 0.10 * 1e6 = 100_000`
    /// * `minting_period = 365 d = 31_536_000_000_000_000 ns`
    /// * `supply_cap = 720 * MegaAvax = 720_000_000_000_000_000 nAVAX`
    #[must_use]
    pub const fn mainnet() -> Self {
        Self {
            max_consumption_rate: 120_000,
            min_consumption_rate: 100_000,
            // 365 * 24 * 60 * 60 seconds, in nanoseconds.
            minting_period: 31_536_000_000_000_000,
            supply_cap: 720 * MEGA_AVAX,
        }
    }
}
