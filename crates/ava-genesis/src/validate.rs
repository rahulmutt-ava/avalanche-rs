// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `genesis.go::validateConfig` — the gate run before building a **custom**
//! network's genesis (specs 23 §2), plus the custom-config loaders
//! (`GetConfigFile` / `GetConfigContent`) and the standard-network override
//! rejection shared by `from_file` / `from_flag`.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_crypto::address;
use ava_platformvm::txs::executor::StakingConfig;
use ava_types::constants::{self, LOCAL_ID, MAINNET_ID, TESTNET_ID};
use ava_types::short_id::ShortId;

use crate::config::Config;
use crate::error::{GenesisError, Result};
use crate::unparsed::UnparsedConfig;

/// `validateConfig` — the 10 ordered checks of specs 23 §2. Each failure maps
/// to a [`GenesisError`] variant matched by identity in tests.
///
/// # Errors
/// The first failing check's [`GenesisError`] variant (Go order).
pub fn validate_config(
    network_id: u32,
    config: &Config,
    staking_cfg: &StakingConfig,
) -> Result<()> {
    // 1. The provided network id must match the config's.
    if network_id != config.network_id {
        return Err(GenesisError::ConflictingNetworkIds {
            expected: network_id,
            actual: config.network_id,
        });
    }

    // 2. The initial supply must compute and be positive.
    let initial_supply = config.initial_supply()?;
    if initial_supply == 0 {
        return Err(GenesisError::NoSupply);
    }

    // 3. The start time cannot be in the future (`time.Since(start) >= 0`).
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    if config.start_time > now_secs {
        return Err(GenesisError::FutureStartTime(config.start_time));
    }

    // 4. A zero stake duration is rejected (no minimum is imposed otherwise).
    if config.initial_stake_duration == 0 {
        return Err(GenesisError::NoStakeDuration);
    }

    // 5. Genesis validators cannot out-stake the configured maximum duration.
    if config.initial_stake_duration > staking_cfg.max_stake_duration.as_secs() {
        return Err(GenesisError::StakeDurationTooHigh);
    }

    // 6. At least one initial staker.
    if config.initial_stakers.is_empty() {
        return Err(GenesisError::NoStakers);
    }

    // 7. The staggered offsets must fit inside the stake duration. Go computes
    //    `offset * uint64(len-1)` with wrapping uint64 arithmetic.
    let offset_time_required = config
        .initial_stake_duration_offset
        .wrapping_mul((config.initial_stakers.len() as u64).saturating_sub(1));
    if offset_time_required > config.initial_stake_duration {
        return Err(GenesisError::InitialStakeDurationTooLow(
            offset_time_required,
        ));
    }

    // 8. All staked funds are unique and have allocations.
    validate_initial_staked_funds(config)?;

    // 9. Σ locked allocation amounts ≥ the staker count.
    validate_allocations_locked_amount(config)?;

    // 10. The C-Chain genesis must be present.
    if config.c_chain_genesis.is_empty() {
        return Err(GenesisError::NoCChainGenesis);
    }

    Ok(())
}

/// `validateInitialStakedFunds` — non-empty; each entry unique and present in
/// the set of allocation `avax_addr`s. Duplicate `avax_addr` **across
/// allocations is allowed** (different `eth_addr`s can map to the same
/// `avax_addr`).
fn validate_initial_staked_funds(config: &Config) -> Result<()> {
    if config.initial_staked_funds.is_empty() {
        return Err(GenesisError::NoInitiallyStakedFunds);
    }

    let allocation_set: HashSet<ShortId> = config
        .allocations
        .iter()
        .map(|allocation| allocation.avax_addr)
        .collect();
    let mut initial_staked_funds_set = HashSet::with_capacity(config.initial_staked_funds.len());
    for staker in &config.initial_staked_funds {
        if !initial_staked_funds_set.insert(*staker) {
            return Err(GenesisError::DuplicateInitiallyStakedAddress(
                format_staker_addr(config.network_id, staker),
            ));
        }
        if !allocation_set.contains(staker) {
            return Err(GenesisError::NoAllocationToStake(format_staker_addr(
                config.network_id,
                staker,
            )));
        }
    }
    Ok(())
}

