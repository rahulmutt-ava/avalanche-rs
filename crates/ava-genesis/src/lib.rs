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

pub mod config;
pub mod error;
pub mod unparsed;

pub use config::{Allocation, Config, LockedAmount, Staker};
pub use error::{GenesisError, Result};
