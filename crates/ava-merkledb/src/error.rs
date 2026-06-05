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

    // --- Proof errors (Go `x/merkledb/proof.go`) ---
    /// The proof is empty. Go `ErrEmptyProof`.
    #[error("proof is empty")]
    EmptyProof,

    /// The recomputed root didn't match the expected root. Go `ErrInvalidProof`.
    #[error("proof obtained an invalid root ID")]
    InvalidProof,

    /// The proven key has a partial final byte. Go `ErrProofKeyPartialByte`.
    #[error("the provided key has partial byte length")]
    ProofKeyPartialByte,

    /// A node with a partial-byte key carried a value. Go
    /// `ErrPartialByteLengthWithValue`.
    #[error("partial byte length key with value")]
    PartialByteLengthWithValue,

    /// The proven value didn't match the proof node's value digest. Go
    /// `ErrProofValueDoesntMatch`.
    #[error("the provided value does not match the proof node for the provided key's value")]
    ProofValueDoesntMatch,

    /// An exclusion proof carried a value. Go `ErrExclusionProofUnexpectedValue`.
    #[error("exclusion proof's value should be empty")]
    ExclusionProofUnexpectedValue,

    /// A proof node's key isn't a prefix of the proven key. Go
    /// `ErrProofNodeNotForKey`.
    #[error("the provided path has a key that is not a prefix of the specified key")]
    ProofNodeNotForKey,

    /// Proof node keys weren't strictly increasing. Go
    /// `ErrNonIncreasingProofNodes`.
    #[error("each proof node key must be a strict prefix of the next")]
    NonIncreasingProofNodes,

    /// An exclusion proof was missing required end nodes. Go
    /// `ErrExclusionProofMissingEndNodes`.
    #[error("missing end nodes from path")]
    ExclusionProofMissingEndNodes,

    /// An exclusion proof's replacement node was at the wrong index. Go
    /// `ErrExclusionProofInvalidNode`.
    #[error("invalid node for exclusion proof")]
    ExclusionProofInvalidNode,

    /// `start > end` for a range/change proof. Go `ErrStartAfterEnd`.
    #[error("start key is greater than end key")]
    StartAfterEnd,

    /// A range proof's `max_length` was not positive. Go `ErrInvalidMaxLength`.
    #[error("expected max length to be > 0")]
    InvalidMaxLength,

    /// Range/change proof keys weren't strictly increasing. Go
    /// `ErrNonIncreasingValues`.
    #[error("keys sent are not in increasing order")]
    NonIncreasingValues,

    /// A key fell outside the requested `[start, end]` range. Go
    /// `ErrStateFromOutsideOfRange`.
    #[error("state key falls outside of the start->end range")]
    StateFromOutsideOfRange,

    /// A proof node's key length disagreed with its byte length. Go
    /// `errInvalidKeyLength`.
    #[error("key length doesn't match bytes length, check specified branchFactor")]
    InvalidKeyLength,

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
