// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The local staking identity (cert + key) used for TLS + IP signing.
//!
//! Mirrors the per-node staking credential from Go (`staking/tls.go` +
//! `network/peer/tls_config.go`): one self-signed leaf certificate — ECDSA
//! P-256 (the Go/rcgen-generated template) or RSA (avalanchego's alternate
//! local-network staker format, PKCS#1/PKCS#8) — and its private key. The
//! same credential is presented in *both* TLS directions (mutual auth) and is
//! also the TLS signer for IP signing (`specs/05` §1.6) — for both key
//! families ([`TlsSigningKey`]), mirroring Go's `crypto.Signer`-based
//! `network/peer/ip_signer.go`.

use std::sync::Arc;

use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, RSA_PKCS1_SHA256, RsaKeyPair};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::error::{Error, Result};

impl core::fmt::Debug for Identity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Identity")
            .field("cert_der_len", &self.cert_len())
            .field("key", &"<redacted>")
            .finish()
    }
}

/// A local staking identity: the leaf certificate (DER) + its private key.
///
/// Cloning is cheap (`Arc`-shared key material). The `Debug` impl deliberately
/// omits the private key material.
#[derive(Clone)]
pub struct Identity {
    /// The DER-encoded leaf certificate (`cert.Raw` in Go).
    cert_der: Arc<Vec<u8>>,
    /// The private key in its original DER encoding (PKCS#1, SEC1, or
    /// PKCS#8 — whichever the source PEM used). Preserving the original
    /// encoding (rather than coercing to PKCS#8) is what lets `rustls_key`
    /// hand RSA PKCS#1 keys back to `rustls` unchanged.
    key_der: Arc<PrivateKeyDer<'static>>,
}

impl Identity {
    /// Generate a fresh staking identity (ECDSA P-256 self-signed, the Go
    /// template) via `ava-crypto`.
    ///
    /// # Errors
    /// [`Error::Identity`] if cert/key generation or PEM decoding fails.
    pub fn generate() -> Result<Identity> {
        let (cert_pem, key_pem) = ava_crypto::staking::new_cert_and_key_bytes()
            .map_err(|e| Error::Identity(e.to_string()))?;
        Identity::from_pem(&cert_pem, &key_pem)
    }

    /// Build an identity from a PEM cert block + a PEM private-key block
    /// (matching the on-disk `staker.crt` / `staker.key` format). Accepts
    /// ECDSA keys (PKCS#8, the Go/rcgen template) and RSA keys (PKCS#1 or
    /// PKCS#8 — avalanchego mints its local-network RSA stakers as PKCS#1
    /// `RSA PRIVATE KEY` blocks).
    ///
    /// # Errors
    /// [`Error::Identity`] if either PEM block is missing/malformed, or the
    /// key is not a type this crate's `rustls` crypto provider can sign with.
    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Identity> {
        let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .next()
            .ok_or_else(|| Error::Identity("no CERTIFICATE block in cert PEM".into()))?
            .map_err(|e| Error::Identity(format!("invalid cert PEM: {e}")))?;

        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .map_err(|e| Error::Identity(format!("invalid key PEM: {e}")))?
            .ok_or_else(|| Error::Identity("no PRIVATE KEY block in key PEM".into()))?;

        // Validate the key against this crate's `rustls` crypto provider (the
        // `ring` default provider, matching `peer::tls_config`) rather than
        // special-casing the DER encoding: the provider's generic loader
        // handles RSA (PKCS#1/PKCS#8) and ECDSA (SEC1/PKCS#8) uniformly, and
        // rejects a genuinely unsupported/malformed key here instead of
        // deferring the failure to `tls_config::server_config`'s
        // `with_single_cert` call.
        default_provider()
            .key_provider
            .load_private_key(key.clone_key())
            .map_err(|e| Error::Identity(format!("unsupported staking key: {e}")))?;

        Ok(Identity {
            cert_der: Arc::new(cert_der.as_ref().to_vec()),
            key_der: Arc::new(key),
        })
    }

