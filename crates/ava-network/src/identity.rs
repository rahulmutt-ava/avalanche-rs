// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The local staking identity (cert + key) used for TLS + IP signing.
//!
//! Mirrors the per-node staking credential from Go (`staking/tls.go` +
//! `network/peer/tls_config.go`): one self-signed ECDSA P-256 leaf certificate
//! and its private key. The same credential is presented in *both* TLS
//! directions (mutual auth) and is also the TLS signer for IP signing
//! (`specs/05` §1.6).

use std::sync::Arc;

use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::error::{Error, Result};

/// A local staking identity: the leaf certificate (DER) + its private key.
///
/// Cloning is cheap (`Arc`-shared key material).
#[derive(Clone)]
pub struct Identity {
    /// The DER-encoded leaf certificate (`cert.Raw` in Go).
    cert_der: Arc<Vec<u8>>,
    /// The PKCS#8 DER-encoded private key.
    key_pkcs8_der: Arc<Vec<u8>>,
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

    /// Build an identity from PEM cert + PEM PKCS#8 key blocks (matching the
    /// on-disk `staker.crt` / `staker.key` format).
    ///
    /// # Errors
    /// [`Error::Identity`] if either PEM block is missing or malformed.
    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Identity> {
        let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .next()
            .ok_or_else(|| Error::Identity("no CERTIFICATE block in cert PEM".into()))?
            .map_err(|e| Error::Identity(format!("invalid cert PEM: {e}")))?;

        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .map_err(|e| Error::Identity(format!("invalid key PEM: {e}")))?
            .ok_or_else(|| Error::Identity("no PRIVATE KEY block in key PEM".into()))?;

        let key_pkcs8 = match key {
            PrivateKeyDer::Pkcs8(k) => k.secret_pkcs8_der().to_vec(),
            // ECDSA staking keys are emitted as PKCS#8 by both Go and rcgen; any
            // other encoding is unexpected for a staking identity.
            _ => return Err(Error::Identity("staking key is not PKCS#8".into())),
        };

        Ok(Identity {
            cert_der: Arc::new(cert_der.as_ref().to_vec()),
            key_pkcs8_der: Arc::new(key_pkcs8),
        })
    }

    /// The DER-encoded leaf certificate.
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// The leaf certificate as a `rustls` owned `CertificateDer`.
    #[must_use]
    pub fn rustls_cert(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert_der.as_ref().clone())
    }

    /// The private key as a `rustls` owned `PrivateKeyDer` (PKCS#8).
    #[must_use]
    pub fn rustls_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_pkcs8_der.as_ref().clone()))
    }

    /// Load the private key into a `ring` ECDSA signing key (ASN.1/DER sig,
    /// SHA-256), used for IP signing (`specs/05` §1.6).
    ///
    /// # Errors
    /// [`Error::Signing`] if the PKCS#8 key cannot be imported.
    pub fn tls_signing_key(&self) -> Result<EcdsaKeyPair> {
        let rng = SystemRandom::new();
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &self.key_pkcs8_der, &rng)
            .map_err(|e| Error::Signing(format!("import staking key: {e}")))
    }
}
