// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-utils` error enum (`thiserror`).
//!
//! Owning spec: `specs/03-core-primitives.md` §7. Covers checked-arithmetic
//! errors (M0.9) and the CB58 codec errors (M0.11).

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by `ava-utils`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Checked arithmetic overflowed the unsigned type (Go `math.ErrOverflow`).
    #[error("overflow")]
    Overflow,

    /// Checked subtraction underflowed below zero (Go `math.ErrUnderflow`).
    #[error("underflow")]
    Underflow,

    /// The base58 layer failed to decode the input (Go `errBase58Decoding`).
    #[error("base58 decoding error: {0}")]
    Base58Decoding(String),

    /// The trailing 4-byte checksum did not match (Go `errBadChecksum`).
    #[error("invalid input checksum")]
    BadChecksum,

    /// The decoded payload is too short to contain a 4-byte checksum
    /// (Go `errMissingChecksum`).
    #[error("input string is smaller than the checksum size")]
    MissingChecksum,

    /// The payload is too large to CB58-encode (Go `errEncodingOverFlow`).
    #[error("encoding overflow")]
    EncodingOverflow,
}
