// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Recoverable secp256k1 with consensus-critical low-S enforcement.
//!
//! Byte-exact port of avalanchego `utils/crypto/secp256k1/secp256k1.go`. The
//! C-FFI `secp256k1` crate (Bitcoin Core libsecp256k1) encapsulates all `unsafe`
//! behind its safe API — this module is `unsafe`-free (`specs/00` §7.6).
//!
//! Wire layout (`specs/03-core-primitives.md` §3.4): Avalanche stores signatures
//! as `[r || s || v]` (64-byte r/s + 1-byte recovery id in `0..=3`); the
//! `secp256k1` crate's recoverable form is `(RecoveryId, [r || s; 64])`. Low-S is
//! enforced on the 32-byte S scalar BEFORE recovery (`errMutatedSig`).

use std::fmt;
use std::str::FromStr;

use ava_types::short_id::ShortId;
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId, Signature as EcdsaSignature};
use secp256k1::{Message, PublicKey as SecpPublicKey, SecretKey, SECP256K1};

use crate::cb58::{cb58_decode, cb58_encode};
use crate::error::{Error, Result};
use crate::hashing::{keccak256, pubkey_bytes_to_address};

/// Length of an Avalanche secp256k1 signature `[r || s || v]` (Go
/// `secp256k1.SignatureLen`).
pub const SIGNATURE_LEN: usize = 65;

/// Length of a secp256k1 private key (Go `secp256k1.PrivateKeyLen`).
pub const PRIVATE_KEY_LEN: usize = 32;

/// Length of a compressed secp256k1 public key (Go `secp256k1.PublicKeyLen`).
pub const PUBLIC_KEY_LEN: usize = 33;

/// The `PrivateKey-` string prefix (Go `secp256k1.PrivateKeyPrefix`).
pub const PRIVATE_KEY_PREFIX: &str = "PrivateKey-";

/// A recoverable secp256k1 signature `[r || s || v]`.
///
/// Stateless helper namespace mirroring the Go free functions.
pub struct Signature;

impl Signature {
    /// `verifySECP256K1RSignatureFormat` — reject high-S signatures (malleable).
    ///
    /// Consensus-critical: a high-S signature must be rejected (`errMutatedSig`)
    /// BEFORE any recovery attempt.
    ///
    /// # Errors
    /// - [`Error::MutatedSig`] if the S scalar is in the upper half of the order.
    /// - [`Error::Compressed`] if the recovery id carries the compressed flag.
    /// - [`Error::InvalidSecp256k1`] if r/s do not parse as a valid signature.
    pub fn verify_format(sig: &[u8; SIGNATURE_LEN]) -> Result<()> {
        // The recovery id is the in-range value 0..=3; Go rejects compressed.
        let v = sig[64];
        if v >= 4 {
            return Err(Error::Compressed);
        }
        // Parse r||s as a non-recoverable signature, then check low-S by
        // normalizing a copy and comparing — if normalization changes it, S was
        // high.
        let rs: [u8; 64] = sig[0..64]
            .try_into()
            .map_err(|_| Error::InvalidSecp256k1("bad signature length".into()))?;
        let parsed = EcdsaSignature::from_compact(&rs)
            .map_err(|e| Error::InvalidSecp256k1(e.to_string()))?;
        let mut normalized = parsed;
        normalized.normalize_s();
        if normalized.serialize_compact() != parsed.serialize_compact() {
            return Err(Error::MutatedSig);
        }
        Ok(())
    }
}

/// A secp256k1 public key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PublicKey(SecpPublicKey);

impl PublicKey {
    /// `PublicKey.Bytes` — the 33-byte compressed serialization.
    #[must_use]
    pub fn bytes(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.0.serialize()
    }

    /// `PublicKey.Address` — `ripemd160(sha256(compressed_pubkey))`.
    #[must_use]
    pub fn address(&self) -> ShortId {
        ShortId::from(pubkey_bytes_to_address(&self.bytes()))
    }

    /// `PublicKey.EthAddress` — `keccak256(uncompressed[1..])[12..]` (EVM).
    #[must_use]
    pub fn eth_address(&self) -> [u8; 20] {
        let uncompressed = self.0.serialize_uncompressed();
        // Drop the 0x04 prefix; keccak the 64-byte X||Y; take the last 20 bytes.
        let h = keccak256(&uncompressed[1..]);
        let mut out = [0u8; 20];
        out.copy_from_slice(&h[12..]);
        out
    }

