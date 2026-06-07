// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-simplex` error model.

use ava_types::node_id::NodeId;

use crate::canoto::DecodeError;

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors raised by the Simplex parameters, messages, and QC paths.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `ErrInvalidParameters` — a parameter failed [`crate::Parameters::verify`].
    #[error("simplex parameters must be valid: {0}")]
    InvalidParameters(&'static str),

    /// A validator's compressed BLS public key failed to parse.
    #[error("failed to parse public key for node {node_id}")]
    InvalidPublicKey {
        /// The offending validator.
        node_id: NodeId,
        /// The underlying crypto error.
        #[source]
        source: ava_crypto::Error,
    },

    /// `errFailedToParseQC` / `errFailedToParseBlacklist` — a canoto message
    /// could not be decoded.
    #[error("failed to parse canoto message: {0}")]
    Decode(#[from] DecodeError),

    /// `errFailedToParseSignature` — the QC signature bytes were not a valid
    /// BLS signature.
    #[error("failed to parse signature")]
    InvalidSignature(#[source] ava_crypto::Error),

    /// `errUnexpectedSigners` — the quorum certificate had the wrong number of
    /// signers.
    #[error("unexpected number of signers: expected {expected}, got {got}")]
    UnexpectedSigners {
        /// The required quorum size.
        expected: usize,
        /// The number of signers present.
        got: usize,
    },

    /// `errDuplicateSigner` — a signer appeared more than once in the QC.
    #[error("duplicate signer in quorum certificate")]
    DuplicateSigner,

    /// `errSignerNotFound` / `errNodeNotFound` — a signer/index was not in the
    /// membership set.
    #[error("signer not found in the membership set")]
    SignerNotFound,

    /// `errInvalidBitSet` — the signers bitset did not round-trip through its
    /// canonical (minimal big-endian) encoding.
    #[error("bitset is invalid")]
    InvalidBitSet,

    /// `errSignatureAggregation` — BLS public-key/signature aggregation failed.
    #[error("signature aggregation failed")]
    SignatureAggregation(#[source] ava_crypto::Error),

    /// `errSignatureVerificationFailed` — the aggregated signature did not
    /// verify against the aggregated public key.
    #[error("signature verification failed")]
    SignatureVerificationFailed,
}
