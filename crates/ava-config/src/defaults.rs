// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Runtime-resolved and sourced flag defaults (specs 12 §1.3, 13 §23).
//!
//! Snowball-derived defaults are sourced from
//! `ava_snow::snowball::parameters::DEFAULT_PARAMETERS`; the outbound-throttler
//! defaults from `ava_network::throttling::outbound_msg`. The
//! `genesis.LocalParams` fee/staking-economics defaults below are local
//! constants until `ava-genesis` lands — every value is verified against the
//! committed Go flag snapshot (`tests/vectors/config/flags.json`).

use std::time::Duration;

/// OS-dependent default fd limit (Go `utils/ulimit.DefaultFDLimit`):
/// `32*1024` on Linux/BSD (`ulimit_unix.go`/`ulimit_bsd.go`), `10*1024` on
/// macOS (`ulimit_darwin.go`). Picked at compile time per `target_os` (13 §2).
#[cfg(target_os = "macos")]
pub const FD_LIMIT_DEFAULT: u64 = 10 * 1024;
/// OS-dependent default fd limit (Go `utils/ulimit.DefaultFDLimit`):
/// `32*1024` on Linux/BSD (`ulimit_unix.go`/`ulimit_bsd.go`), `10*1024` on
/// macOS (`ulimit_darwin.go`). Picked at compile time per `target_os` (13 §2).
#[cfg(not(target_os = "macos"))]
pub const FD_LIMIT_DEFAULT: u64 = 32 * 1024;

/// Go `runtime.NumCPU()` equivalent for the runtime-derived CPU-throttler
/// defaults (13 §9 note).
fn num_cpu() -> f64 {
    std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64)
}

/// Default for `--throttler-inbound-cpu-validator-alloc` (= `NumCPU`).
pub(crate) fn cpu_validator_alloc_default() -> String {
    num_cpu().to_string()
}

/// Default for `--throttler-inbound-cpu-max-non-validator-usage`
/// (= `0.8 * NumCPU`).
pub(crate) fn cpu_max_non_validator_usage_default() -> String {
    (0.8 * num_cpu()).to_string()
}

/// Default for `--throttler-inbound-cpu-max-non-validator-node-usage`
/// (= `NumCPU / 8`).
pub(crate) fn cpu_max_non_validator_node_usage_default() -> String {
    (num_cpu() / 8.0).to_string()
}

// ---------------------------------------------------------------------------
// genesis.LocalParams (Go `genesis/genesis_local.go`). On Mainnet/Fuji the
// corresponding flags are IGNORED at parse time (13 §4/§5); these are only the
// registered pflag defaults.
// TODO(M8.12): re-source every LOCAL_* constant below from ava-genesis.
// ---------------------------------------------------------------------------

/// `LocalParams.TxFee` (`MilliAvax`). TODO(M8.12): re-source from ava-genesis.
pub const LOCAL_TX_FEE: u64 = 1_000_000;
/// `LocalParams.CreateAssetTxFee` (`MilliAvax`). TODO(M8.12): re-source from
/// ava-genesis.
pub const LOCAL_CREATE_ASSET_TX_FEE: u64 = 1_000_000;

/// `LocalParams.DynamicFeeConfig.Weights[Bandwidth]`. TODO(M8.12): re-source
/// from ava-genesis.
pub const LOCAL_DYNAMIC_FEES_BANDWIDTH_WEIGHT: u64 = 1;
/// `LocalParams.DynamicFeeConfig.Weights[DBRead]`. TODO(M8.12): re-source from
/// ava-genesis.
pub const LOCAL_DYNAMIC_FEES_DB_READ_WEIGHT: u64 = 1_000;
/// `LocalParams.DynamicFeeConfig.Weights[DBWrite]`. TODO(M8.12): re-source
/// from ava-genesis.
pub const LOCAL_DYNAMIC_FEES_DB_WRITE_WEIGHT: u64 = 1_000;
/// `LocalParams.DynamicFeeConfig.Weights[Compute]`. TODO(M8.12): re-source
/// from ava-genesis.
pub const LOCAL_DYNAMIC_FEES_COMPUTE_WEIGHT: u64 = 4;
/// `LocalParams.DynamicFeeConfig.MaxCapacity`. TODO(M8.12): re-source from
/// ava-genesis.
pub const LOCAL_DYNAMIC_FEES_MAX_GAS_CAPACITY: u64 = 1_000_000;
/// `LocalParams.DynamicFeeConfig.MaxPerSecond`. TODO(M8.12): re-source from
/// ava-genesis.
pub const LOCAL_DYNAMIC_FEES_MAX_GAS_PER_SECOND: u64 = 100_000;
/// `LocalParams.DynamicFeeConfig.TargetPerSecond`. TODO(M8.12): re-source from
/// ava-genesis.
pub const LOCAL_DYNAMIC_FEES_TARGET_GAS_PER_SECOND: u64 = 50_000;
/// `LocalParams.DynamicFeeConfig.MinPrice`. TODO(M8.12): re-source from
/// ava-genesis.
pub const LOCAL_DYNAMIC_FEES_MIN_GAS_PRICE: u64 = 1;
/// `LocalParams.DynamicFeeConfig.ExcessConversionConstant`. TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_DYNAMIC_FEES_EXCESS_CONVERSION_CONSTANT: u64 = 2_164_043;

