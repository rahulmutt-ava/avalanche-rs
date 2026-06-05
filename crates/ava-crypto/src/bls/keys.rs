// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! BLS `SecretKey` / `PublicKey` (`blst::min_pk`).
//!
//! Byte-exact port of avalanchego `utils/crypto/bls/{secretkey,publickey}.go`.
//! The `blst` C-FFI is wrapped behind a safe API; all `unsafe` lives inside the
//! `blst` crate. `blst::min_pk::SecretKey` derives `Zeroize`-on-drop, so the
//! in-memory scalar is wiped automatically (`specs/03` §3.5, `specs/25` §6).
//! Owning spec: `specs/03-core-primitives.md` §3.5.

use blst::min_pk::{AggregatePublicKey, PublicKey as BlstPublicKey, SecretKey as BlstSecretKey};

use super::ciphersuite::{CIPHERSUITE_POP, CIPHERSUITE_SIGNATURE};
use super::sign::Signature;
use crate::error::{Error, Result};

/// Length of a compressed BLS public key (G1). Go `bls.PublicKeyLen`.
pub const PUBLIC_KEY_LEN: usize = 48;

/// Length of a BLS secret key. Go `bls.SecretKeyLen`.
pub const SECRET_KEY_LEN: usize = 32;

/// Length of an uncompressed BLS public key (G1). Go `bls.UncompressedPublicKeyLen`.
pub const UNCOMPRESSED_PUBLIC_KEY_LEN: usize = 96;

/// A BLS12-381 secret key (`min_pk`). Zeroized on drop (via `blst`).
pub struct SecretKey {
    inner: BlstSecretKey,
}

impl SecretKey {
    /// `localsigner.New` — generate from 32 bytes of CSPRNG IKM.
    ///
    /// # Errors
    /// [`Error::FailedSecretKeyDeserialize`] if key generation rejects the IKM.
    pub fn new(ikm: &[u8; SECRET_KEY_LEN]) -> Result<Self> {
        // key_gen requires ikm.len() >= 32. The caller owns IKM zeroization.
        let inner =
            BlstSecretKey::key_gen(ikm, &[]).map_err(|_| Error::FailedSecretKeyDeserialize)?;
        Ok(Self { inner })
    }

    /// `localsigner.FromBytes` — big-endian deserialize of the 32-byte scalar.
    ///
    /// # Errors
    /// [`Error::FailedSecretKeyDeserialize`] if the bytes are not a valid scalar.
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        let inner = BlstSecretKey::from_bytes(b).map_err(|_| Error::FailedSecretKeyDeserialize)?;
        Ok(Self { inner })
    }

    /// The big-endian 32-byte serialization (Go `SecretKeyToBytes`).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; SECRET_KEY_LEN] {
        self.inner.to_bytes()
    }

    /// The corresponding [`PublicKey`].
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            inner: self.inner.sk_to_pk(),
        }
    }

    /// `Signer.Sign` — sign `msg` with the SIGNATURE ciphersuite (hash-to-G2).
    #[must_use]
    pub fn sign(&self, msg: &[u8]) -> Signature {
        Signature::from_blst(self.inner.sign(msg, CIPHERSUITE_SIGNATURE, &[]))
    }

    /// `Signer.SignProofOfPossession` — sign with the POP ciphersuite.
    #[must_use]
    pub fn sign_pop(&self, msg: &[u8]) -> Signature {
        Signature::from_blst(self.inner.sign(msg, CIPHERSUITE_POP, &[]))
    }
}

/// A BLS12-381 public key (G1, `min_pk`).
#[derive(Clone)]
pub struct PublicKey {
    inner: BlstPublicKey,
}

impl PublicKey {
    /// `PublicKeyToCompressedBytes` — 48-byte compressed serialization.
    #[must_use]
    pub fn compress(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.inner.compress()
    }

    /// `PublicKeyToUncompressedBytes` — 96-byte uncompressed serialization.
    #[must_use]
    pub fn serialize(&self) -> [u8; UNCOMPRESSED_PUBLIC_KEY_LEN] {
        self.inner.serialize()
    }

    /// `PublicKeyFromCompressedBytes` — uncompress + subgroup validate.
    ///
    /// # Errors
    /// [`Error::InvalidBls`] if the bytes are not a valid subgroup point.
    pub fn from_compressed(b: &[u8]) -> Result<Self> {
        // key_validate = uncompress + subgroup check + non-infinity check.
        let inner =
            BlstPublicKey::key_validate(b).map_err(|e| Error::InvalidBls(format!("{e:?}")))?;
        Ok(Self { inner })
    }

    /// Internal: wrap a validated `blst` public key.
    pub(super) fn from_blst(inner: BlstPublicKey) -> Self {
        Self { inner }
    }

    /// Internal: borrow the underlying `blst` public key for verify.
    pub(super) fn as_blst(&self) -> &BlstPublicKey {
        &self.inner
    }
}

/// `AggregatePublicKeys` — aggregate G1 public keys.
///
/// # Errors
/// [`Error::NoAggregateInputs`] if `pks` is empty; [`Error::InvalidBls`] on a
/// `blst` aggregation failure.
pub fn aggregate_public_keys(pks: &[&PublicKey]) -> Result<PublicKey> {
    if pks.is_empty() {
        return Err(Error::NoAggregateInputs);
    }
    let blst_pks: Vec<&BlstPublicKey> = pks.iter().map(|p| p.as_blst()).collect();
    // Already validated on parse, so pass pks_validate = false (mirrors Go).
    let agg = AggregatePublicKey::aggregate(&blst_pks, false)
        .map_err(|e| Error::InvalidBls(format!("{e:?}")))?;
    Ok(PublicKey::from_blst(agg.to_public_key()))
}
