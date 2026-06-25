// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Custom `rustls` certificate verifiers — the leaf-key identity policy.
//!
//! Avalanche authenticates peers purely by their **self-signed leaf public
//! key**, not a CA chain (`specs/05` §1.6). rustls' default verifiers require a
//! chain to a trust anchor, so we provide custom verifiers that reproduce Go's
//! `InsecureSkipVerify + VerifyConnection(ValidateCertificate)` behavior.
//!
//! The verifiers live in the [`danger`] module because they deliberately
//! override certificate-chain verification. They are NOT insecure: the real
//! TLS 1.3 handshake-signature check still runs — `verify_tls13_signature`
//! extracts the leaf SPKI and verifies it via the raw-key path
//! (`verify_tls13_signature_with_raw_key`), proving the peer holds the private
//! key for the presented leaf. The raw-key path is used (instead of the
//! cert-based variant) because avalanchego mints RSA staking certs as X.509 v1,
//! which webpki's cert parser rejects. This module contains no `unsafe` code
//! (`specs/00` §7.6).

/// Verifiers that override the default certificate-chain verification, applying
/// only Avalanche's leaf-public-key policy (`specs/05` §4.4/§4.5).
pub mod danger {
    use std::sync::Arc;

    use rustls::DigitallySignedStruct;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{
        CryptoProvider, WebPkiSupportedAlgorithms, verify_tls13_signature_with_raw_key,
    };
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
    use rustls::{DistinguishedName, SignatureScheme};

    /// Server-side client-cert verifier: requires a client cert (mutual TLS)
    /// and runs the leaf-key policy, with no CA chain check. Mirrors Go's
    /// `ClientAuth = RequireAnyClientCert` + `ValidateCertificate`.
    #[derive(Debug)]
    pub struct AvaClientCertVerifier {
        supported: WebPkiSupportedAlgorithms,
    }

    /// Client-side server-cert verifier: the mirror of [`AvaClientCertVerifier`].
    #[derive(Debug)]
    pub struct AvaServerCertVerifier {
        supported: WebPkiSupportedAlgorithms,
    }

    impl AvaClientCertVerifier {
        /// Build the verifier from the active crypto provider's supported
        /// signature algorithms.
        #[must_use]
        pub fn new(provider: &Arc<CryptoProvider>) -> Arc<Self> {
            Arc::new(Self {
                supported: provider.signature_verification_algorithms,
            })
        }
    }

    impl AvaServerCertVerifier {
        /// Build the verifier from the active crypto provider's supported
        /// signature algorithms.
        #[must_use]
        pub fn new(provider: &Arc<CryptoProvider>) -> Arc<Self> {
            Arc::new(Self {
                supported: provider.signature_verification_algorithms,
            })
        }
    }

    impl ClientCertVerifier for AvaClientCertVerifier {
        fn root_hint_subjects(&self) -> &[DistinguishedName] {
            &[]
        }

        fn offer_client_auth(&self) -> bool {
            true
        }

        // == tls.RequireAnyClientCert: a client cert is mandatory.
        fn client_auth_mandatory(&self) -> bool {
            true
        }

        fn verify_client_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _now: UnixTime,
        ) -> Result<ClientCertVerified, rustls::Error> {
            super::validate_leaf_public_key(end_entity).map_err(super::policy_to_rustls)?;
            Ok(ClientCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            // TLS 1.3 only — 1.2 is disabled at config level; never reached.
            Err(rustls::Error::PeerIncompatible(
                rustls::PeerIncompatible::Tls12NotOffered,
            ))
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            // webpki (the default `verify_tls13_signature`) rejects avalanchego's
            // X.509 v1 RSA staking certs with `UnsupportedCertVersion`. We extract
            // the leaf SPKI ourselves (v1-tolerant) and verify against the raw key,
            // bypassing webpki's cert-version gate (`specs/05` §4.4/§4.5).
            let spki = super::leaf_spki_der(cert)?;
            verify_tls13_signature_with_raw_key(message, &spki, dss, &self.supported)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.supported.supported_schemes()
        }
    }

    impl ServerCertVerifier for AvaServerCertVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            super::validate_leaf_public_key(end_entity).map_err(super::policy_to_rustls)?;
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Err(rustls::Error::PeerIncompatible(
                rustls::PeerIncompatible::Tls12NotOffered,
            ))
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            // webpki (the default `verify_tls13_signature`) rejects avalanchego's
            // X.509 v1 RSA staking certs with `UnsupportedCertVersion`. We extract
            // the leaf SPKI ourselves (v1-tolerant) and verify against the raw key,
            // bypassing webpki's cert-version gate (`specs/05` §4.4/§4.5).
            let spki = super::leaf_spki_der(cert)?;
            verify_tls13_signature_with_raw_key(message, &spki, dss, &self.supported)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.supported.supported_schemes()
        }
    }
}

use rustls::pki_types::{CertificateDer, SubjectPublicKeyInfoDer};
use x509_parser::prelude::FromDer;
use x509_parser::public_key::PublicKey as X509PublicKey;

use crate::error::{Error, Result};

/// Map a leaf-key policy rejection to a `rustls` handshake error. A missing /
/// unparsable cert is reported as `BadEncoding`; any policy violation as an
/// application verification failure (the handshake is then aborted).
fn policy_to_rustls(e: Error) -> rustls::Error {
    use rustls::CertificateError;
    let cert_err = match e {
        Error::EmptyCert | Error::NoCertsSent => CertificateError::BadEncoding,
        _ => CertificateError::ApplicationVerificationFailure,
    };
    rustls::Error::InvalidCertificate(cert_err)
}

