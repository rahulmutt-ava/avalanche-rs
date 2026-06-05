// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-version` — client version, compatibility, and upgrade schedule.
//!
//! Tier T0 (primitives). Owning specs: `specs/03-core-primitives.md` §5, §11,
//! `specs/26-versioning-and-compatibility.md`. Implemented across M0:
//!
//! - [`application`] — `Application` + version constants (`CLIENT="avalanchego"`,
//!   `RPC_CHAIN_VM_PROTOCOL=45`, `CURRENT`, ...) (M0.22)
//! - [`compatibility`] — version-vs-upgrade-time compatibility (M0.22)
//! - [`upgrade`] — `UpgradeConfig` + `Fork` + activation schedule (M0.23)
//! - [`error`] — the crate error enum
//!
//! NOTE: the wire/P2P client string is `avalanchego` (drop-in interop); the
//! local CLI prints `avalanchers/<ver>` (see `crates/avalanchers`). M0.22 wires
//! the binary's `--version` to `CURRENT`'s numeric version with the local prefix.

#![forbid(unsafe_code)]

pub mod application;
pub mod compatibility;
pub mod error;
pub mod upgrade;

// ── Flat re-exports for ergonomic use ────────────────────────────────────────

pub use application::{
    Application,
    APPLICATION_NAME,
    CLIENT,
    CURRENT,
    CURRENT_DATABASE,
    MINIMUM_COMPATIBLE,
    PREV_DATABASE,
    PREV_MINIMUM_COMPATIBLE,
    RPC_CHAIN_VM_PROTOCOL,
};
pub use error::{Error, Result};
