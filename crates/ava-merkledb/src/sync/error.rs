// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! State-sync error model (spec 19 §4). Mirrors the Go sentinels in
//! `database/merkle/sync/{syncer,db}.go` so a Rust caller can match exactly
//! where Go uses `errors.Is`.

use ava_types::id::Id;

use crate::error::Error as MerkleError;

/// Result alias for the sync protocol.
pub type SyncResult<T> = core::result::Result<T, SyncError>;

/// Errors produced by the merkledb state-sync protocol.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SyncError {
    /// Syncing completed but the resulting root didn't match the target.
    /// Go `ErrFinishedWithUnexpectedRoot`.
    #[error("finished syncing with an unexpected root: expected {expected:?}, got {got:?}")]
    FinishedWithUnexpectedRoot {
        /// The target root we were syncing toward.
        expected: Id,
        /// The root we actually ended up with.
        got: Id,
    },

    /// The server doesn't have enough history to generate the requested proof.
    /// Go `ErrInsufficientHistory`.
    #[error("insufficient history to generate proof")]
    InsufficientHistory,

    /// The server's history doesn't contain the requested end root. Go
    /// `ErrNoEndRoot` (wraps `ErrInsufficientHistory`).
    #[error("insufficient history to generate proof: end root not found")]
    NoEndRoot,

    /// A returned range proof failed verification. Go `errInvalidRangeProof`.
    #[error("failed to verify range proof")]
    InvalidRangeProof,

    /// A returned change proof failed verification. Go `errInvalidChangeProof`.
    #[error("failed to verify change proof")]
    InvalidChangeProof,

    /// A response carried more than the requested number of bytes. Go
    /// `errTooManyBytes`.
    #[error("response contains more than requested bytes")]
    TooManyBytes,

    /// A proof for the empty trie was requested. Go `errEmptyProof`.
    #[error("proof for empty trie requested")]
    EmptyProof,

    /// A request's `bytes_limit` was zero. Go `errInvalidBytesLimit`.
    #[error("bytes limit must be greater than 0")]
    InvalidBytesLimit,

    /// A request's `key_limit` was zero. Go `errInvalidKeyLimit`.
    #[error("key limit must be greater than 0")]
    InvalidKeyLimit,

    /// A request carried a malformed root hash (wrong length). Go
    /// `errInvalidRootHash`/`errInvalidStartRootHash`/`errInvalidEndRootHash`.
    #[error("root hash must have length 32")]
    InvalidRootHash,

    /// `start > end` in a request's bounds. Go `errInvalidBounds`.
    #[error("start key is greater than end key")]
    InvalidBounds,

    /// No proof could be generated within the requested byte limit. Go
    /// `errMinProofSizeIsTooLarge`.
    #[error("cannot generate any proof within the requested limit")]
    MinProofSizeTooLarge,

    /// A wire frame failed to decode. Go `proto.Unmarshal` failure.
    #[error("failed to decode proof frame: {0}")]
    Decode(String),

    /// The sync driver was cancelled / closed before completing.
    #[error("syncer is closed")]
    Closed,

    /// An error surfaced by the underlying merkledb layer.
    #[error("merkledb error: {0}")]
    Merkle(#[from] MerkleError),
}
