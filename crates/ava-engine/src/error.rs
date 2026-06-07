// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The crate-level [`Error`]/[`Result`] returned by the engine op handlers.
//!
//! This is **distinct** from [`AppError`](crate::common::error::AppError): a
//! handler method returning `Err(Error)` is a *fatal* engine error (Go returns a
//! non-nil `error` from a `Handler` method, which tears the chain down), whereas
//! an `AppError` is an application-level value carried *inside* a successful
//! `AppRequestFailed`/`SendAppError` flow.

/// A fatal consensus-engine error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A handler or sender operation failed.
    #[error("engine error: {0}")]
    Engine(String),

    /// An error bubbled up from the underlying VM.
    #[error("vm error: {0}")]
    Vm(#[from] ava_vm::error::Error),

    /// An error bubbled up from the consensus core.
    #[error("consensus error: {0}")]
    Consensus(#[from] ava_snow::error::Error),

    /// An error bubbled up from the validator subsystem.
    #[error("validators error: {0}")]
    Validators(#[from] ava_validators::error::Error),

    /// The engine was halted (the [`tokio_util::sync::CancellationToken`] fired).
    #[error("engine halted")]
    Halted,
}

/// Convenience alias for engine results.
pub type Result<T> = std::result::Result<T, Error>;
