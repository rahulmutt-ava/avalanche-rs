// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking-cert signature verification.
//!
//! Port of Go `staking/verify.go::CheckSignature(cert, msg, sig)`: hash `msg`
//! with SHA-256 then verify with RSA `PKCS1v15`/SHA-256 (RSA keys) or ECDSA
//! `VerifyASN1` over P-256 (ECDSA keys). `ring` hashes the message internally
//! with the suite's digest, so we hand it `msg` directly (== verifying the
//! signature over `sha256(msg)`). Owning spec: `specs/03-core-primitives.md`
//! §3.6.

use ring::signature::{ECDSA_P256_SHA256_ASN1, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

use super::certificate::{CertPublicKey, Certificate};
use crate::error::{Error, Result};

/// `staking.CheckSignature` — verify that `sig` is a valid signature of `msg`
/// under the certificate's public key.
///
/// The signature scheme is selected by the certificate's key family:
/// - [`CertPublicKey::EcdsaP256`] → ECDSA P-256 with the ASN.1 (DER) signature
///   encoding, SHA-256 digest.
/// - [`CertPublicKey::Rsa`] → RSA `PKCS#1 v1.5`, SHA-256 digest.
///
/// # Errors
/// [`Error::CertificateVerifyFailed`] if the signature does not verify (or the
/// RSA `SubjectPublicKeyInfo` cannot be reconstructed).
pub fn check_signature(cert: &Certificate, msg: &[u8], sig: &[u8]) -> Result<()> {
    match &cert.public_key {
        CertPublicKey::EcdsaP256(point) => {
            let key = UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point);
            key.verify(msg, sig)
                .map_err(|_| Error::CertificateVerifyFailed)
        }
        CertPublicKey::Rsa { modulus, exponent } => {
            let spki = rsa_pkcs1_public_key_der(modulus, exponent);
            let key = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, spki);
            key.verify(msg, sig)
                .map_err(|_| Error::CertificateVerifyFailed)
        }
    }
}

/// Build the DER `RSAPublicKey ::= SEQUENCE { modulus INTEGER, exponent INTEGER }`
/// that `ring` expects for `UnparsedPublicKey`. The modulus/exponent come from
/// the parsed SPKI and are already in big-endian DER `INTEGER` content form
/// (`parse.rs` keeps the leading sign byte), so we re-wrap them verbatim.
fn rsa_pkcs1_public_key_der(modulus: &[u8], exponent: &[u8]) -> Vec<u8> {
    let modulus_field = der_integer(modulus);
    let exponent_field = der_integer(exponent);
    let mut body = Vec::with_capacity(modulus_field.len().saturating_add(exponent_field.len()));
    body.extend_from_slice(&modulus_field);
    body.extend_from_slice(&exponent_field);
    der_sequence(&body)
}

/// DER-encode a big-endian, already-canonical `INTEGER` content as a TLV.
fn der_integer(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len().saturating_add(4));
    out.push(0x02); // INTEGER
    encode_len(&mut out, content.len());
    out.extend_from_slice(content);
    out
}

/// DER-encode a `SEQUENCE` wrapping the supplied (already-encoded) fields.
fn der_sequence(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len().saturating_add(4));
    out.push(0x30); // SEQUENCE
    encode_len(&mut out, body.len());
    out.extend_from_slice(body);
    out
}

/// Append a DER definite-length encoding for `len` to `out`.
fn encode_len(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        // Short form: a single byte < 0x80 fits the length directly.
        #[allow(clippy::cast_possible_truncation)]
        out.push(len as u8);
        return;
    }
    // Long form: 0x80 | (#length-octets), then the big-endian length octets.
    let bytes = len.to_be_bytes();
    let first_nonzero = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let significant = &bytes[first_nonzero..];
    let count = significant.len();
    debug_assert!(count <= 0x7f, "length too large for DER long form");
    #[allow(clippy::cast_possible_truncation)]
    out.push(0x80u8 | (count as u8));
    out.extend_from_slice(significant);
}

#[cfg(test)]
mod tests {
    use rcgen::KeyPair;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};

    use super::*;
    use crate::staking::{new_cert_and_key_bytes, parse_certificate};

    /// Generate an ECDSA P-256 staking cert, sign a message with its private key
    /// (ASN.1 sig, SHA-256), and confirm `check_signature` accepts it and
    /// rejects a tampered message.
    #[test]
    fn ecdsa_p256_check_signature_roundtrip() {
        let (cert_pem, key_pem) = new_cert_and_key_bytes().expect("gen cert");

        // Re-read the cert DER from PEM and parse it via the strict parser.
        let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .next()
            .expect("a cert block")
            .expect("valid cert pem");
        let cert = parse_certificate(&cert_der).expect("parse generated cert");

        // Load the PKCS#8 key into ring and sign.
        let key_pair = KeyPair::from_pem(&key_pem).expect("parse key pem");
        let pkcs8 = key_pair.serialize_der();
        let rng = SystemRandom::new();
        let signing = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng)
            .expect("import pkcs8");

        let msg = b"avalanche staking handshake";
        let sig = signing.sign(&rng, msg).expect("sign");

        check_signature(&cert, msg, sig.as_ref()).expect("valid signature verifies");

        // Tampered message must fail.
        let bad = check_signature(&cert, b"different message", sig.as_ref());
        assert!(matches!(bad, Err(Error::CertificateVerifyFailed)));
    }
}
