// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

#![forbid(unsafe_code)]

//! `ava-genesis` — network genesis construction (port of `genesis/**`,
//! specs 23, 12 §6).
//!
//! **Source of truth** for: the embedded Mainnet/Fuji/Local genesis configs,
//! the bootstrapper lists, and the byte-exact `FromConfig` pipeline that
//! derives the P-Chain genesis bytes + the AVAX asset ID + the genesis block
//! IDs. This is the **early interop gate**: a Rust node must produce genesis
//! bytes, genesis block IDs, AVAX asset ID, and per-VM `CreateChainTx` IDs that
//! are byte-identical to Go for Mainnet, Fuji, and Local, or it cannot join
//! those networks (specs 23 §0/§7).

use std::path::Path;

use ava_platformvm::txs::executor::StakingConfig;
use ava_types::id::Id;

pub mod build;
pub mod chains;
pub mod config;
pub mod error;
pub mod split;
pub mod unparsed;
pub mod validate;

pub use build::{avax_asset_id, from_config, vm_genesis};
pub use config::{Allocation, Config, LockedAmount, Staker};
pub use error::{GenesisError, Result};

/// `genesis.FromFile` — builds a **custom** network's genesis from a JSON
/// config file (validate + build). Rejects Mainnet/Testnet/Local network ids
/// (those use the embedded config, never a file).
///
/// # Errors
/// [`GenesisError::OverridesStandardNetworkConfig`] for standard networks, the
/// load/parse error, the failing [`validate::validate_config`] check, or the
/// build error.
pub fn from_file(
    network_id: u32,
    path: &Path,
    staking_cfg: &StakingConfig,
) -> Result<(Vec<u8>, Id)> {
    validate::check_not_standard_network(network_id)?;
    let config = validate::get_config_file(path)?;
    validate::validate_config(network_id, &config, staking_cfg)?;
    build::from_config(&config)
}

/// `genesis.FromFlag` — builds a **custom** network's genesis from the
/// base64-encoded `--genesis-file-content` flag value (validate + build).
/// Rejects Mainnet/Testnet/Local network ids.
///
/// # Errors
/// As [`from_file`], with [`GenesisError::InvalidBase64`] for a bad encoding.
pub fn from_flag(
    network_id: u32,
    content_b64: &str,
    staking_cfg: &StakingConfig,
) -> Result<(Vec<u8>, Id)> {
    validate::check_not_standard_network(network_id)?;
    let config = validate::get_config_content(content_b64)?;
    validate::validate_config(network_id, &config, staking_cfg)?;
    build::from_config(&config)
}
