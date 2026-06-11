// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-network fee + staking parameters (Go `genesis/params.go` +
//! `genesis_{mainnet,fuji,local}.go`; specs 13 §4/§5, 12 §1.6).
//!
//! On Mainnet/Fuji the fee/staking-economics *flags* are ignored at parse
//! time; `get_tx_fee_config` / `get_staking_config` are the authoritative
//! values (`config.go::getTxFeeConfig`/`getStakingConfig`). Unknown (custom)
//! network ids fall back to the Local params, exactly like Go.

use std::time::Duration;

use ava_platformvm::reward;
use ava_platformvm::txs::executor;
use ava_types::constants::{FUJI_ID, MAINNET_ID};

// Re-exported so `ava-config` can assemble the staking-economics block from
// flags (custom networks) without a direct `ava-platformvm` dependency.
pub use ava_platformvm::reward::{Config as RewardConfig, PERCENT_DENOMINATOR};

/// `1 AVAX = 1e9 nAVAX` (`utils/units`).
const AVAX: u64 = 1_000_000_000;
/// `1 MilliAvax = 1e6 nAVAX`.
const MILLI_AVAX: u64 = 1_000_000;
/// `1 KiloAvax = 1e12 nAVAX`.
const KILO_AVAX: u64 = 1_000 * AVAX;
/// `1 MegaAvax = 1e15 nAVAX`.
const MEGA_AVAX: u64 = 1_000_000 * AVAX;

/// Go `genesis.StakingConfig` — the Primary-Network staking economics block.
#[derive(Clone, Debug, PartialEq)]
pub struct StakingConfig {
    /// Fraction of time a validator must be online to receive rewards
    /// (`uptimeRequirement`).
    pub uptime_requirement: f64,
    /// Minimum stake, in nAVAX, to validate the primary network.
    pub min_validator_stake: u64,
    /// Maximum stake, in nAVAX, on a primary-network validator.
    pub max_validator_stake: u64,
    /// Minimum stake, in nAVAX, that can be delegated.
    pub min_delegator_stake: u64,
    /// Minimum delegation fee, in `[0, 1_000_000]` (millionths).
    pub min_delegation_fee: u32,
    /// Minimum staking duration.
    pub min_stake_duration: Duration,
    /// Maximum staking duration.
    pub max_stake_duration: Duration,
    /// The reward (minting) function config.
    pub reward_config: reward::Config,
}

impl StakingConfig {
    /// Converts into the executor-facing
    /// [`StakingConfig`](executor::StakingConfig) shape consumed by the
    /// genesis build/validate pipeline ([`crate::from_file`] /
    /// [`crate::from_flag`]).
    #[must_use]
    pub fn to_executor(&self) -> executor::StakingConfig {
        executor::StakingConfig {
            min_validator_stake: self.min_validator_stake,
            max_validator_stake: self.max_validator_stake,
            min_delegator_stake: self.min_delegator_stake,
            min_delegation_fee: self.min_delegation_fee,
            min_stake_duration: self.min_stake_duration,
            max_stake_duration: self.max_stake_duration,
            reward_config: self.reward_config,
        }
    }
}

/// Go `gas.Config` — the ACP-103 dynamic-fee parameters carried in the
/// genesis params (the owning gas module keys these as consts, not a struct;
/// this is the config-shaped holder).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicFeeConfig {
    /// Per-dimension complexity→gas weights
    /// `[Bandwidth, DBRead, DBWrite, Compute]`.
    pub weights: [u64; 4],
    /// Maximum stored gas (`MaxCapacity`).
    pub max_capacity: u64,
    /// Gas refill rate (`MaxPerSecond`).
    pub max_per_second: u64,
    /// Target gas usage rate (`TargetPerSecond`).
    pub target_per_second: u64,
    /// Minimum gas price (`MinPrice`).
    pub min_price: u64,
    /// Excess→price conversion constant (`ExcessConversionConstant`).
    pub excess_conversion_constant: u64,
}

