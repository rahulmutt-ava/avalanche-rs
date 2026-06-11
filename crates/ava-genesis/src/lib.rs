// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

#![forbid(unsafe_code)]
// ava-evm and ava-version are dev-dependencies used only in integration tests
// (tests/golden_genesis_extras.rs); the lib itself does not import them, so the
// `unused_crate_dependencies` lint fires at the lib level. This allow is the
// standard workaround until Rust supports per-target dep declarations.
#![allow(unused_crate_dependencies)]

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

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use ava_platformvm::txs::executor::StakingConfig;
use ava_types::id::Id;

pub mod bootstrappers;
pub mod build;
pub mod chains;
pub mod config;
pub mod error;
pub mod recent_start;
pub mod split;
pub mod unparsed;
pub mod validate;

pub use bootstrappers::{Bootstrapper, bootstrappers, sample_bootstrappers};
pub use build::{avax_asset_id, from_config, vm_genesis};
pub use config::{Allocation, Config, LockedAmount, Staker, get_config};
pub use error::{GenesisError, Result};

/// A chain whose genesis identity can be derived from the network genesis
/// (specs 23 §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// The Platform Chain (its genesis id == `sha256(p_chain_genesis_bytes)`).
    P,
    /// The X-Chain (its blockchain id == the AVM `CreateChainTx` id).
    X,
    /// The C-Chain (its blockchain id == the EVM `CreateChainTx` id).
    C,
}

/// A cached `(p_chain_genesis_bytes, avax_asset_id)` build result.
type BuiltGenesis = Arc<(Vec<u8>, Id)>;

/// Per-network cache of the embedded-config build results (specs 23 §6 —
/// `from_config` is pure and computed once per network id).
static EMBEDDED_GENESIS: LazyLock<Mutex<HashMap<u32, BuiltGenesis>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `(p_chain_genesis_bytes, avax_asset_id)` for a network: the custom config
/// when given, else [`config::get_config`]`(network_id)` (cached per network
/// id). Note Go parity: Local resolves to the **live** start-time-advanced
/// config, so its P-Chain genesis id is time-dependent; the golden tests pin
/// [`config::UNMODIFIED_LOCAL_CONFIG`] via [`from_config`] directly
/// (specs 23 §5.1).
///
/// # Errors
/// Propagates the [`build::from_config`] error.
pub fn genesis_bytes(network_id: u32, custom: Option<&Config>) -> Result<(Vec<u8>, Id)> {
    if let Some(config) = custom {
        return build::from_config(config);
    }
    let mut cache = EMBEDDED_GENESIS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = cache.get(&network_id) {
        return Ok((cached.0.clone(), cached.1));
    }
    let built = Arc::new(build::from_config(&config::get_config(network_id))?);
    cache.insert(network_id, Arc::clone(&built));
    Ok((built.0.clone(), built.1))
}

/// The genesis identity of `chain` on `network_id` (specs 23 §4/§7):
/// `Chain::P` ⇒ `sha256(p_chain_genesis_bytes)` (the P-Chain genesis block's
/// parent / the `TestGenesis` golden id); `Chain::X`/`Chain::C` ⇒ the matching
/// `CreateChainTx` id (the blockchain id; start-time-independent, so fixed
/// even for the live Local config).
///
/// # Errors
/// Propagates the build/parse error.
pub fn genesis_block_id(network_id: u32, chain: Chain) -> Result<Id> {
    let (p_bytes, _avax_asset_id) = genesis_bytes(network_id, None)?;
    match chain {
        Chain::P => Ok(ava_platformvm::genesis::genesis_id(&p_bytes)),
        Chain::X => Ok(build::vm_genesis(&p_bytes, chains::avm_id())?.id()),
        Chain::C => Ok(build::vm_genesis(&p_bytes, chains::evm_id())?.id()),
    }
}

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
