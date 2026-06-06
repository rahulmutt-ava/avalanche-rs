// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Error model for the uptime manager/calculator (Go `snow/uptime` sentinels).
//!
//! Mirrors Go's `errAlreadyStartedTracking`, `errNotStartedTracking`, and the
//! `lockedCalculator`'s `errStillBootstrapping`. Backend failures from the
//! [`UptimeState`](super::state::UptimeState) — including the validator
//! `database.ErrNotFound` sentinel — flow through [`Error::Database`].

use ava_database::Error as DbError;

/// Result alias for the uptime subsystem.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the uptime manager/calculator.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `start_tracking` was called while already tracking
    /// (Go `errAlreadyStartedTracking`).
    #[error("already started tracking")]
    AlreadyStartedTracking,

    /// `stop_tracking` was called before tracking began
    /// (Go `errNotStartedTracking`).
    #[error("not started tracking")]
    NotStartedTracking,

    /// The locked calculator was queried before a backing calculator was
    /// installed (Go `errStillBootstrapping`).
    #[error("still bootstrapping")]
    StillBootstrapping,

    /// A failure from the backing [`UptimeState`](super::state::UptimeState),
    /// including the `NotFound` sentinel for non-validators.
    #[error(transparent)]
    Database(#[from] DbError),
}

impl Error {
    /// Returns the underlying [`DbError`] when this is an [`Error::Database`].
    /// Lets callers match the `NotFound` sentinel (the Rust analog of
    /// `errors.Is(err, database.ErrNotFound)`).
    #[must_use]
    pub fn as_db_error(&self) -> Option<&DbError> {
        match self {
            Error::Database(e) => Some(e),
            _ => None,
        }
    }

    /// Whether this is the [`Error::StillBootstrapping`] sentinel.
    #[must_use]
    pub fn is_still_bootstrapping(&self) -> bool {
        matches!(self, Error::StillBootstrapping)
    }
}