/// `LocalParams.ValidatorFeeConfig.Capacity`. TODO(M8.12): re-source from
/// ava-genesis.
pub const LOCAL_VALIDATOR_FEES_CAPACITY: u64 = 20_000;
/// `LocalParams.ValidatorFeeConfig.Target`. TODO(M8.12): re-source from
/// ava-genesis.
pub const LOCAL_VALIDATOR_FEES_TARGET: u64 = 10_000;
/// `LocalParams.ValidatorFeeConfig.MinPrice` (`1*NanoAvax`). TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_VALIDATOR_FEES_MIN_PRICE: u64 = 1;
/// `LocalParams.ValidatorFeeConfig.ExcessConversionConstant`. TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_VALIDATOR_FEES_EXCESS_CONVERSION_CONSTANT: u64 = 865_617;

/// `LocalParams.StakingConfig.UptimeRequirement` (`.8`). TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_UPTIME_REQUIREMENT: f64 = 0.8;
/// `LocalParams.StakingConfig.MinValidatorStake` (`2*KiloAvax`). TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_MIN_VALIDATOR_STAKE: u64 = 2_000_000_000_000;
/// `LocalParams.StakingConfig.MaxValidatorStake` (`3*MegaAvax`). TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_MAX_VALIDATOR_STAKE: u64 = 3_000_000_000_000_000;
/// `LocalParams.StakingConfig.MinDelegatorStake` (`25*Avax`). TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_MIN_DELEGATOR_STAKE: u64 = 25_000_000_000;
/// `LocalParams.StakingConfig.MinDelegationFee` (`20_000` = 2%). TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_MIN_DELEGATION_FEE: u64 = 20_000;
/// `LocalParams.StakingConfig.MinStakeDuration` (`24h`). TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_MIN_STAKE_DURATION: Duration = Duration::from_secs(24 * 60 * 60);
/// `LocalParams.StakingConfig.MaxStakeDuration` (`365*24h`). TODO(M8.12):
/// re-source from ava-genesis.
pub const LOCAL_MAX_STAKE_DURATION: Duration = Duration::from_secs(365 * 24 * 60 * 60);
/// `LocalParams.StakingConfig.RewardConfig.MaxConsumptionRate`
/// (`.12*PercentDenominator`). TODO(M8.12): re-source from ava-genesis.
pub const LOCAL_STAKE_MAX_CONSUMPTION_RATE: u64 = 120_000;
/// `LocalParams.StakingConfig.RewardConfig.MinConsumptionRate`
/// (`.10*PercentDenominator`). TODO(M8.12): re-source from ava-genesis.
pub const LOCAL_STAKE_MIN_CONSUMPTION_RATE: u64 = 100_000;
/// `LocalParams.StakingConfig.RewardConfig.MintingPeriod` (`365*24h`).
/// TODO(M8.12): re-source from ava-genesis.
pub const LOCAL_STAKE_MINTING_PERIOD: Duration = Duration::from_secs(365 * 24 * 60 * 60);
/// `LocalParams.StakingConfig.RewardConfig.SupplyCap` (`720*MegaAvax`).
/// TODO(M8.12): re-source from ava-genesis.
pub const LOCAL_STAKE_SUPPLY_CAP: u64 = 720_000_000_000_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fd_limit_default_is_os_dependent() {
        if cfg!(target_os = "macos") {
            assert_eq!(FD_LIMIT_DEFAULT, 10 * 1024);
        } else {
            assert_eq!(FD_LIMIT_DEFAULT, 32 * 1024);
        }
    }

    #[test]
    fn network_allow_private_ips_registered_default_is_false() {
        // 13 §8 note: the REGISTERED pflag default is `false`; the effective
        // default (`!ProductionNetworkIDs.contains(network_id)`) is resolved
        // at parse time (M8.12), NOT in the flag table.
        let spec = crate::flags::FLAG_SPECS
            .iter()
            .find(|s| s.key == crate::keys::KEY_NETWORK_ALLOW_PRIVATE_IPS)
            .expect("network-allow-private-ips spec");
        assert_eq!(spec.default.resolve(), "false");
    }

    #[test]
    fn cpu_defaults_are_runtime_derived() {
        let n: f64 = cpu_validator_alloc_default().parse().expect("parse NumCPU");
        assert!(n >= 1.0);
        let usage: f64 = cpu_max_non_validator_usage_default()
            .parse()
            .expect("parse 0.8*NumCPU");
        assert!((usage - 0.8 * n).abs() < 1e-9);
        let node: f64 = cpu_max_non_validator_node_usage_default()
            .parse()
            .expect("parse NumCPU/8");
        assert!((node - n / 8.0).abs() < 1e-9);
    }
}
