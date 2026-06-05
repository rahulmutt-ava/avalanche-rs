// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! BLS `Signature` + aggregation + verification.
//!
//! Byte-exact port of avalanchego `utils/crypto/bls/signature.go`. Signatures
//! live in G2 (`min_pk`). Group elements are validated on parse, so the verify
//! path passes `false` validation flags to `blst` (mirrors Go). Owning spec:
//! `specs/03-core-primitives.md` §3.5.

use blst::min_pk::{AggregateSignature, Signature as BlstSignature};
use blst::BLST_ERROR;

use super::ciphersuite::{CIPHERSUITE_POP, CIPHERSUITE_SIGNATURE};
use super::keys::PublicKey;
use crate::error::{Error, Result};

/// Length of a compressed BLS signature (G2). Go `bls.SignatureLen`.
pub const SIGNATURE_LEN: usize = 96;

/// A BLS12-381 signature (G2, `min_pk`).
#[derive(Clone)]
pub struct Signature {
    inner: BlstSignature,
}

impl Signature {
    /// `SignatureToBytes` — 96-byte compressed serialization.
    #[must_use]
    pub fn compress(&self) -> [u8; SIGNATURE_LEN] {
        self.inner.compress()
    }

    /// `SignatureFromBytes` — uncompress + subgroup (`sig_validate`).
    ///
    /// # Errors
    /// [`Error::InvalidBls`] if the bytes are not a valid subgroup point.
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        // sig_validate(false) = uncompress + subgroup check (no infinity check),
        // matching Go's SignatureFromBytes.
        let inner =
            BlstSignature::sig_validate(b, false).map_err(|e| Error::InvalidBls(format!("{e:?}")))?;
        Ok(Self { inner })
    }

    /// Internal: wrap a freshly produced `blst` signature.
    pub(super) fn from_blst(inner: BlstSignature) -> Self {
        Self { inner }
    }
}

/// `Verify(pk, sig, msg)` — verify under the SIGNATURE ciphersuite.
#[must_use]
pub fn verify(pk: &PublicKey, sig: &Signature, msg: &[u8]) -> bool {
    // sig_groupcheck=false, pk_validate=false: both validated on parse (Go).
    sig.inner
        .verify(false, msg, CIPHERSUITE_SIGNATURE, &[], pk.as_blst(), false)
        == BLST_ERROR::BLST_SUCCESS
}

/// `VerifyProofOfPossession(pk, sig, msg)` — verify under the POP ciphersuite.
#[must_use]
pub fn verify_pop(pk: &PublicKey, sig: &Signature, msg: &[u8]) -> bool {
    sig.inner
        .verify(false, msg, CIPHERSUITE_POP, &[], pk.as_blst(), false)
        == BLST_ERROR::BLST_SUCCESS
}

/// `AggregateSignatures` — aggregate G2 signatures.
///
/// # Errors
/// [`Error::NoAggregateInputs`] if `sigs` is empty; [`Error::InvalidBls`] on a
/// `blst` aggregation failure.
pub fn aggregate_signatures(sigs: &[&Signature]) -> Result<Signature> {
    if sigs.is_empty() {
        return Err(Error::NoAggregateInputs);
    }
    let blst_sigs: Vec<&BlstSignature> = sigs.iter().map(|s| &s.inner).collect();
    // Already validated on parse, so pass sigs_groupcheck = false (mirrors Go).
    let agg = AggregateSignature::aggregate(&blst_sigs, false)
        .map_err(|e| Error::InvalidBls(format!("{e:?}")))?;
    Ok(Signature::from_blst(agg.to_signature()))
}
