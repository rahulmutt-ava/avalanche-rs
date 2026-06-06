// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Crate error model (specs 06 §9).
//!
//! A single `thiserror` enum carries the Snowman/snowball sentinels asserted by
//! the consensus tests via `assert_matches!`. A `record_poll`/`accept` returning
//! `Err` is a **critical** error: the engine logs and the chain halts, matching
//! Go which propagates the error up and shuts the chain.

/// Errors produced by the `ava-snow` consensus core.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A block already present in the consensus instance was added again
    /// (Go `errDuplicateAdd`).
    #[error("duplicate block add")]
    DuplicateAdd,

    /// A block whose parent is not tracked by the consensus instance was added
    /// (Go `errUnknownParentBlock`).
    #[error("unknown parent block")]
    UnknownParentBlock,

    /// The number of processing blocks exceeded `max_outstanding_items`
    /// (health; Go `errTooManyProcessingBlocks`).
    #[error("too many processing blocks")]
    TooManyProcessingBlocks,

    /// A block has been processing for longer than `max_item_processing_time`
    /// (health; Go `errBlockProcessingTooLong`).
    #[error("block processing too long")]
    BlockProcessingTooLong,

    /// The average block acceptance time exceeded the health threshold
    /// (health; Go `errAcceptanceTimeTooHigh`).
    #[error("acceptance time too high")]
    AcceptanceTimeTooHigh,

    /// The snowball [`crate::snowball::Parameters`] failed validation
    /// (Go `parameters.Verify`). Carries a human-readable reason.
    #[error("invalid snowball parameters: {0}")]
    ParametersInvalid(String),

    /// Multiple errors joined together (Go `errors.Join`, used by health
    /// checks). Mirrors the joined-error shape of the Go consensus health path.
    #[error("multiple errors: {0:?}")]
    Multiple(Vec<Error>),
}

/// Convenience result alias for `ava-snow` (specs 00 §7.1).
pub type Result<T> = std::result::Result<T, Error>;
