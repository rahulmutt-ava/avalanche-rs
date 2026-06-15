// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-version` error enum (`thiserror`).
//!
//! Owning spec: `specs/03-core-primitives.md` §7.

use thiserror::Error;

/// Crate-wide error type for `ava-version`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    /// The upgrade schedule has fork times that are not monotonically non-decreasing.
    ///
    /// Mirrors Go `upgrade.errInvalidUpgradeTimes` / `upgrade_test.go` rejection
    /// of out-of-order configs. (`upgrade/upgrade.go`)
    #[error("upgrade times are not monotonically non-decreasing")]
    InvalidUpgradeTimes,

    /// The embedded `compatibility.json` could not be parsed into the
    /// rpcchainvm-protocol → versions table.
    ///
    /// Mirrors a failure to load Go's `//go:embed compatibility.json`
    /// (`version/compatibility.go`). The embedded file is a committed,
    /// byte-parity-tested input, so this should never occur in practice.
    #[error("invalid embedded compatibility.json: {0}")]
    CompatibilityTable(String),
}

/// Crate-wide `Result` alias.
pub type Result<T> = core::result::Result<T, Error>;
