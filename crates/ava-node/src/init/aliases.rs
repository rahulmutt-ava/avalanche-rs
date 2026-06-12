// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 23 (specs/12 §2.2): the default chain aliases and the API-path
//! aliases (mirror Go `initChainAliases` / `initAPIAliases` over
//! `genesis.Aliases`).
//!
//! Go re-parses the platform genesis to discover the X/C chain IDs;
//! `Node::new` already derived them in step 20 (`init_chain_manager`), so both
//! steps take the IDs instead of re-parsing.

use std::collections::HashMap;

use ava_api::server::ApiServer;
use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::init::chain_manager::{AssemblyChainManager, PLATFORM_CHAIN_ID};

/// Go `genesis.PChainAliases`.
pub const P_CHAIN_ALIASES: [&str; 2] = ["P", "platform"];
/// Go `genesis.XChainAliases`.
pub const X_CHAIN_ALIASES: [&str; 2] = ["X", "avm"];
/// Go `genesis.CChainAliases`.
pub const C_CHAIN_ALIASES: [&str; 2] = ["C", "evm"];

/// Go `constants.ChainAliasPrefix` — API chain mounts live at
/// `/ext/bc/<chainID>`.
pub const CHAIN_ALIAS_PREFIX: &str = "bc";

/// Step 23a: register the default chain aliases (P/X/C) plus the configured
/// `--chain-aliases-file` entries on the chain manager (mirror Go
/// `initChainAliases`).
///
/// # Errors
/// [`Error::ChainAlias`] when an alias conflicts (e.g. a user alias colliding
/// with a default).
pub fn init_chain_aliases(
    manager: &AssemblyChainManager,
    x_chain_id: Id,
    c_chain_id: Id,
    config_aliases: &HashMap<Id, Vec<String>>,
) -> Result<()> {
    tracing::info!("initializing chain aliases");

    for alias in P_CHAIN_ALIASES {
        manager
            .alias(PLATFORM_CHAIN_ID, alias)
            .map_err(Error::ChainAlias)?;
    }
    for alias in X_CHAIN_ALIASES {
        manager.alias(x_chain_id, alias).map_err(Error::ChainAlias)?;
    }
    for alias in C_CHAIN_ALIASES {
        manager.alias(c_chain_id, alias).map_err(Error::ChainAlias)?;
    }
    for (chain_id, aliases) in config_aliases {
        for alias in aliases {
            manager.alias(*chain_id, alias).map_err(Error::ChainAlias)?;
        }
    }
    Ok(())
}

/// The API aliases of one chain mount: `<short>`, `<long>`, `bc/<short>`,
/// `bc/<long>` all resolve `/ext/bc/<chainID>` (Go `genesis.Aliases`).
fn api_aliases_of(aliases: [&str; 2]) -> Vec<String> {
    let mut out = Vec::with_capacity(4);
    for alias in aliases {
        out.push(alias.to_owned());
    }
    for alias in aliases {
        out.push(format!("{CHAIN_ALIAS_PREFIX}/{alias}"));
    }
    out
}

/// Step 23b: register the API-path aliases for the P/X/C mounts on the HTTP
/// server (mirror Go `initAPIAliases`).
///
/// # Errors
/// [`crate::error::Error::ApiServer`] when an alias path is already taken.
pub fn init_api_aliases(
    api_server: &dyn ApiServer,
    x_chain_id: Id,
    c_chain_id: Id,
) -> Result<()> {
    tracing::info!("initializing API aliases");

    for (chain_id, aliases) in [
        (PLATFORM_CHAIN_ID, P_CHAIN_ALIASES),
        (x_chain_id, X_CHAIN_ALIASES),
        (c_chain_id, C_CHAIN_ALIASES),
    ] {
        let endpoint = format!("{CHAIN_ALIAS_PREFIX}/{chain_id}");
        api_server.add_aliases(&endpoint, &api_aliases_of(aliases))?;
    }
    Ok(())
}
