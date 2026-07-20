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
    /// `Network.AddHandler`/`router.addHandler` was called twice for the same
    /// handler id (Go `network/p2p/router.go:88-104`'s
    /// `ErrExistingAppProtocol`, wrapped as `"failed to register handler id
    /// %d: %w"`).
    #[error("failed to register handler id {0}: existing app protocol")]
    DuplicateHandler(u64),
    /// `Client::app_request` allocated a request id that is still awaiting a
    /// response/failure in the pending map (Go `network/p2p/client.go:79-88`'s
    /// `ErrRequestPending`, wrapped as `"failed to issue request with request
    /// id %d: %w"`). The stale pending entry is left untouched — only the new
    /// request is rejected.
    #[error("failed to issue request with request id {0}: request pending")]
    RequestPending(u32),
}

/// Convenience alias for `Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;