/// Go `validators/fee.Config` — the ACP-77 continuous validator-fee
/// parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatorFeeConfig {
    /// Maximum number of L1 validators (`Capacity`).
    pub capacity: u64,
    /// Target number of L1 validators (`Target`).
    pub target: u64,
    /// Minimum validator price in nAVAX/s (`MinPrice`).
    pub min_price: u64,
    /// Excess→price conversion constant (`ExcessConversionConstant`).
    pub excess_conversion_constant: u64,
}

/// Go `genesis.TxFeeConfig`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxFeeConfig {
    /// Fee, in nAVAX, for transactions that create new assets.
    pub create_asset_tx_fee: u64,
    /// Static transaction fee, in nAVAX.
    pub tx_fee: u64,
    /// The ACP-103 dynamic gas fee parameters.
    pub dynamic_fee_config: DynamicFeeConfig,
    /// The ACP-77 validator fee parameters.
    pub validator_fee_config: ValidatorFeeConfig,
}

/// The dynamic-fee config shared verbatim by Mainnet, Fuji and Local
/// (`genesis_*.go`).
const DYNAMIC_FEE_CONFIG: DynamicFeeConfig = DynamicFeeConfig {
    weights: [1, 1_000, 1_000, 4],
    max_capacity: 1_000_000,
    max_per_second: 100_000,
    target_per_second: 50_000,
    min_price: 1,
    excess_conversion_constant: 2_164_043, // double every 30s
};

/// Go `MainnetParams.TxFeeConfig`.
const MAINNET_TX_FEE_CONFIG: TxFeeConfig = TxFeeConfig {
    create_asset_tx_fee: 10 * MILLI_AVAX,
    tx_fee: MILLI_AVAX,
    dynamic_fee_config: DYNAMIC_FEE_CONFIG,
    validator_fee_config: ValidatorFeeConfig {
        capacity: 20_000,
        target: 10_000,
        min_price: 512,                            // 512 * NanoAvax
        excess_conversion_constant: 1_246_488_515, // double every day
    },
};

/// Go `FujiParams.TxFeeConfig`.
const FUJI_TX_FEE_CONFIG: TxFeeConfig = TxFeeConfig {
    create_asset_tx_fee: 10 * MILLI_AVAX,
    tx_fee: MILLI_AVAX,
    dynamic_fee_config: DYNAMIC_FEE_CONFIG,
    validator_fee_config: ValidatorFeeConfig {
        capacity: 20_000,
        target: 10_000,
        min_price: 512,
        excess_conversion_constant: 51_937_021, // double every hour
    },
};

/// Go `LocalParams.TxFeeConfig`.
const LOCAL_TX_FEE_CONFIG: TxFeeConfig = TxFeeConfig {
    create_asset_tx_fee: MILLI_AVAX,
    tx_fee: MILLI_AVAX,
    dynamic_fee_config: DYNAMIC_FEE_CONFIG,
    validator_fee_config: ValidatorFeeConfig {
        capacity: 20_000,
        target: 10_000,
        min_price: 1,                        // 1 * NanoAvax
        excess_conversion_constant: 865_617, // double every minute
    },
};

/// The reward config shared by all three networks (`reward.Config` —
/// `.12`/`.10` consumption over `PercentDenominator`, 365-day minting period,
/// 720 MAVAX supply cap).
fn shared_reward_config() -> reward::Config {
    reward::Config::mainnet()
}

/// Go `MainnetParams.StakingConfig`.
fn mainnet_staking_config() -> StakingConfig {
    StakingConfig {
        uptime_requirement: 0.8,
        min_validator_stake: 2 * KILO_AVAX,
        max_validator_stake: 3 * MEGA_AVAX,
        min_delegator_stake: 25 * AVAX,
        min_delegation_fee: 20_000, // 2%
        min_stake_duration: Duration::from_secs(2 * 7 * 24 * 60 * 60),
        max_stake_duration: Duration::from_secs(365 * 24 * 60 * 60),
        reward_config: shared_reward_config(),
    }
}

/// Go `FujiParams.StakingConfig`.
fn fuji_staking_config() -> StakingConfig {
    StakingConfig {
        uptime_requirement: 0.8,
        min_validator_stake: AVAX,
        max_validator_stake: 3 * MEGA_AVAX,
        min_delegator_stake: AVAX,
        min_delegation_fee: 20_000,
        min_stake_duration: Duration::from_secs(24 * 60 * 60),
        max_stake_duration: Duration::from_secs(365 * 24 * 60 * 60),
        reward_config: shared_reward_config(),
    }
}

