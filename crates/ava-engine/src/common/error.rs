// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`AppError`] — the typed application error carried on
//! `AppRequestFailed`/`SendAppError` (port of `snow/engine/common/error.go`,
//! specs 06 §4.1).

/// `snow/engine/common.AppError` — an application-defined error matched by its
/// integer `code` (not by structural variant), mirroring Go's `(*AppError).Is`,
/// which compares only `Code`.
///
/// This intentionally matches the shape defined in `ava-vm` (`vms` inbound side)
/// so the two round-trip identically over `proto/vm`/`proto/appsender`. The
/// predefined codes keep the exact Go integer values.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct AppError {
    /// Application-defined error code, used for matching. Negative codes are
    /// reserved by the framework ([`AppError::TIMEOUT`]).
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
}

impl AppError {
    /// `ErrUndefined.Code` — an undefined application error.
    pub const UNDEFINED: i32 = 0;
    /// `ErrTimeout.Code` — signals an `AppRequest` response timeout.
    pub const TIMEOUT: i32 = -1;

    /// Constructs an `AppError` from a code and message.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `ErrUndefined` — the predefined `code == 0` error.
    #[must_use]
    pub fn undefined() -> Self {
        Self::new(Self::UNDEFINED, "undefined")
    }

    /// `ErrTimeout` — the predefined `code == -1` error.
    #[must_use]
    pub fn timeout() -> Self {
        Self::new(Self::TIMEOUT, "timed out")
    }

    /// `(*AppError).Is` — two `AppError`s are considered equal iff their codes
    /// match (the message is ignored), matching Go's sentinel comparison.
    #[must_use]
    pub fn is(&self, other: &AppError) -> bool {
        self.code == other.code
    }
}
