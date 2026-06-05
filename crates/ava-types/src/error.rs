// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-types` error enum (`thiserror`).
//!
//! Mirrors the Go sentinel errors in `ids/` (`errInvalidHashLen`,
//! `ErrNoIDWithAlias`, `errAliasAlreadyMapped`, `errShortNodeID`,
//! `errMissingQuotes`). Owning spec: `specs/03-core-primitives.md` §7.

use thiserror::Error;

/// Crate-wide error type for `ava-types`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    /// A byte slice had the wrong length to form a fixed-size id.
    /// Mirrors Go `hashing.ToHash256`/`ToHash160` length checks.
    #[error("invalid hash length: expected {expected}, got {actual}")]
    InvalidHashLen {
        /// Expected length in bytes.
        expected: usize,
        /// Actual length in bytes.
        actual: usize,
    },

    /// No id is registered for the requested alias.
    /// Mirrors Go `ids.ErrNoIDWithAlias`.
    #[error("there is no ID with alias: {0}")]
    NoIdWithAlias(String),

    /// An alias is already mapped to an id.
    /// Mirrors Go `ids.errAliasAlreadyMapped`.
    #[error("alias already mapped to an ID: {0}")]
    AliasAlreadyMapped(String),

    /// A `NodeID-` string was missing its required prefix (or was too short).
    /// Mirrors Go `ids.errShortNodeID`.
    #[error("missing the prefix: {0}")]
    ShortNodeId(String),

    /// A JSON id string was missing its surrounding quotes.
    /// Mirrors Go `ids.errMissingQuotes`.
    #[error("first and last characters should be quotes")]
    MissingQuotes,

    /// A CB58 decode error from `ava-utils::cb58` (bad base58, bad checksum, etc.).
    /// Wraps [`ava_utils::error::Error`].
    #[error("cb58 decode error: {0}")]
    Cb58(#[from] ava_utils::error::Error),
}

/// Crate-wide `Result` alias.
pub type Result<T> = core::result::Result<T, Error>;
