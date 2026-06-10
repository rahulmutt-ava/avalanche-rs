// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The per-crate error enum (specs 12 §11, 00 §8).

/// Errors produced by the configuration subsystem.
///
/// Variants mirror the Go `config/` sentinel errors one-for-one where the Go
/// side has a named error; parse-shaped failures carry the offending key and
/// input so callers can render Go-equivalent messages.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A value failed to parse as a Go `time.ParseDuration` duration.
    #[error("time: invalid duration {input:?}")]
    InvalidDuration {
        /// The raw input string.
        input: String,
    },

    /// A duration string had a valid grammar but a missing unit suffix
    /// (Go: `time: missing unit in duration`).
    #[error("time: missing unit in duration {input:?}")]
    MissingDurationUnit {
        /// The raw input string.
        input: String,
    },

    /// A duration string had a valid grammar but an unknown unit suffix
    /// (Go: `time: unknown unit`).
    #[error("time: unknown unit {unit:?} in duration {input:?}")]
    UnknownDurationUnit {
        /// The unrecognized unit token.
        unit: String,
        /// The raw input string.
        input: String,
    },

    /// A negative duration. Valid in Go (`time.Duration` is signed), but
    /// `std::time::Duration` is unsigned; no avalanchego flag default is
    /// negative, and the durations are validated non-negative at parse time
    /// anyway (13 §6/§11/§12).
    #[error("negative duration {input:?} is not supported")]
    NegativeDurationUnsupported {
        /// The raw input string.
        input: String,
    },
}

/// Crate-local result alias.
pub type Result<T, E = ConfigError> = std::result::Result<T, E>;
