// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Error model for the Warp / ICM primitives (specs 20 §9).
//!
//! A single `thiserror` [`Error`] enum, preserving the Go sentinel identities
//! (`vms/platformvm/warp/**`, compared via `errors.Is`, specs 00 §7.1) for the
//! signing / bit-set / quorum paths. These variants are re-exported / mapped by
//! consumers (P-Chain, the EVM warp precompile, SAE) onto their own error enums.

/// The Warp result alias.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors produced by the Warp / ICM primitives (`vms/platformvm/warp`).
///
/// Variants preserve the Go sentinel names so call sites can pattern-match the
/// exact failure mode (`assert_matches!(err, Error::WrongNetworkId)`), mirroring
/// `errors.Is(err, errWrongNetworkID)` in Go.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `ErrWrongSourceChainID` — a [`LocalSigner`](crate::signer::LocalSigner)
    /// was asked to sign an [`UnsignedMessage`](crate::UnsignedMessage) for a
    /// chain other than its own.
    #[error("wrong SourceChainID")]
    WrongSourceChainId,

    /// `ErrWrongNetworkID` — an [`UnsignedMessage`](crate::UnsignedMessage)'s
    /// network id does not match the signer's / verifier's network id.
    #[error("wrong networkID")]
    WrongNetworkId,

    /// `ErrInvalidBitSet` — a [`BitSetSignature`](crate::BitSetSignature)'s
    /// signer bit-set has unnecessary zero-padding
    /// (`set.BitsFromBytes(b).Bytes() != b`).
    #[error("bitset is invalid")]
    InvalidBitSet,

    /// `ErrUnknownValidator` — a [`BitSetSignature`](crate::BitSetSignature)
    /// selects a canonical index past the end of the validator set.
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

    /// An arithmetic operation overflowed (Go `safemath.ErrOverflow`).
    #[error("overflow")]
    Overflow,

    /// A registry payload failed its structural `verify()` check
    /// (`ErrInvalidSubnetID` / `ErrInvalidWeight` / `ErrInvalidNodeID` /
    /// `ErrInvalidOwner` / `ErrNonceReservedForRemoval`, all collapsed to one
    /// component-invalid sentinel). Consumers map this onto their own
    /// "invalid component" error.
    #[error("invalid registry payload")]
    InvalidPayload,

    /// The source chain's subnet has no validator set at the pinned P-Chain
    /// height (no entry in
    /// [`get_warp_validator_sets`](ava_validators::state::ValidatorState::get_warp_validator_sets)).
    #[error("no validator set for source subnet")]
    NoValidatorSet,

    /// A wrapped codec (de)serialization failure.
    #[error("codec: {0}")]
    Codec(#[from] ava_codec::error::CodecError),

    /// A wrapped validator-state failure surfaced while obtaining the warp set.
    #[error("validators: {0}")]
    Validators(#[from] ava_validators::error::Error),
}