/// Formats a staked-funds address for an error message (`X-<hrp>1...`); falls
/// back to the hex form if bech32 formatting fails (Go reports a formatting
/// error instead — the address is decorative either way).
fn format_staker_addr(network_id: u32, staker: &ShortId) -> String {
    address::format("X", constants::get_hrp(network_id), staker.as_bytes())
        .unwrap_or_else(|_| staker.hex())
}

/// `validateAllocationsLockedAmount` — Σ of all `unlock_schedule[].amount`
/// (wrapping, like Go's `+=`) must reach the staker count.
fn validate_allocations_locked_amount(config: &Config) -> Result<()> {
    let mut total_locked: u64 = 0;
    for allocation in &config.allocations {
        for unlock in &allocation.unlock_schedule {
            total_locked = total_locked.wrapping_add(unlock.amount);
        }
    }
    let stakers_count = config.initial_stakers.len() as u64;
    if total_locked < stakers_count {
        return Err(GenesisError::AllocationsLockedAmountTooLow {
            locked: total_locked,
            stakers: stakers_count,
        });
    }
    Ok(())
}

/// `GetConfigFile` — loads + parses a custom genesis config from a JSON file.
///
/// # Errors
/// [`GenesisError::Io`] on read failure, [`GenesisError::InvalidGenesisJson`]
/// on malformed JSON, else the address parse error.
pub fn get_config_file(path: &Path) -> Result<Config> {
    let bytes = std::fs::read(path)?;
    parse_genesis_json_bytes_to_config(&bytes)
}

/// `GetConfigContent` — loads + parses a custom genesis config from a base64
/// (std) encoded JSON string (the `--genesis-file-content` flag form).
///
/// # Errors
/// [`GenesisError::InvalidBase64`] on a bad encoding,
/// [`GenesisError::InvalidGenesisJson`] on malformed JSON, else the address
/// parse error.
pub fn get_config_content(content_b64: &str) -> Result<Config> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_b64)
        .map_err(|e| GenesisError::InvalidBase64(e.to_string()))?;
    parse_genesis_json_bytes_to_config(&bytes)
}

/// `parseGenesisJSONBytesToConfig` — unmarshal the unparsed JSON form, then
/// parse the string addresses.
fn parse_genesis_json_bytes_to_config(bytes: &[u8]) -> Result<Config> {
    let unparsed: UnparsedConfig = serde_json::from_slice(bytes)
        .map_err(|e| GenesisError::InvalidGenesisJson(e.to_string()))?;
    unparsed.parse()
}