/// Extract the leaf certificate's `SubjectPublicKeyInfo` DER, tolerant of X.509
/// v1 certs (avalanchego mints RSA staking certs as v1, which webpki refuses).
/// Used by the verifiers' raw-key TLS-1.3 signature check. A parse failure maps
/// to `BadEncoding`, matching `policy_to_rustls`'s treatment of unparsable certs.
fn leaf_spki_der(
    cert: &CertificateDer<'_>,
) -> core::result::Result<SubjectPublicKeyInfoDer<'static>, rustls::Error> {
    let (_, parsed) = x509_parser::certificate::X509Certificate::from_der(cert.as_ref())
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    Ok(SubjectPublicKeyInfoDer::from(
        parsed.public_key().raw.to_vec(),
    ))
}

/// Allowed RSA modulus bit lengths (Go `staking.allowedRSAModulusBitLens`).
const ALLOWED_RSA_MODULUS_BITS: [usize; 2] = [2048, 4096];

/// The only allowed RSA public exponent (Go `staking.allowedRSAPublicExponent`).
const ALLOWED_RSA_EXPONENT: u64 = 65537;

/// P-256 public-key size in bits.
const P256_KEY_BITS: usize = 256;

/// The leaf-key policy — Go `ValidateCertificate` (`specs/05` §1.6/§4.5).
///
/// Authenticates a peer by its self-signed leaf public key only (no CA chain):
/// - ECDSA ⇒ the curve MUST be P-256, else [`Error::CurveMismatch`].
/// - RSA ⇒ the key must be well-formed (modulus 2048/4096 bits, positive, odd;
///   exponent 65537), matching `staking::ValidateRSAPublicKeyIsWellFormed`.
/// - any other key type ⇒ [`Error::UnsupportedKeyType`].
///
/// # Errors
/// [`Error::EmptyCert`] if the DER fails to parse; [`Error::CurveMismatch`],
/// [`Error::UnsupportedKeyType`] per the policy above.
pub fn validate_leaf_public_key(der: &CertificateDer<'_>) -> Result<()> {
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(der.as_ref())
        .map_err(|_| Error::EmptyCert)?;

    let parsed = cert
        .public_key()
        .parsed()
        .map_err(|_| Error::UnsupportedKeyType)?;

    match parsed {
        X509PublicKey::EC(ec) => {
            // Go restricts ECDSA staking keys to the P-256 curve.
            if ec.key_size() == P256_KEY_BITS {
                Ok(())
            } else {
                Err(Error::CurveMismatch)
            }
        }
        X509PublicKey::RSA(rsa) => validate_rsa_well_formed(rsa.modulus, rsa.exponent),
        _ => Err(Error::UnsupportedKeyType),
    }
}

/// `staking.ValidateRSAPublicKeyIsWellFormed` — modulus positive, odd, and
/// exactly 2048 or 4096 bits; exponent exactly 65537 (`specs/03` §3.6). Returns
/// [`Error::UnsupportedKeyType`] for any non-conformant RSA key (Go reports a
/// dedicated RSA error here; in the verifier path it is collapsed to the same
/// "reject" outcome).
fn validate_rsa_well_formed(modulus: &[u8], exponent: &[u8]) -> Result<()> {
    // Exponent must be exactly 65537.
    let exp = be_bytes_to_u64(exponent);
    if exp != Some(ALLOWED_RSA_EXPONENT) {
        return Err(Error::UnsupportedKeyType);
    }
    // Modulus must be positive, odd, and exactly 2048 or 4096 bits.
    let bits = modulus_bit_len(modulus);
    if bits == 0 || modulus_is_even(modulus) || !ALLOWED_RSA_MODULUS_BITS.contains(&bits) {
        return Err(Error::UnsupportedKeyType);
    }
    Ok(())
}

/// Bit length of a big-endian DER `INTEGER` modulus, ignoring leading-zero
/// padding. Returns 0 for a non-positive (zero/empty) modulus.
fn modulus_bit_len(modulus: &[u8]) -> usize {
    let Some(idx) = modulus.iter().position(|&b| b != 0) else {
        return 0;
    };
    let Some(significant) = modulus.get(idx..) else {
        return 0;
    };
    let Some(&top) = significant.first() else {
        return 0;
    };
    let leading_zeros = top.leading_zeros() as usize;
    8usize
        .saturating_mul(significant.len())
        .saturating_sub(leading_zeros)
}

/// Whether the big-endian modulus is even (low bit of the last byte is 0).
fn modulus_is_even(modulus: &[u8]) -> bool {
    match modulus.last() {
        None => true,
        Some(&last) => last & 1 == 0,
    }
}

/// Parse a big-endian byte slice as a `u64`, ignoring leading zeros. Returns
/// `None` if the significant bytes do not fit in a `u64`.
fn be_bytes_to_u64(bytes: &[u8]) -> Option<u64> {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let significant = bytes.get(start..)?;
    if significant.len() > 8 {
        return None;
    }
    let mut acc: u64 = 0;
    for &b in significant {
        acc = acc.checked_shl(8)?.checked_add(u64::from(b))?;
    }
    Some(acc)
}
