// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain error model (specs 08 §10).
//!
//! A single `thiserror` [`Error`] enum for the crate. Go's sentinel errors
//! (compared via `errors.Is`) become variants asserted via `assert_matches!`
//! (specs 00 §7.1). New sentinels are added by the wave task that first needs
//! them; the ones seeded here are the cross-cutting ones named in 08 §10.

/// The P-Chain result alias.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors produced across the P-Chain (`vms/platformvm`).
///
/// Variants preserve the Go sentinel names so call sites can pattern-match the
/// exact failure mode (`assert_matches!(err, Error::WrongTxType)`), mirroring
/// `errors.Is(err, errWrongTxType)` in Go.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `errNilTx` — a nil/absent transaction was supplied.
    #[error("tx is nil")]
    NilTx,

    /// `errWrongTxType` — a [`crate`] visitor was invoked for a tx type it does
    /// not handle (the default `Visitor` method).
    #[error("wrong tx type")]
    WrongTxType,

    /// `ErrRemoveStakerTooEarly` — attempted to remove a staker before its end
    /// time / the chain's current time.
    #[error("attempted to remove staker before its end time")]
    RemoveStakerTooEarly,

    /// `ErrMutatedL1Validator` — an L1 validator's immutable fields were changed.
    #[error("L1 validator immutable fields were mutated")]
    MutatedL1Validator,

    /// `ErrConflictingL1Validator` — an L1 validator conflicts with an existing
    /// entry (duplicate validation ID / node).
    #[error("conflicting L1 validator")]
    ConflictingL1Validator,

    /// `errUnfinalizedHeight` — a validator-set query targeted a height that is
    /// not yet finalized (`current < target`). Returned, never panicked.
    #[error("requested height is not yet finalized")]
    UnfinalizedHeight,

    /// `ErrInvalidProofOfPossession` — a BLS proof-of-possession failed to
    /// verify against its public key.
    #[error("invalid BLS proof of possession")]
    InvalidProofOfPossession,

    /// A wrapped codec (de)serialization failure.
    #[error("codec: {0}")]
    Codec(#[from] ava_codec::error::CodecError),
}
