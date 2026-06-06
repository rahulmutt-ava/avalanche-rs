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
}

/// Convenience alias for engine results.
pub type Result<T> = std::result::Result<T, Error>;
