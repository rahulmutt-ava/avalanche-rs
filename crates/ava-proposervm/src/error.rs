// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! ProposerVM error model.
//!
//! Mirrors the sentinel errors in Go `vms/proposervm/block` (block.go) and
//! `vms/proposervm/proposer` (windower.go).

use thiserror::Error;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors surfaced by ProposerVM block parsing/verification and the windower.
#[derive(Debug, Error)]
pub enum Error {
    /// A signature was provided on a block that carries no certificate
    /// (Go `errUnexpectedSignature`).
    #[error("signature provided when none was expected")]
    UnexpectedSignature,

    /// The embedded staking certificate failed to parse (Go
    /// `errInvalidCertificate`).
    #[error("invalid certificate: {0}")]
    InvalidCertificate(String),

    /// A Granite block carried a zero `Epoch` (Go `errZeroEpoch`).
    #[error("epoch must be provided after granite")]
    ZeroEpoch,

    /// The block signature did not verify against the proposer's certificate.
    #[error("signature verification failed")]
    SignatureVerifyFailed,

    /// A codec (de)serialization error.
    #[error("codec error: {0}")]
    Codec(String),

    /// The decoded codec version did not match the expected version.
    #[error("expected codec version {expected} but got {got}")]
    WrongCodecVersion {
        /// The version the codec expected.
        expected: u16,
        /// The version actually decoded.
        got: u16,
    },

    /// No validators are currently available; anyone can propose
    /// (Go `ErrAnyoneCanPropose`).
    #[error("anyone can propose")]
    AnyoneCanPropose,

    /// The deterministic sampler failed unexpectedly
    /// (Go `ErrUnexpectedSamplerFailure`).
    #[error("unexpected sampler failure")]
    UnexpectedSamplerFailure,

    /// Summing validator weights overflowed `u64`.
    #[error("validator weight overflow")]
    WeightOverflow,

    /// The underlying `ValidatorState` lookup failed.
    #[error("validator state error: {0}")]
    ValidatorState(String),
}
