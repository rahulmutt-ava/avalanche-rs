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

    /// `ErrInsufficientCapacity` — `gas.State.ConsumeGas` was asked to consume
    /// more gas than the remaining block capacity (specs 21 §1).
    #[error("insufficient capacity")]
    InsufficientCapacity,

    /// A fee/gas computation overflowed `u64` (`complexity.ToGas` /
    /// `gas.Cost`; specs 21 §0–§1).
    #[error("fee computation overflow")]
    FeeOverflow,

    /// A wrapped codec (de)serialization failure.
    #[error("codec: {0}")]
    Codec(#[from] ava_codec::error::CodecError),

    /// A wrapped base-database failure. A `database.ErrNotFound` surfaces here
    /// for absent state keys (e.g. `get_utxo` / `get_current_validator` on a
    /// missing entry), matching Go's `errors.Is(err, database.ErrNotFound)`.
    #[error("database: {0}")]
    Database(#[from] ava_database::error::Error),

    /// A tx/UTXO component (`avax`/`secp256k1fx`) failed verification.
    #[error("invalid component")]
    InvalidComponent,

    /// `errOutputsNotSorted` — a tx's outputs are not in canonical order.
    #[error("outputs not sorted")]
    OutputsNotSorted,

    /// `errInputsNotSortedUnique` — a tx's inputs are not sorted and unique.
    #[error("inputs not sorted and unique")]
    InputsNotSortedUnique,

    /// `errInvalidLocktime` — a `stakeable` lock has a zero locktime.
    #[error("invalid locktime")]
    InvalidLocktime,

    /// `errNestedStakeableLocks` — a `stakeable` lock wraps another.
    #[error("shouldn't nest stakeable locks")]
    NestedStakeableLock,

    /// `errEmptyNodeID` — a validator's node id is empty.
    #[error("validator nodeID cannot be empty")]
    EmptyNodeId,

    /// `errNoStake` — a staking tx provided no stake outputs.
    #[error("no stake")]
    NoStake,

    /// `errTooManyShares` — `DelegationShares > reward::PERCENT_DENOMINATOR`.
    #[error("too many shares")]
    TooManyShares,

    /// `errInvalidSigner` — BLS key presence does not match the Primary Network.
    #[error("invalid signer")]
    InvalidSigner,

    /// `errMultipleStakedAssets` — stake outputs span more than one asset.
    #[error("multiple staked assets")]
    MultipleStakedAssets,

    /// `errValidatorWeightMismatch` — stake total != `Validator.Wght`.
    #[error("validator weight mismatch")]
    ValidatorWeightMismatch,

    /// `ErrWeightTooSmall` — a validator's weight is zero.
    #[error("weight of this validator is too low")]
    WeightTooSmall,

    /// `errBadSubnetID` — a subnet validator's subnet is the Primary Network.
    #[error("subnet ID can't be primary network ID")]
    BadSubnetId,

    /// An arithmetic operation overflowed.
    #[error("overflow")]
    Overflow,

    /// `errUnexpectedDiffKeyLength` — a staker weight/pk-diff key was not the
    /// expected fixed length (`state/disk_staker_diff_iterator.go`, 08 §7.1).
    #[error("unexpected diff key length")]
    UnexpectedDiffKeyLength,

    /// `errUnexpectedWeightValueLength` — a staker weight-diff value was not the
    /// expected fixed length (`state/disk_staker_diff_iterator.go`, 08 §7.1).
    #[error("unexpected weight value length")]
    UnexpectedWeightValueLength,

    // ----- executor sentinels (M4.16, `txs/executor`) -----
    /// `ErrWeightTooLarge` — a validator's weight exceeds the configured maximum.
    #[error("weight of this validator is too large")]
    WeightTooLarge,

    /// `ErrInsufficientDelegationFee` — the declared delegation fee is below the
    /// configured minimum.
    #[error("staker charges an insufficient delegation fee")]
    InsufficientDelegationFee,

    /// `ErrStakeTooShort` — the staking period is shorter than the minimum.
    #[error("staking period is too short")]
    StakeTooShort,

    /// `ErrStakeTooLong` — the staking period is longer than the maximum.
    #[error("staking period is too long")]
    StakeTooLong,

    /// `ErrFlowCheckFailed` — the value-conservation flow check failed.
    #[error("flow check failed")]
    FlowCheckFailed,

    /// `ErrNotValidator` — the referenced node is not a current/pending
    /// validator (of the primary network or the named subnet).
    #[error("isn't a current or pending validator")]
    NotValidator,

    /// `ErrRemovePermissionlessValidator` — attempted to remove a permissionless
    /// validator via `RemoveSubnetValidatorTx`.
    #[error("attempting to remove permissionless validator")]
    RemovePermissionlessValidator,

    /// `ErrWrongStakedAssetID` — the stake output asset is not the subnet's
    /// configured staking asset.
    #[error("incorrect staked assetID")]
    WrongStakedAssetId,

    /// `ErrDuplicateValidator` — the node is already a validator of the subnet.
    #[error("duplicate validator")]
    DuplicateValidator,

    /// `ErrAlreadyValidator` — the node is already a primary-network validator.
    #[error("already a validator")]
    AlreadyValidator,

    /// `ErrTimestampNotBeforeStartTime` — pre-Durango, the staker start time is
    /// not strictly after the chain time.
    #[error("chain timestamp not before start time")]
    TimestampNotBeforeStartTime,

    /// `errTimeTooAdvanced` — pre-Durango, the staker start time is too far in
    /// the future (beyond `MaxFutureStartTime`).
    #[error("staker start time too far in the future")]
    TimeTooAdvanced,

    /// `errPeriodMismatch` — the proposed staking period is not inside the
    /// dependent (primary-network) staking period.
    #[error("proposed staking period is not inside dependent staking period")]
    PeriodMismatch,

    /// `errWrongNumberOfCredentials` — the tx has no credential available for the
    /// subnet/owner authorization.
    #[error("should have the same number of credentials as inputs")]
    WrongNumberOfCredentials,

    /// `errUnauthorizedModification` — the subnet/owner authorization credential
    /// failed to prove control of the owner.
    #[error("unauthorized modification")]
    UnauthorizedModification,

    /// `ErrDurangoUpgradeNotActive` — a Durango-gated tx was issued before the
    /// Durango upgrade activated.
    #[error("attempting to use a Durango-upgrade feature prior to activation")]
    DurangoUpgradeNotActive,

    /// `errEtnaUpgradeNotActive` — an Etna-gated tx was issued before the Etna
    /// upgrade activated.
    #[error("attempting to use an Etna-upgrade feature prior to activation")]
    EtnaUpgradeNotActive,

    /// `ErrAddValidatorTxPostDurango` — `AddValidatorTx` is not permitted post
    /// Durango.
    #[error("AddValidatorTx is not permitted post-Durango")]
    AddValidatorTxPostDurango,

    /// `ErrAddDelegatorTxPostDurango` — `AddDelegatorTx` is not permitted post
    /// Durango.
    #[error("AddDelegatorTx is not permitted post-Durango")]
    AddDelegatorTxPostDurango,
}
