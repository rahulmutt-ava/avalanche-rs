// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Error type for `ava-p2p` (port of Go `network/p2p`'s ad hoc errors).

/// Errors produced by the `ava-p2p` crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A proto message failed to decode.
    #[error("decode: {0}")]
    Decode(String),
    /// A message failed to send (e.g. the send queue/channel was closed).
    #[error("send: {0}")]
    Send(String),
    /// A set/bloom-filter operation failed.
    #[error("set: {0}")]
    Set(String),
}

/// Convenience alias for `Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;
