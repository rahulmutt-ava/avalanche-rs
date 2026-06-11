// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-indexer` error enum.
//!
//! Mirrors the sentinel errors of Go `indexer/index.go` byte-for-byte where the
//! string reaches an RPC client (gorilla maps a handler error to a `-32000`
//! whose `message` is the error's `Error()` string — 14 §16.1), so `Display`
//! strings here are part of the wire-compat surface.

use ava_codec::error::CodecError;
use ava_types::id::Id;

use crate::index::MAX_FETCHED_BY_RANGE;

/// Errors produced by the indexer (Go `indexer/index.go` + `indexer.go`).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Go `errNoneAccepted`.
    #[error("no containers have been accepted")]
    NoneAccepted,

    /// Go `errNumToFetchInvalid` wrapped as `"%w but is %d"`.
    #[error("numToFetch must be in [1,{MAX_FETCHED_BY_RANGE}] but is {0}")]
    NumToFetchInvalid(u64),

    /// Go `errNoContainerAtIndex` wrapped as `"%w %d"`.
    #[error("no container at index {0}")]
    NoContainerAtIndex(u64),

    /// Go `index.go::GetContainerRange`'s start-index bound check.
    #[error("start index ({start}) > last accepted index ({last})")]
    StartIndexTooHigh {
        /// The requested start index.
        start: u64,
        /// The last accepted index.
        last: u64,
    },

    /// A bare database error (Go returns `database.ErrNotFound` & co.
    /// unwrapped from e.g. `GetContainerByID`/`GetIndex`).
    #[error(transparent)]
    Database(#[from] ava_database::Error),

    /// Go `index.go::getContainerByIndexBytes`'s read wrap.
    #[error("couldn't read from database: {0}")]
    ReadFailed(ava_database::Error),

    /// Go `index.go::Accept`'s serialize wrap.
    #[error("couldn't serialize container {id}: {source}")]
    SerializeContainer {
        /// The container that failed to serialize.
        id: Id,
        /// The codec failure.
        source: CodecError,
    },

    /// Go `index.go::getContainerByIndexBytes`'s unmarshal wrap.
    #[error("couldn't unmarshal container: {0}")]
    UnmarshalContainer(CodecError),

    /// Mounting an index API route failed (Go `registerChainHelper`'s
    /// `AddRoute` error path).
    #[error("couldn't add route to index API: {0}")]
    Route(String),
}

/// The per-crate result alias.
pub type Result<T> = std::result::Result<T, Error>;