/// Go `LocalParams.StakingConfig`.
fn local_staking_config() -> StakingConfig {
    StakingConfig {
        uptime_requirement: 0.8,
        min_validator_stake: 2 * KILO_AVAX,
        max_validator_stake: 3 * MEGA_AVAX,
        min_delegator_stake: 25 * AVAX,
        min_delegation_fee: 20_000,
        min_stake_duration: Duration::from_secs(24 * 60 * 60),
        max_stake_duration: Duration::from_secs(365 * 24 * 60 * 60),
        reward_config: shared_reward_config(),
    }
}

/// Go `genesis.GetTxFeeConfig` — Mainnet/Fuji/Local params; unknown ids fall
/// back to Local.
#[must_use]
pub fn get_tx_fee_config(network_id: u32) -> TxFeeConfig {
    match network_id {
        MAINNET_ID => MAINNET_TX_FEE_CONFIG,
        FUJI_ID => FUJI_TX_FEE_CONFIG,
        _ => LOCAL_TX_FEE_CONFIG,
    }
}

/// Go `genesis.GetStakingConfig` — Mainnet/Fuji/Local params; unknown ids
/// fall back to Local.
#[must_use]
pub fn get_staking_config(network_id: u32) -> StakingConfig {
    match network_id {
        MAINNET_ID => mainnet_staking_config(),
        FUJI_ID => fuji_staking_config(),
        _ => local_staking_config(),
    }
}

#[cfg(test)]
mod tests {
    use ava_types::constants::LOCAL_ID;

    use super::*;

    #[test]
    fn params_match_go_genesis_params() {
        // genesis_mainnet.go
        let m = get_tx_fee_config(MAINNET_ID);
        assert_eq!(m.create_asset_tx_fee, 10_000_000);
        assert_eq!(m.tx_fee, 1_000_000);
        assert_eq!(m.dynamic_fee_config.weights, [1, 1_000, 1_000, 4]);
        assert_eq!(m.dynamic_fee_config.excess_conversion_constant, 2_164_043);
        assert_eq!(m.validator_fee_config.min_price, 512);
        assert_eq!(
            m.validator_fee_config.excess_conversion_constant,
            1_246_488_515
        );

        // genesis_fuji.go
        let f = get_tx_fee_config(FUJI_ID);
        assert_eq!(f.create_asset_tx_fee, 10_000_000);
        assert_eq!(
            f.validator_fee_config.excess_conversion_constant,
            51_937_021
        );

        // genesis_local.go (also the fallback for custom ids)
        let l = get_tx_fee_config(LOCAL_ID);
        assert_eq!(l.create_asset_tx_fee, 1_000_000);
        assert_eq!(l.validator_fee_config.min_price, 1);
        assert_eq!(l.validator_fee_config.excess_conversion_constant, 865_617);
        assert_eq!(get_tx_fee_config(1_337), l);

        let m = get_staking_config(MAINNET_ID);
        assert_eq!(m.min_validator_stake, 2_000_000_000_000);
        assert_eq!(m.max_validator_stake, 3_000_000_000_000_000);
        assert_eq!(m.min_delegator_stake, 25_000_000_000);
        assert_eq!(m.min_stake_duration, Duration::from_secs(14 * 24 * 60 * 60));

        let f = get_staking_config(FUJI_ID);
        assert_eq!(f.min_validator_stake, 1_000_000_000);
        assert_eq!(f.min_delegator_stake, 1_000_000_000);
        assert_eq!(f.min_stake_duration, Duration::from_secs(24 * 60 * 60));

        let l = get_staking_config(LOCAL_ID);
        assert_eq!(l.min_validator_stake, 2_000_000_000_000);
        assert_eq!(l.min_stake_duration, Duration::from_secs(24 * 60 * 60));
        assert_eq!(get_staking_config(1_337), l);

        // The executor view drops only the uptime requirement.
        let exec = m.to_executor();
        assert_eq!(exec.min_validator_stake, m.min_validator_stake);
        assert_eq!(exec.reward_config, m.reward_config);
    }
}
