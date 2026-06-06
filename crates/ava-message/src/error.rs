// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Crate error type. Preserves the Go sentinels (`message/*.go`,
//! `network/peer/msg_length.go`) as typed variants (specs/05 §8); tests assert
//! via `assert_matches!`, never string compares.

/// Result alias for `ava-message`.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors produced by the p2p message codec / framing.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The `p2p.Message` oneof was unset or carried a variant with no `Op`
    /// mapping (mirrors Go `errUnknownMessageType`).
    #[error("unknown message type")]
    UnknownOp,

    /// The declared frame length exceeds the maximum allowed message size
    /// (mirrors Go `errMaxMessageLengthExceeded`).
    #[error("maximum message length exceeded; the message length {len} exceeds the specified limit {max}")]
    MaxMessageLengthExceeded {
        /// The declared/attempted message length in bytes.
        len: u32,
        /// The configured maximum message size in bytes.
        max: u32,
    },

    /// The length prefix was not exactly 4 bytes (mirrors Go
    /// `errInvalidMessageLength`).
    #[error("invalid message length: {0}")]
    InvalidMessageLength(String),

    /// The outer `Message` was compressed with an algorithm the codec does not
    /// support (mirrors Go `errUnknownCompressionType`). Only zstd is produced;
    /// gzip is decode-only legacy tolerance (specs/05 §1.3).
    #[error("message is compressed with an unknown compression type")]
    UnknownCompressionType,

    /// A protobuf decode failure on the (outer or inner) `Message`.
    #[error("protobuf decode failed: {0}")]
    ProtoDecode(#[from] prost::DecodeError),

    /// A zstd (de)compression failure, or a decompressed payload that would
    /// exceed `MAX_MESSAGE_SIZE` (guards against decompression-bomb over-reads).
    #[error("compression error: {0}")]
    Compression(String),
}
