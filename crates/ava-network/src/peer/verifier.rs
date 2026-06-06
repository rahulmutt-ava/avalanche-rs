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
//! TLS 1.3 handshake-signature check still runs (`verify_tls13_signature`
//! delegates to the crypto provider), proving the peer holds the private key
//! for the presented leaf — exactly what Go achieves. This module contains no
//! `unsafe` code (`specs/00` §7.6).

/// Verifiers that override the default certificate-chain verification, applying
/// only Avalanche's leaf-public-key policy (`specs/05` §4.4/§4.5).
pub mod danger {
    use std::sync::Arc;

    use rustls::DigitallySignedStruct;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls13_signature};
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
            super::validate_leaf_public_key(end_entity)?;
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
            verify_tls13_signature(message, cert, dss, &self.supported)
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
            super::validate_leaf_public_key(end_entity)?;
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
            verify_tls13_signature(message, cert, dss, &self.supported)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.supported.supported_schemes()
        }
    }
}

/// Placeholder leaf-key policy — accepts any leaf (replaced in M2.8 with the
/// real P-256/RSA policy). Kept private until the policy lands.
#[allow(clippy::unnecessary_wraps)]
fn validate_leaf_public_key(
    _der: &rustls::pki_types::CertificateDer<'_>,
) -> Result<(), rustls::Error> {
    Ok(())
}
