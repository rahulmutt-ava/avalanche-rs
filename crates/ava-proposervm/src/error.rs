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

    /// The proposer's signer failed to produce a signature (Go `key.Sign`).
    #[error("failed to sign block: {0}")]
    SignFailed(String),

    /// A key was not present in the proposervm state DB (Go
    /// `database.ErrNotFound`).
    #[error("not found")]
    NotFound,

    /// The proposervm state database returned an error.
    #[error("database error: {0}")]
    Database(String),

    /// The inner (wrapped) VM returned an error during delegation.
    #[error("inner VM error: {0}")]
    InnerVm(String),

    /// It is not this node's slot to propose a block yet (Go
    /// `errUnexpectedProposer` / `errProposerWindowNotStarted`).
    #[error("not this node's slot to propose")]
    NotProposer,
}

impl From<ava_database::Error> for Error {
    fn from(e: ava_database::Error) -> Self {
        match e {
            ava_database::Error::NotFound => Error::NotFound,
            other => Error::Database(format!("{other:?}")),
        }
    }
}

// The `ChainVm`/`Block` trait surfaces return `ava_vm::Error` / `ava_snow::Error`
// respectively; map the proposervm error onto those crates' (closed) enums. The
// orphan rule permits these `From` impls because the source type is local.
//
// Neither `ava_vm::Error` nor `ava_snow::Error` exposes a free-form `Other`
// variant, so non-`NotFound` proposervm errors collapse onto the nearest
// string-carrying / structural variant. `NotFound` round-trips exactly.
impl From<Error> for ava_vm::error::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::NotFound => ava_vm::error::Error::NotFound,
            // No generic string variant exists on `ava_vm::Error`; surface a
            // stable, descriptive static message (the detailed message is in the
            // proposervm log path, not the engine-facing error).
            _ => ava_vm::error::Error::InvalidComponent("proposervm build/state error"),
        }
    }
}

impl From<Error> for ava_snow::error::Error {
    fn from(e: Error) -> Self {
        // `ava_snow::Error::ParametersInvalid(String)` is the only string-carrying
        // variant; reuse it to preserve the proposervm error message on the
        // critical accept/verify path (a returned `Err` halts the chain).
        ava_snow::error::Error::ParametersInvalid(format!("proposervm: {e}"))
    }
}
