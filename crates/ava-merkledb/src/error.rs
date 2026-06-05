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

    /// The trie this view was based on has changed, rendering this view
    /// invalid. Go `ErrInvalid`.
    #[error("the trie this view was based on has changed, rendering this view invalid")]
    Invalid,

    /// A view has already been committed. Go `ErrCommitted`.
    #[error("cannot commit a view that has already been committed")]
    Committed,

    /// A view's parent is not the database, so it cannot be committed directly.
    /// Go `ErrParentNotDatabase`.
    #[error("a view's parent must be the database being committed to")]
    ParentNotDatabase,

    /// The database has been closed. Mirrors `database.ErrClosed`.
    #[error("closed")]
    Closed,

    /// An error surfaced by the base `Database`.
    #[error("database error: {0}")]
    Database(String),
}

impl From<ava_database::Error> for Error {
    fn from(e: ava_database::Error) -> Self {
        match e {
            ava_database::Error::Closed => Error::Closed,
            // NotFound is control flow at the merkledb layer; callers translate
            // it to "absent". We keep a distinct message for any leak.
            ava_database::Error::NotFound => Error::Database("not found".to_string()),
            ava_database::Error::Other(e) => Error::Database(e.to_string()),
        }
    }
}
