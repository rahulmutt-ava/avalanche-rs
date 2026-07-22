// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Error model for the validator subsystem (`thiserror` enum + [`Result`]).
//!
//! Mirrors the sentinel errors `snow/validators` returns (`ErrMissingValidators`,
//! `ErrWeightOverflow`, the duplicate/under-flow add-weight guards). Weight
//! arithmetic flows through [`ava_utils::math`]; an overflow there is surfaced as
//! [`Error::WeightOverflow`].

/// Convenience alias for the validator subsystem.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the validator subsystem.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Summing weights overflowed `u64` (Go `safemath.ErrOverflow` /
    /// `errOverflow`). Returned by `total_weight`/`subset_weight`/`add_weight`.
    #[error("total weight overflows u64")]
    WeightOverflow,

    /// Removing more weight than a validator holds, or removing weight from an
    /// absent validator (Go `errs` around `RemoveWeight`).
    #[error("cannot remove {requested} weight from validator with {present} weight")]
    WeightUnderflow {
        /// Weight that was requested to be removed.
        requested: u64,
        /// Weight the validator currently holds (0 if absent).
        present: u64,
    },

    /// Adding a staker whose `NodeId` already exists in the subnet (Go
    /// `errDuplicateValidator`).
    #[error("validator {node_id} already exists in subnet")]
    DuplicateValidator {
        /// Hex of the offending node id.
        node_id: String,
    },

    /// Adding zero weight (Go `errZeroWeight`).
    #[error("cannot add a validator with zero weight")]
    ZeroWeight,

    /// The requested subnet has no validators registered (Go
    /// `ErrMissingValidators`). Returned by `sample` / the `ValidatorState`
    /// adapters when a height/subnet is unknown.
    #[error("missing validators for subnet")]
    MissingValidators,

    /// The deterministic sampler could not satisfy the request: `size` exceeds
    /// the TOTAL WEIGHT of the set (the sampler draws weight units without
    /// replacement, so the boundary is total weight, not validator count),
    /// mirroring Go's `sampler.Sample` returning `ok == false` /
    /// `errInsufficientWeight`.
    #[error("insufficient weight to sample {requested}")]
    InsufficientValidators {
        /// Number of weight units (sample size) requested.
        requested: usize,
    },

    /// A [`ValidatorState`](crate::state::ValidatorState) query targeted a
    /// height that is not yet finalized (the current P-Chain height is below the
    /// requested height). Go `errUnfinalizedHeight`; returned, never panicked.
    #[error("failed to fetch validator set at unfinalized height")]
    UnfinalizedHeight,

    /// A [`ValidatorState`](crate::state::ValidatorState) implementation failed
    /// while reading its backing state (e.g. a missing block / tx / diff, or a
    /// reconstruction-arithmetic overflow). The message preserves the
    /// implementation-specific failure for diagnostics; callers needing the
    /// precise sentinel match on the implementation's own error type.
    #[error("validator state error: {message}")]
    State {
        /// The backing-state failure, rendered.
        message: String,
    },
}

impl From<ava_utils::error::Error> for Error {
    fn from(_: ava_utils::error::Error) -> Self {
        // The only fallible math on this path is the checked weight sum.
        Error::WeightOverflow
    }
}