    /// Construct from a compressed (33-byte) or uncompressed serialization.
    ///
    /// # Errors
    /// [`Error::InvalidSecp256k1`] if the bytes are not a valid point.
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        SecpPublicKey::from_slice(b)
            .map(PublicKey)
            .map_err(|e| Error::InvalidSecp256k1(e.to_string()))
    }

    /// `RecoverPublicKeyFromHash` — recover the signer from `[r||s||v]` over a
    /// 32-byte hash. Rejects high-S first (consensus-critical).
    ///
    /// # Errors
    /// - [`Error::MutatedSig`] / [`Error::Compressed`] from format validation.
    /// - [`Error::InvalidSecp256k1`] if recovery fails.
    pub fn recover_from_hash(hash: &[u8; 32], sig: &[u8; SIGNATURE_LEN]) -> Result<Self> {
        Signature::verify_format(sig)?;
        let recid = RecoveryId::try_from(i32::from(sig[64]))
            .map_err(|e| Error::InvalidSecp256k1(e.to_string()))?;
        let rs: [u8; 64] = sig[0..64]
            .try_into()
            .map_err(|_| Error::InvalidSecp256k1("bad signature length".into()))?;
        let recoverable = RecoverableSignature::from_compact(&rs, recid)
            .map_err(|e| Error::InvalidSecp256k1(e.to_string()))?;
        let msg = Message::from_digest(*hash);
        SECP256K1
            .recover_ecdsa(msg, &recoverable)
            .map(PublicKey)
            .map_err(|e| Error::InvalidSecp256k1(e.to_string()))
    }

    /// `PublicKey.VerifyHash` — recover from `sig` and compare addresses.
    #[must_use]
    pub fn verify_hash(&self, hash: &[u8; 32], sig: &[u8; SIGNATURE_LEN]) -> bool {
        match Self::recover_from_hash(hash, sig) {
            Ok(recovered) => recovered.address() == self.address(),
            Err(_) => false,
        }
    }
}

/// A secp256k1 private key.
pub struct PrivateKey(SecretKey);

impl PrivateKey {
    /// `secp256k1.ToPrivateKey` — construct from 32 raw bytes.
    ///
    /// # Errors
    /// [`Error::InvalidSecp256k1`] if the scalar is invalid (zero / overflow).
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        let arr: [u8; PRIVATE_KEY_LEN] = b
            .try_into()
            .map_err(|_| Error::InvalidSecp256k1("private key must be 32 bytes".into()))?;
        SecretKey::from_byte_array(arr)
            .map(PrivateKey)
            .map_err(|e| Error::InvalidSecp256k1(e.to_string()))
    }

    /// The raw 32-byte scalar.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; PRIVATE_KEY_LEN] {
        self.0.secret_bytes()
    }

    /// The corresponding [`PublicKey`].
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.public_key(SECP256K1))
    }

    /// `PrivateKey.SignHash` — RFC6979 deterministic, low-S–normalized,
    /// recoverable signature over a 32-byte hash, serialized as `[r||s||v]`.
    ///
    /// # Errors
    /// Never fails in practice; returns [`Error`] for API symmetry.
    pub fn sign_hash(&self, hash: &[u8; 32]) -> Result<[u8; SIGNATURE_LEN]> {
        let msg = Message::from_digest(*hash);
        let recoverable = SECP256K1.sign_ecdsa_recoverable(msg, &self.0);
        let (recid, rs) = recoverable.serialize_compact();
        let mut out = [0u8; SIGNATURE_LEN];
        out[0..64].copy_from_slice(&rs);
        // RecoveryId -> i32 is in 0..=3 (uncompressed); fits in a u8.
        let v = i32::from(recid);
        out[64] = u8::try_from(v).map_err(|_| Error::InvalidSecp256k1("bad recid".into()))?;
        Ok(out)
    }
}

impl fmt::Display for PrivateKey {
    /// `"PrivateKey-" + cb58(32-byte sk)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // cb58_encode of a 32-byte payload never overflows.
        let encoded = cb58_encode(&self.to_bytes()).map_err(|_| fmt::Error)?;
        write!(f, "{PRIVATE_KEY_PREFIX}{encoded}")
    }
}

impl FromStr for PrivateKey {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let body = s
            .strip_prefix(PRIVATE_KEY_PREFIX)
            .ok_or(Error::MissingPrivateKeyPrefix)?;
        let bytes = cb58_decode(body).map_err(|e| Error::Base58Decoding(e.to_string()))?;
        Self::from_bytes(&bytes)
    }
}