/// The `FromFile` / `FromFlag` guard: Mainnet, Testnet (Fuji) and Local must
/// use the embedded config, never a file/flag override.
///
/// # Errors
/// [`GenesisError::OverridesStandardNetworkConfig`] for a standard network id.
pub fn check_not_standard_network(network_id: u32) -> Result<()> {
    match network_id {
        MAINNET_ID | TESTNET_ID | LOCAL_ID => Err(GenesisError::OverridesStandardNetworkConfig(
            constants::network_name(network_id),
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)] // test fixtures
mod tests {
    use assert_matches::assert_matches;

    use crate::config::{Allocation, UNMODIFIED_LOCAL_CONFIG};

    use super::*;

    fn staking_cfg() -> StakingConfig {
        // genesisStakingCfg in Go's genesis_test.go: MaxStakeDuration = 365 d.
        StakingConfig::mainnet()
    }

    fn local() -> Config {
        UNMODIFIED_LOCAL_CONFIG.clone()
    }

    /// M8.6 red test: mirror Go `TestValidateConfig` — every check maps to its
    /// `GenesisError` variant by identity (specs 23 §2).
    #[test]
    fn validate_config_table() {
        let cfg = staking_cfg();

        // Happy paths: the three embedded configs validate.
        validate_config(1, &crate::config::MAINNET_CONFIG, &cfg).expect("mainnet");
        validate_config(5, &crate::config::FUJI_CONFIG, &cfg).expect("fuji");
        validate_config(12345, &local(), &cfg).expect("local");

        // networkID mismatch.
        assert_matches!(
            validate_config(2, &crate::config::MAINNET_CONFIG, &cfg),
            Err(GenesisError::ConflictingNetworkIds {
                expected: 2,
                actual: 1
            })
        );

        // invalid (future) start time.
        let mut c = local();
        c.start_time = 999_999_999_999_999;
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::FutureStartTime(_))
        );

        // no initial supply.
        let mut c = local();
        c.allocations = Vec::new();
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::NoSupply)
        );

        // no initial stakers.
        let mut c = local();
        c.initial_stakers = Vec::new();
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::NoStakers)
        );

        // invalid initial stake duration.
        let mut c = local();
        c.initial_stake_duration = 0;
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::NoStakeDuration)
        );

        // too large initial stake duration (max + 1s).
        let mut c = local();
        c.initial_stake_duration = cfg.max_stake_duration.as_secs() + 1;
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::StakeDurationTooHigh)
        );

        // invalid stake offset.
        let mut c = local();
        c.initial_stake_duration_offset = 100_000_000;
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::InitialStakeDurationTooLow(_))
        );

        // empty initial staked funds.
        let mut c = local();
        c.initial_staked_funds = Vec::new();
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::NoInitiallyStakedFunds)
        );

        // duplicate initial staked funds.
        let mut c = local();
        c.initial_staked_funds.push(c.initial_staked_funds[0]);
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::DuplicateInitiallyStakedAddress(_))
        );

        // initial staked funds not in allocations.
        let mut c = crate::config::FUJI_CONFIG.clone();
        c.initial_staked_funds.push(local().initial_staked_funds[0]);
        assert_matches!(
            validate_config(5, &c, &cfg),
            Err(GenesisError::NoAllocationToStake(_))
        );

        // total locked allocations below the staker count.
        let mut c = local();
        for a in &mut c.allocations {
            a.unlock_schedule = Vec::new();
        }
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::AllocationsLockedAmountTooLow { .. })
        );

        // empty C-Chain genesis.
        let mut c = local();
        c.c_chain_genesis = String::new();
        assert_matches!(
            validate_config(12345, &c, &cfg),
            Err(GenesisError::NoCChainGenesis)
        );

        // empty message is allowed.
        let mut c = local();
        c.message = String::new();
        validate_config(12345, &c, &cfg).expect("empty message allowed");

        // duplicate avaxAddr across allocations is allowed (different ethAddrs
        // can claim to the same avaxAddr).
        let mut c = local();
        let mut dup: Allocation = c.allocations[0].clone();
        dup.eth_addr = ShortId::from([0xee; 20]);
        c.allocations.push(dup);
        validate_config(12345, &c, &cfg).expect("duplicate avaxAddr allowed");
    }

    /// `FromFile`/`FromFlag` reject Mainnet/Testnet/Local network ids.
    #[test]
    fn standard_networks_rejected() {
        for id in [1u32, 5, 12345] {
            assert_matches!(
                check_not_standard_network(id),
                Err(GenesisError::OverridesStandardNetworkConfig(_))
            );
        }
        check_not_standard_network(9999).expect("custom network ok");
    }

    /// `GetConfigContent` decodes base64(JSON); bad base64 / bad JSON map to
    /// their sentinels.
    #[test]
    fn config_content_loader() {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;

        let encoded = engine.encode(crate::config::LOCAL_GENESIS_CONFIG_JSON);
        let parsed = get_config_content(&encoded).expect("decode embedded local");
        assert_eq!(parsed.network_id, 12345);

        assert_matches!(
            get_config_content("!!!not-base64!!!"),
            Err(GenesisError::InvalidBase64(_))
        );
        let bad_json = engine.encode("{\"networkID\": 9999}}}}");
        assert_matches!(
            get_config_content(&bad_json),
            Err(GenesisError::InvalidGenesisJson(_))
        );
    }
}
