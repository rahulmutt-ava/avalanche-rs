// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Crate error enum.
//!
//! The codec decode-rejection variants mirror the sentinel errors in Go
//! `x/merkledb/codec.go` (their *behavior* is part of the conformance surface:
//! a Go node and a Rust node must reject the same bad bytes).

/// Result alias for this crate.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors produced by `ava-merkledb`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// A varint had leading zeroes (non-canonical). Go `errLeadingZeroes`.
    #[error("varint has leading zeroes")]
    LeadingZeroes,

    /// A decoded bool byte was neither `0x00` nor `0x01`. Go `errInvalidBool`.
    #[error("decoded bool is neither true nor false")]
    InvalidBool,

    /// A key's partial final byte had non-zero padding bits.
    /// Go `errNonZeroKeyPadding`.
    #[error("key partial byte should be padded with 0s")]
    NonZeroKeyPadding,

    /// Trailing bytes remained after decoding. Go `errExtraSpace`.
    #[error("trailing buffer space")]
    ExtraSpace,

    /// A decoded length/value overflowed the platform `usize`/`int`.
    /// Go `errIntOverflow`.
    #[error("value overflows int")]
    IntOverflow,

    /// More children than the largest branch factor allows.
    /// Go `errTooManyChildren`.
    #[error("too many children")]
    TooManyChildren,

    /// A child index was out of range, duplicated, or out of order.
    /// Go `errChildIndexTooLarge`.
    #[error("invalid child index. Must be less than branching factor")]
    ChildIndexTooLarge,

    /// The buffer ended before a value could be fully decoded.
    /// Go `io.ErrUnexpectedEOF`.
    #[error("unexpected EOF")]
    UnexpectedEof,
}
