// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-crypto` error enum (`thiserror`).
//!
//! Mirrors the Go sentinels across `utils/formatting`, `utils/crypto/secp256k1`,
//! `utils/crypto/bls`, and `staking/`. Owning specs:
//! `specs/03-core-primitives.md` §7 and `specs/25-key-management-and-signing.md`
//! §7.1.

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by `ava-crypto`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    // --- formatting / encodings (M0.17) ---
    /// A hex string was missing the required `0x` prefix
    /// (Go `formatting.ErrMissingHexPrefix`).
    #[error("missing 0x prefix")]
    MissingHexPrefix,

    /// The trailing checksum did not verify (Go `formatting.ErrBadChecksum`).
    #[error("invalid input checksum")]
    BadChecksum,

    /// The decoded payload is shorter than the checksum (Go
    /// `formatting.ErrMissingChecksum`).
    #[error("input string is smaller than the checksum size")]
    MissingChecksum,

    /// A hex string failed to decode (Go hex decode error).
    #[error("hex decoding error: {0}")]
    HexDecoding(String),

    /// The base58 layer failed to decode the input (Go `errBase58Decoding`).
    #[error("base58 decoding error: {0}")]
    Base58Decoding(String),

    /// The requested encoding is not supported on this call path (Go
    /// `formatting` returns an error for `JSON`).
    #[error("unsupported encoding")]
    UnsupportedEncoding,

    /// A chain-prefixed address had no `-` separator
    /// (Go `address.ErrNoSeparator`).
    #[error("no separator found in address")]
    NoSeparator,

    /// bech32 encode/decode failed (Go bech32 error).
    #[error("bech32 error: {0}")]
    Bech32(String),

    // --- secp256k1 (M0.18) ---
    /// A signature has a high-S component (malleable); rejected before recovery
    /// (Go `secp256k1.errMutatedSig`). Consensus-critical.
    #[error("signature was mutated from its original format")]
    MutatedSig,

    /// A recoverable signature carried the compressed recovery flag
    /// (Go `secp256k1.errCompressed`).
    #[error("wasn't expecting a compressed key")]
    Compressed,

    /// A secp256k1 signature, public key, or private key was malformed.
    #[error("invalid secp256k1 input: {0}")]
    InvalidSecp256k1(String),

    /// A private-key string was missing the `PrivateKey-` prefix
    /// (Go `secp256k1.errMissingKeyPrefix`).
    #[error("missing PrivateKey- prefix")]
    MissingPrivateKeyPrefix,

    // --- BLS (M0.19 / M0.21) ---
    /// A BLS secret key failed to deserialize
    /// (Go `localsigner.ErrFailedSecretKeyDeserialize`).
    #[error("couldn't deserialize secret key")]
    FailedSecretKeyDeserialize,

    /// A BLS public key, signature, or aggregate input was malformed.
    #[error("invalid BLS input: {0}")]
    InvalidBls(String),

    /// An aggregate operation was given no inputs (Go `bls` aggregate errors).
    #[error("no signatures or public keys to aggregate")]
    NoAggregateInputs,

    /// BLS verification failed.
    #[error("BLS verification failed")]
    BlsVerifyFailed,

    /// A proof-of-possession failed to verify
    /// (Go `signer.errInvalidProofOfPossession`).
    #[error("invalid proof of possession")]
    InvalidProofOfPossession,

    // --- staking certs (M0.20) ---
    /// The DER cert exceeded `MAX_CERTIFICATE_LEN` (Go
    /// `staking.ErrCertificateTooLarge`).
    #[error("certificate length is greater than the maximum")]
    CertificateTooLarge,

    /// The RSA modulus bit-length was not exactly 2048 or 4096 (Go
    /// `staking.ErrUnsupportedRSAModulusBitLen`).
    #[error("unsupported rsa modulus bitlen")]
    UnsupportedRsaModulusBitLen,

    /// The RSA public exponent was not 65537 (Go
    /// `staking.ErrUnsupportedRSAPublicExponent`).
    #[error("unsupported rsa public exponent")]
    UnsupportedRsaPublicExponent,

    /// The RSA modulus was not positive (Go
    /// `staking.ErrRSAModulusNotPositive`).
    #[error("rsa modulus is not positive")]
    RsaModulusNotPositive,

    /// The RSA modulus was even (Go `staking.ErrRSAModulusIsEven`).
    #[error("rsa modulus is even")]
    RsaModulusIsEven,

    /// Failed to unmarshal the elliptic-curve point (Go
    /// `staking.ErrFailedUnmarshallingEllipticCurvePoint`).
    #[error("failed to unmarshal elliptic curve point")]
    FailedUnmarshallingEllipticCurvePoint,

    /// The certificate used an unknown public-key algorithm (Go
    /// `staking.ErrUnknownPublicKeyAlgorithm`).
    #[error("unknown public key algorithm")]
    UnknownPublicKeyAlgorithm,

    /// The certificate DER failed to parse.
    #[error("failed to parse certificate: {0}")]
    CertificateParse(String),

    /// Certificate generation failed.
    #[error("certificate generation failed: {0}")]
    CertificateGenerate(String),

    /// Certificate signature verification failed.
    #[error("certificate signature verification failed")]
    CertificateVerifyFailed,

    // --- I/O (key files, M0.20 / M0.21) ---
    /// An I/O error while reading or writing key material.
    #[error("io error: {0}")]
    Io(String),
}