    /// The DER-encoded leaf certificate.
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// Returns the DER length (used by the `Debug` impl, avoids leaking bytes).
    #[must_use]
    fn cert_len(&self) -> usize {
        self.cert_der.len()
    }

    /// The leaf certificate as a `rustls` owned `CertificateDer`.
    #[must_use]
    pub fn rustls_cert(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert_der.as_ref().clone())
    }

    /// The private key as a `rustls` owned `PrivateKeyDer`, in its original
    /// DER encoding (PKCS#1, SEC1, or PKCS#8).
    #[must_use]
    pub fn rustls_key(&self) -> PrivateKeyDer<'static> {
        self.key_der.clone_key()
    }

    /// Load the private key into a [`TlsSigningKey`], used for IP signing
    /// (`specs/05` §1.6). Both key families this crate accepts from
    /// [`Identity::from_pem`] can sign: ECDSA P-256 (PKCS#8) and RSA (PKCS#1
    /// or PKCS#8) — mirroring Go `network/peer/ip_signer.go`, which signs via
    /// a generic `crypto.Signer` regardless of the staking key's type.
    ///
    /// # Errors
    /// [`Error::Signing`] if the key is not one of the two supported
    /// families, or the `ring` key parse fails.
    pub fn tls_signing_key(&self) -> Result<TlsSigningKey> {
        let rng = SystemRandom::new();
        match self.key_der.as_ref() {
            PrivateKeyDer::Pkcs8(pkcs8) => {
                let der = pkcs8.secret_pkcs8_der();
                // ECDSA staking keys are always PKCS#8 (Go/rcgen's template);
                // try that first. An RSA key wrapped in PKCS#8 (a valid, if
                // less common, encoding for avalanchego RSA stakers) fails
                // `from_pkcs8`'s algorithm-OID check and falls through to RSA.
                if let Ok(ecdsa) =
                    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, der, &rng)
                {
                    return Ok(TlsSigningKey::EcdsaP256(ecdsa));
                }
                RsaKeyPair::from_pkcs8(der)
                    .map(TlsSigningKey::Rsa)
                    .map_err(|e| Error::Signing(format!("import staking key: {e}")))
            }
            // avalanchego mints its RSA local-network stakers as PKCS#1
            // (`RSA PRIVATE KEY`); `ring::signature::RsaKeyPair::from_der`
            // parses the raw (unwrapped) `RSAPrivateKey` DER `from_pkcs8`
            // wants a PKCS#8 envelope around, which a PKCS#1 key does not have.
            PrivateKeyDer::Pkcs1(pkcs1) => RsaKeyPair::from_der(pkcs1.secret_pkcs1_der())
                .map(TlsSigningKey::Rsa)
                .map_err(|e| Error::Signing(format!("import staking key: {e}"))),
            _ => Err(Error::Signing(
                "IP signing requires a PKCS#1/PKCS#8 RSA or PKCS#8 ECDSA staking key".into(),
            )),
        }
    }
}

/// A loaded staking-key signer used for IP signing (`specs/05` §1.6),
/// dispatching on the staking identity's key family. Mirrors Go
/// `network/peer/ip_signer.go` signing through a generic `crypto.Signer`
/// regardless of whether the underlying key is ECDSA or RSA.
pub enum TlsSigningKey {
    /// ECDSA P-256; signatures are ASN.1/DER-encoded over `SHA256(msg)`.
    EcdsaP256(EcdsaKeyPair),
    /// RSA; signatures are PKCS#1 v1.5 over `SHA256(msg)` — matching Go
    /// `staking/verify.go::CheckSignature`'s RSA branch
    /// (`RSA_PKCS1_2048_8192_SHA256` on the verify side,
    /// `RSA_PKCS1_SHA256` here on the sign side).
    Rsa(RsaKeyPair),
}

