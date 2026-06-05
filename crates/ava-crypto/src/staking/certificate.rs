// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The parsed staking `Certificate` type.
//!
//! Mirrors Go `staking.Certificate { Raw, PublicKey }`. Produced by
//! [`super::parse`] and consumed by NodeID derivation + [`super::verify`].
//! Owning spec: `specs/03-core-primitives.md` §3.6.

/// The public-key family carried by a staking certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertPublicKey {
    /// ECDSA on the NIST P-256 curve. Holds the SEC1 point bytes.
    EcdsaP256(Vec<u8>),
    /// RSA with a 2048- or 4096-bit modulus and exponent 65537. Holds the
    /// modulus and exponent bytes (DER `INTEGER` contents).
    Rsa {
        /// Big-endian modulus bytes (may include a leading `0x00`).
        modulus: Vec<u8>,
        /// Big-endian exponent bytes.
        exponent: Vec<u8>,
    },
}

/// A parsed staking certificate.
///
/// `raw` is the full DER encoding (`cert.Raw` in Go) — the input to NodeID
/// derivation. `public_key` is the validated, policy-conformant key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certificate {
    /// The entire DER-encoded certificate.
    pub raw: Vec<u8>,
    /// The validated public key.
    pub public_key: CertPublicKey,
}
