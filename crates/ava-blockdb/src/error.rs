// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Error types for `ava-blockdb`.
//!
//! This crate is intentionally self-contained: it defines its own error enum
//! rather than depending on `ava-database` (specs/04 §5.1). The control-flow
//! sentinels [`Error::Closed`] and [`Error::NotFound`] mirror Go's
//! `database.ErrClosed`/`database.ErrNotFound`; the remaining variants mirror
//! the Go `x/blockdb` sentinels (`errors.go`).

use std::io;

/// Result alias for `ava-blockdb`.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the block store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The database has been closed (Go `database.ErrClosed`).
    #[error("closed")]
    Closed,

    /// No block exists at the requested height (Go `database.ErrNotFound`).
    #[error("not found")]
    NotFound,

    /// The requested block height is invalid (Go `ErrInvalidBlockHeight`).
    #[error("blockdb: invalid block height")]
    InvalidBlockHeight,

    /// Unrecoverable on-disk corruption detected (Go `ErrCorrupted`).
    #[error("blockdb: unrecoverable corruption detected")]
    Corrupted,

    /// The block is too large to store (Go `ErrBlockTooLarge`).
    #[error("blockdb: block size too large")]
    BlockTooLarge,

    /// A configuration value is invalid.
    #[error("blockdb: invalid config: {0}")]
    InvalidConfig(String),

    /// An arithmetic operation would overflow.
    #[error("blockdb: arithmetic overflow: {0}")]
    Overflow(&'static str),

    /// A checksum mismatch was detected while reading a block.
    #[error("blockdb: checksum mismatch: calculated {calculated}, stored {stored}")]
    ChecksumMismatch {
        /// The checksum computed over the read block.
        calculated: u64,
        /// The checksum stored in the block entry header.
        stored: u64,
    },

    /// An underlying I/O error.
    #[error("blockdb: io error: {0}")]
    Io(#[from] io::Error),

    /// A compression/decompression error.
    #[error("blockdb: compression error: {0}")]
    Compression(String),
}
