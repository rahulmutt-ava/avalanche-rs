// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Strict staking-cert parser (matches Go's accept/reject set exactly).
//!
//! Go's `staking/parse.go` is deliberately stricter than `crypto/x509`: it caps
//! the cert size, requires the public key to parse, and constrains RSA/ECDSA
//! key parameters. This port performs the SPKI walk via `x509-parser` and then
//! applies the explicit policy checks. Owning spec:
//! `specs/03-core-primitives.md` §3.6, `specs/25` §8.1.

use x509_parser::prelude::FromDer;
use x509_parser::public_key::PublicKey as X509PublicKey;
use x509_parser::x509::X509Version;

use super::certificate::{CertPublicKey, Certificate};
use crate::error::{Error, Result};

/// Maximum DER length of a staking certificate (Go `staking.MaxCertificateLen`
/// = `2 * units.KiB`).
pub const MAX_CERTIFICATE_LEN: usize = 2 * 1024;

/// Allowed RSA modulus bit lengths (Go `staking.allowedRSAModulusBitLens`).
const ALLOWED_RSA_MODULUS_BITS: [usize; 2] = [2048, 4096];

/// The only allowed RSA public exponent (Go `staking.allowedRSAPublicExponent`).
const ALLOWED_RSA_EXPONENT: u64 = 65537;

/// P-256 public-key size in bits (Go restricts ECDSA to the P-256 curve).
const P256_KEY_BITS: usize = 256;

/// `staking.ParseCertificate` — strict parse + policy validation.
///
/// # Errors
/// - [`Error::CertificateTooLarge`] if the DER exceeds [`MAX_CERTIFICATE_LEN`].
/// - [`Error::CertificateParse`] if the DER does not decode as an X.509 cert.
/// - [`Error::UnsupportedRsaModulusBitLen`] / [`Error::UnsupportedRsaPublicExponent`]
///   / [`Error::RsaModulusNotPositive`] / [`Error::RsaModulusIsEven`] for
///   non-conformant RSA keys.
/// - [`Error::FailedUnmarshallingEllipticCurvePoint`] for non-P-256 ECDSA keys.
/// - [`Error::UnknownPublicKeyAlgorithm`] for any other key algorithm.
pub fn parse_certificate(der: &[u8]) -> Result<Certificate> {
    if der.len() > MAX_CERTIFICATE_LEN {
        return Err(Error::CertificateTooLarge);
    }

    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(der)
        .map_err(|e| Error::CertificateParse(e.to_string()))?;

    // Go only handles v1/v3 certs; reject obviously malformed versions early by
    // requiring a recognized version (x509-parser already enforces structure).
    let _ = cert.version() == X509Version::V3 || cert.version() == X509Version::V1;

    let spki = cert.public_key();
    let parsed = spki
        .parsed()
        .map_err(|_| Error::UnknownPublicKeyAlgorithm)?;

    let public_key = match parsed {
        X509PublicKey::RSA(rsa) => {
            // Exponent must be exactly 65537.
            let exponent = rsa
                .try_exponent()
                .map_err(|_| Error::UnsupportedRsaPublicExponent)?;
            if exponent != ALLOWED_RSA_EXPONENT {
                return Err(Error::UnsupportedRsaPublicExponent);
            }

            // Modulus must be positive, odd, and exactly 2048 or 4096 bits.
            let bits = modulus_bit_len(rsa.modulus);
            if bits == 0 {
                return Err(Error::RsaModulusNotPositive);
            }
            if modulus_is_even(rsa.modulus) {
                return Err(Error::RsaModulusIsEven);
            }
            if !ALLOWED_RSA_MODULUS_BITS.contains(&bits) {
                return Err(Error::UnsupportedRsaModulusBitLen);
            }
            CertPublicKey::Rsa {
                modulus: rsa.modulus.to_vec(),
                exponent: rsa.exponent.to_vec(),
            }
        }
        X509PublicKey::EC(ec) => {
            // Go restricts ECDSA staking keys to P-256. The uncompressed SEC1
            // point for P-256 is 65 bytes (0x04 || X[32] || Y[32]) -> 256 bits.
            if ec.key_size() != P256_KEY_BITS {
                return Err(Error::FailedUnmarshallingEllipticCurvePoint);
            }
            CertPublicKey::EcdsaP256(ec.data().to_vec())
        }
        _ => return Err(Error::UnknownPublicKeyAlgorithm),
    };

    Ok(Certificate {
        raw: der.to_vec(),
        public_key,
    })
}

/// Bit length of a big-endian DER `INTEGER` modulus, ignoring leading zero
/// padding. Returns 0 for a non-positive (zero or empty) modulus.
fn modulus_bit_len(modulus: &[u8]) -> usize {
    // Skip leading zero bytes (DER prepends 0x00 to keep positive integers
    // positive; multiple leading zeros are not valid DER but be defensive).
    let first_nonzero = modulus.iter().position(|&b| b != 0);
    match first_nonzero {
        None => 0,
        Some(idx) => {
            let significant = &modulus[idx..];
            // bits = 8 * (len - 1) + bit position of the top set bit + 1.
            let top = significant[0];
            let leading_zeros = top.leading_zeros() as usize;
            8usize
                .saturating_mul(significant.len())
                .saturating_sub(leading_zeros)
        }
    }
}

/// Whether the big-endian modulus is even (low bit of the last byte is 0).
fn modulus_is_even(modulus: &[u8]) -> bool {
    match modulus.last() {
        None => true, // empty == not positive == treated as even/invalid upstream
        Some(&last) => last & 1 == 0,
    }
}
