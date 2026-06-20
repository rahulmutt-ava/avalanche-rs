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

    /// A persisted singleton or block-index entry had an unexpected byte length
    /// when resuming state from disk (`State::load`) — a corrupt-DB signal,
    /// since the base DB is the truth on recovery (specs 27 §5.1).
    #[error("corrupt persisted state: {0}")]
    CorruptState(&'static str),

    /// `errWrongTxType` — a [`crate`] visitor was invoked for a tx type it does
    /// not handle (the default `Visitor` method).
    #[error("wrong tx type")]
    WrongTxType,

    /// `ErrRemoveStakerTooEarly` — attempted to remove a staker before its end
    /// time / the chain's current time.
    #[error("attempted to remove staker before its end time")]
    RemoveStakerTooEarly,

    /// `ErrRemoveWrongStaker` — a `RewardValidatorTx` named a staker that is not
    /// the next one due to leave the current set (M4.17, `txs/executor`).
    #[error("attempting to remove wrong staker")]
    RemoveWrongStaker,

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

    /// `errHeliconUpgradeNotActive` — a Helicon-gated (ACP-236 auto-renew) tx was
    /// issued before the Helicon upgrade activated.
    #[error("attempting to use a Helicon-upgrade feature prior to activation")]
    HeliconUpgradeNotActive,

    /// `ErrAddValidatorTxPostDurango` — `AddValidatorTx` is not permitted post
    /// Durango.
    #[error("AddValidatorTx is not permitted post-Durango")]
    AddValidatorTxPostDurango,

    /// `ErrAddDelegatorTxPostDurango` — `AddDelegatorTx` is not permitted post
    /// Durango.
    #[error("AddDelegatorTx is not permitted post-Durango")]
    AddDelegatorTxPostDurango,

    // ----- ACP-77 L1 lifecycle sentinels (M4.19, `txs/executor`) -----
    /// `errMaxNumActiveValidators` — the converted subnet is already at the
    /// configured maximum number of active L1 validators.
    #[error("already at the max number of active validators")]
    MaxNumActiveValidators,

    /// `errCouldNotLoadL1Validator` — the referenced L1 validator could not be
    /// loaded from state.
    #[error("could not load L1 validator")]
    CouldNotLoadL1Validator,

    /// `errWarpMessageContainsStaleNonce` — a `SetL1ValidatorWeight` message's
    /// nonce is below the validator's `MinNonce`.
    #[error("warp message contains stale nonce")]
    WarpMessageContainsStaleNonce,

    /// `errRemovingLastValidator` — attempted to remove the last L1 validator
    /// from a converted subnet (weight would drop to zero).
    #[error("attempting to remove the last L1 validator from a converted subnet")]
    RemovingLastValidator,

    /// `errStateCorruption` — an invariant that should be unreachable was
    /// violated (e.g. an active validator's `EndAccumulatedFee <= accruedFees`).
    #[error("state corruption")]
    StateCorruption,

    /// `errWarpMessageExpired` — a `RegisterL1Validator` message's expiry is at or
    /// before the current chain time.
    #[error("warp message expired")]
    WarpMessageExpired,

    /// `errWarpMessageNotYetAllowed` — a `RegisterL1Validator` message's expiry is
    /// further in the future than the allowed registration window.
    #[error("warp message not yet allowed")]
    WarpMessageNotYetAllowed,

    /// `errWarpMessageAlreadyIssued` — a `RegisterL1Validator` message replays a
    /// validation id that has already been issued (expiry-set replay guard).
    #[error("warp message already issued")]
    WarpMessageAlreadyIssued,

    /// `errCouldNotLoadSubnetToL1Conversion` / `errWrongWarpMessageSourceChainID`
    /// / `errWrongWarpMessageSourceAddress` — the embedded Warp message did not
    /// originate from the subnet's recorded L1-conversion manager chain/address.
    #[error("warp message source does not match the subnet's L1 conversion")]
    WrongWarpMessageSource,

    // ----- warp signing / verification sentinels (M4.22, `warp`) -----
    /// `ErrWrongSourceChainID` — a [`LocalSigner`](crate::warp::signer::LocalSigner)
    /// was asked to sign an `UnsignedMessage` for a chain other than its own.
    #[error("wrong SourceChainID")]
    WrongSourceChainId,

    /// `ErrWrongNetworkID` — an `UnsignedMessage`'s network id does not match the
    /// signer's / verifier's network id.
    #[error("wrong networkID")]
    WrongNetworkId,

    /// `ErrInvalidBitSet` — a `BitSetSignature`'s signer bit-set has unnecessary
    /// zero-padding (`set.BitsFromBytes(b).Bytes() != b`).
    #[error("bitset is invalid")]
    InvalidBitSet,

    /// `ErrUnknownValidator` — a `BitSetSignature` selects a canonical index past
    /// the end of the validator set.
    #[error("unknown validator")]
    UnknownValidator,

    /// `ErrInsufficientWeight` — the signing validators' weight is below the
    /// required quorum fraction of the total weight.
    #[error("signature weight is insufficient")]
    InsufficientWeight,

    /// `ErrParseSignature` — the aggregate BLS signature bytes failed to parse.
    #[error("failed to parse signature")]
    ParseSignature,

    /// `ErrInvalidSignature` — the aggregate BLS signature did not verify against
    /// the aggregated public key over the message bytes.
    #[error("signature is invalid")]
    InvalidSignature,

    /// The source chain's subnet has no validator set at the pinned P-Chain
    /// height (no entry in
    /// [`get_warp_validator_sets`](ava_validators::state::ValidatorState::get_warp_validator_sets)).
    #[error("no validator set for source subnet")]
    NoValidatorSet,

    /// A wrapped validator-state failure surfaced while obtaining the warp set.
    #[error("validators: {0}")]
    Validators(#[from] ava_validators::error::Error),

    // ----- VM lifecycle sentinels (M4.25, `vm.rs`/`block/builder`) -----
    /// `ErrNoPendingBlocks` — the block builder was asked to build a block but
    /// the mempool is empty and no time-advance / reward proposal is due
    /// (`block/builder.builder`, 08 §4.3). The engine treats this as "the VM
    /// declined to issue a block", not a hard failure.
    #[error("no pending blocks")]
    NoPendingBlocks,

    /// The VM was driven before [`PlatformVm::initialize`](crate::vm::PlatformVm)
    /// ran (the block manager / validator manager are not yet constructed).
    #[error("VM not initialized")]
    NotInitialized,

    // ----- read-service failures (M4.28, `service.rs`/`client.rs`) -----
    /// A JSON-RPC read-method failure that carries a descriptive message
    /// (a wrapped [`ValidatorState`](ava_validators::state::ValidatorState)
    /// error, a missing block at a height, a malformed address, etc.). These
    /// surface to the API caller, not the consensus engine.
    #[error("service: {0}")]
    Service(String),
}

// Map the generic Warp / ICM error ([`ava_warp::Error`], specs 20 §9) onto the
// P-Chain sentinels. The variant identities are preserved 1:1 (so call sites /
// tests can still `assert_matches!` the exact failure mode); the registry
// payload's structural-`verify()` failure (`ava_warp::Error::InvalidPayload`)
// collapses onto [`Error::InvalidComponent`], matching the pre-extraction
// behaviour where the warp `verify()` returned `Error::InvalidComponent`
// directly. The wrapped `Codec` / `Validators` errors round-trip through the
// existing `#[from]` variants.
impl From<ava_warp::Error> for Error {
    fn from(e: ava_warp::Error) -> Self {
        match e {
            ava_warp::Error::WrongSourceChainId => Error::WrongSourceChainId,
            ava_warp::Error::WrongNetworkId => Error::WrongNetworkId,
            ava_warp::Error::InvalidBitSet => Error::InvalidBitSet,
            ava_warp::Error::UnknownValidator => Error::UnknownValidator,
            ava_warp::Error::InsufficientWeight => Error::InsufficientWeight,
            ava_warp::Error::ParseSignature => Error::ParseSignature,
            ava_warp::Error::InvalidSignature => Error::InvalidSignature,
            ava_warp::Error::Overflow => Error::Overflow,
            ava_warp::Error::NoValidatorSet => Error::NoValidatorSet,
            ava_warp::Error::InvalidPayload => Error::InvalidComponent,
            ava_warp::Error::Codec(c) => Error::Codec(c),
            ava_warp::Error::Validators(v) => Error::Validators(v),
            // `ava_warp::Error` is `#[non_exhaustive]`; any future warp-only
            // sentinel surfaces as a generic component-invalid failure until the
            // P-Chain grows a dedicated mapping.
            _ => Error::InvalidComponent,
        }
    }
}

// The `ChainVm`/`Block` trait surfaces return `ava_vm::Error` / `ava_snow::Error`
// respectively; map the P-Chain error onto those crates' (closed, non-exhaustive)
// enums. The orphan rule permits these `From` impls because the source type is
// local. This mirrors the established `ava-proposervm` precedent (its `error.rs`).
//
// Neither `ava_vm::Error` nor `ava_snow::Error` exposes a free-form `Other`
// variant, so non-`NotFound` P-Chain errors collapse onto the nearest carrying
// variant; `NotFound` round-trips exactly.
impl From<Error> for ava_vm::error::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Database(ava_database::error::Error::NotFound) => ava_vm::error::Error::NotFound,
            // No generic string variant exists on `ava_vm::Error`; surface a
            // stable, descriptive static message (the detailed message stays in
            // the P-Chain log path, not the engine-facing error).
            _ => ava_vm::error::Error::InvalidComponent("platformvm vm/build error"),
        }
    }
}

impl From<Error> for ava_snow::error::Error {
    fn from(e: Error) -> Self {
        // `ava_snow::Error::ParametersInvalid(String)` is the only string-carrying
        // variant; reuse it to preserve the P-Chain error message on the critical
        // verify/accept path (a returned `Err` halts the chain).
        ava_snow::error::Error::ParametersInvalid(format!("platformvm: {e}"))
    }
}