impl TlsSigningKey {
    /// Sign `msg`, producing the raw signature bytes `SignedIp::tls_signature`
    /// carries: ASN.1/DER for ECDSA, PKCS#1 v1.5 for RSA. `ring` hashes `msg`
    /// with SHA-256 internally in both cases (== signing `SHA256(msg)`).
    ///
    /// # Errors
    /// [`Error::Signing`] if the underlying `ring` sign operation fails.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>> {
        let rng = SystemRandom::new();
        match self {
            TlsSigningKey::EcdsaP256(key) => key
                .sign(&rng, msg)
                .map(|sig| sig.as_ref().to_vec())
                .map_err(|_| Error::Signing("ecdsa sign failed".into())),
            TlsSigningKey::Rsa(key) => {
                let mut sig = vec![0u8; key.public().modulus_len()];
                key.sign(&RSA_PKCS1_SHA256, &rng, msg, &mut sig)
                    .map(|()| sig)
                    .map_err(|_| Error::Signing("rsa sign failed".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

    use super::*;

    #[test]
    fn generate_then_load_signing_key() {
        let id = Identity::generate().expect("generate identity");
        assert!(!id.cert_der().is_empty());
        // The staking key imports as a P-256 ECDSA signer.
        id.tls_signing_key().expect("load signing key");
    }

    #[test]
    fn from_pem_round_trips_a_p256_cert() {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("p256 key");
        let cert = CertificateParams::default()
            .self_signed(&key)
            .expect("self-sign");
        let id = Identity::from_pem(&cert.pem(), &key.serialize_pem()).expect("from_pem");
        assert_eq!(id.cert_der(), cert.der().as_ref());
    }

    #[test]
    fn from_pem_loads_an_rsa_cert() {
        let cert = include_str!("../tests/testdata/rsa_staker.crt");
        let key = include_str!("../tests/testdata/rsa_staker.key");
        let id = Identity::from_pem(cert, key).expect("Identity::from_pem(rsa)");
        // NodeID pinned from the one-time generation step
        // (`crates/ava-network/tests/testdata/rsa_staker.{crt,key}`).
        let node_id = crate::peer::upgrader::node_id_from_cert_der(id.cert_der())
            .expect("node_id_from_cert_der(rsa)");
        assert_eq!(
            node_id.to_string(),
            "NodeID-Foj2bN48Hm4orFr5Hg3ttEZYrNCUQf9tz"
        );
    }

    #[test]
    fn tls_signing_key_signs_with_rsa_identity() {
        // The RSA fixture must be able to produce a signed IP claim (Task 8:
        // the live validator's genesis staker slot is an RSA identity).
        // `tls_signing_key` must return an RSA signer (PKCS#1 v1.5/SHA-256),
        // and the resulting signature must verify against the RSA cert's
        // public key via the same `ava_crypto::staking::check_signature` path
        // real peers use (mirrors Go `staking/verify.go::CheckSignature`'s
        // RSA branch).
        let cert = include_str!("../tests/testdata/rsa_staker.crt");
        let key = include_str!("../tests/testdata/rsa_staker.key");
        let id = Identity::from_pem(cert, key).expect("Identity::from_pem(rsa)");

        let signer = id.tls_signing_key().expect("tls_signing_key(rsa)");
        let msg = b"avalanche staking handshake";
        let sig = signer.sign(msg).expect("rsa sign");

        let parsed_cert =
            ava_crypto::staking::parse_certificate(id.cert_der()).expect("parse rsa cert");
        ava_crypto::staking::check_signature(&parsed_cert, msg, &sig)
            .expect("rsa signature verifies under PKCS#1v1.5/SHA-256");
    }

    #[test]
    fn from_pem_rejects_empty_cert() {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("p256 key");
        assert_matches!(
            Identity::from_pem("", &key.serialize_pem()),
            Err(Error::Identity(_))
        );
    }
}
