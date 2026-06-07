// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking-reward [`Calculator`] — exact `BigUint` port of
//! `vms/platformvm/reward/calculator.go`.
//!
//! The reward math is **consensus-critical**: every multiplication happens
//! before any division, and the three trailing divides are separate truncating
//! steps. Reordering or simplifying changes the truncation points and forks the
//! chain. See `specs/08-platformvm-pchain.md` §5 and
//! `specs/21-fee-economics-math.md` §3 for the worked vectors.

use num_bigint::BigUint;

use super::config::{Config, PERCENT_DENOMINATOR};

/// Returns `Some(v)` iff `n` fits in a `u64` (Go `big.Int.IsUint64`).
///
/// A `BigUint` fits in a `u64` exactly when its little-endian 64-bit-digit
/// representation is empty (value `0`) or a single digit.
fn biguint_to_u64(n: &BigUint) -> Option<u64> {
    let digits = n.to_u64_digits();
    match digits.as_slice() {
        [] => Some(0),
        [low] => Some(*low),
        _ => None,
    }
}

/// Computes Primary-Network staking rewards from a frozen [`Config`].
///
/// Construct with [`Calculator::new`]; the heavy `BigUint` operands derived from
/// the config are cached so each [`Calculator::calculate`] only allocates the
/// per-call operands.
#[derive(Clone, Debug)]
pub struct Calculator {
    /// `MaxConsumptionRate - MinConsumptionRate`.
    max_sub_min_consumption_rate: BigUint,
    /// `MinConsumptionRate`.
    min_consumption_rate: BigUint,
    /// `MintingPeriod` (nanoseconds), as a `BigUint`.
    minting_period: BigUint,
    /// `SupplyCap` (nAVAX).
    supply_cap: u64,
}

impl Calculator {
    /// Builds a [`Calculator`] from `config`.
    ///
    /// Mirrors `reward.NewCalculator`. The `max_consumption_rate -
    /// min_consumption_rate` subtraction matches the Go
    /// `c.MaxConsumptionRate - c.MinConsumptionRate` (callers must supply a
    /// config where `max >= min`; the canonical configs satisfy this). A
    /// `min > max` config saturates to `0` here rather than wrapping.
    #[must_use]
    pub fn new(config: Config) -> Self {
        let max_sub_min = config
            .max_consumption_rate
            .saturating_sub(config.min_consumption_rate);
        Self {
            max_sub_min_consumption_rate: BigUint::from(max_sub_min),
            min_consumption_rate: BigUint::from(config.min_consumption_rate),
            minting_period: BigUint::from(config.minting_period),
            supply_cap: config.supply_cap,
        }
    }

    /// Returns the number of tokens (nAVAX) to reward a staker with.
    ///
    /// ```text
    /// RemainingSupply          = SupplyCap - CurrentSupply
    /// PortionOfExistingSupply  = StakedAmount / CurrentSupply
    /// PortionOfStakingDuration = StakingDuration / MintingPeriod
    /// MintingRate              = MinRate + MaxSubMinRate * PortionOfStakingDuration
    /// Reward = RemainingSupply * PortionOfExistingSupply * MintingRate
    ///          * PortionOfStakingDuration
    /// ```
    ///
    /// expanded into the exact integer pipeline (all muls, then three divides):
    ///
    /// ```text
    /// adjConsumNum = maxSubMin * Δt + minRate * P
    /// adjConsumDen = P * D
    /// reward = remaining * adjConsumNum * stake * Δt / adjConsumDen / supply / P
    /// ```
    ///
    /// `staked_duration` and the minting period are in **nanoseconds**; `Δt`
    /// must not exceed the minting period (caller-enforced, as in Go). Callers
    /// must guarantee `current_supply <= SupplyCap` (the P-Chain supply
    /// invariant); a violation saturates `remaining` to `0`.
    ///
    /// If the computed reward does not fit in a `u64` it is clamped to
    /// `remaining`; otherwise the result is `min(remaining, reward)`.
    // `BigUint` is arbitrary-precision, so its `+`/`*`/`*=`/`/=` cannot overflow;
    // the `arithmetic_side_effects` lint targets machine-int wrap, which does not
    // apply. The three divisors are all non-zero: `adjConsumDen = P * D` and `P`
    // are non-zero config constants, and `current_supply` is caller-guaranteed
    // `> 0` (the P-Chain supply invariant — supply starts at the genesis initial
    // supply and only grows).
    #[allow(clippy::arithmetic_side_effects)]
    #[must_use]
    pub fn calculate(
        &self,
        staked_duration_ns: u64,
        staked_amount: u64,
        current_supply: u64,
    ) -> u64 {
        let big_staked_duration = BigUint::from(staked_duration_ns);
        let big_staked_amount = BigUint::from(staked_amount);
        let big_current_supply = BigUint::from(current_supply);

        // adjConsumNum = maxSubMin * Δt + minRate * P
        let adj_consumption_rate_numerator = &self.max_sub_min_consumption_rate
            * &big_staked_duration
            + &self.min_consumption_rate * &self.minting_period;
        // adjConsumDen = P * D
        let adj_consumption_rate_denominator =
            &self.minting_period * BigUint::from(PERCENT_DENOMINATOR);

        let remaining_supply = self.supply_cap.saturating_sub(current_supply);

        // reward = remaining * adjConsumNum * stake * Δt   (ALL muls first)
        let mut reward = BigUint::from(remaining_supply);
        reward *= &adj_consumption_rate_numerator;
        reward *= &big_staked_amount;
        reward *= &big_staked_duration;
        // THEN three separate truncating divides: / adjConsumDen / supply / P
        reward /= &adj_consumption_rate_denominator;
        reward /= &big_current_supply;
        reward /= &self.minting_period;

        match biguint_to_u64(&reward) {
            // !IsUint64 in Go: the reward overflows u64 ⇒ clamp to remaining.
            None => remaining_supply,
            Some(final_reward) => remaining_supply.min(final_reward),
        }
    }
}

/// Splits `total_amount` into `(amount_from_shares, remainder_amount)`.
///
/// `vms/platformvm/reward/calculator.go::Split`. `amount_from_shares` is the
/// `shares`-percentage portion (numerator over [`PERCENT_DENOMINATOR`]); the
/// remainder is the rest.
///
/// Invariant: `shares <= PERCENT_DENOMINATOR`.
///
/// Rounding is delayed as long as possible: the `remainderShares * total`
/// product is computed exactly when it fits in a `u64`, dividing only at the
/// end; on overflow it falls back to the less-precise `remainderShares *
/// (total / D)`.
#[must_use]
pub fn split(total_amount: u64, shares: u32) -> (u64, u64) {
    // remainderShares = PercentDenominator - shares
    let remainder_shares = PERCENT_DENOMINATOR.saturating_sub(u64::from(shares));

    // Fallback (less precise): remainderShares * (total / PercentDenominator).
    // saturating: matches Go uint64 behaviour for in-invariant inputs; only an
    // out-of-invariant `shares` could make this overflow.
    let mut remainder_amount = remainder_shares.saturating_mul(total_amount / PERCENT_DENOMINATOR);

    // Delay rounding as long as possible for small numbers: if the exact product
    // fits in u64, divide at the very end instead.
    if let Some(optimistic) = remainder_shares.checked_mul(total_amount) {
        remainder_amount = optimistic / PERCENT_DENOMINATOR;
    }

    let amount_from_shares = total_amount.saturating_sub(remainder_amount);
    (amount_from_shares, remainder_amount)
}
