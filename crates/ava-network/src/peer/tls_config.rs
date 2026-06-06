// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! TLS 1.3-only, mutual `rustls` server/client configs (`specs/05` §4.1/§4.2).
//!
//! Mirrors Go `network/peer/tls_config.go::TLSConfig`:
//! - TLS 1.3 only (`MinVersion = VersionTLS13`) — the single most important
//!   interop constant; both peers negotiate from the TLS 1.3 suite set.
//! - Mutual auth: the server requires a client cert (`RequireAnyClientCert`),
//!   and both directions present our staking cert + key.
//! - No ALPN, no SNI/hostname verification — peers are authenticated purely by
//!   their leaf public key → derived NodeID (the custom verifiers).
//! - The default `ring` provider's unmodified TLS 1.3 suite list (matches Go's
//!   `crypto/tls`: `AES_128_GCM`, `AES_256_GCM`, `CHACHA20_POLY1305`).

use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::crypto::ring::default_provider;
use rustls::{ClientConfig, ServerConfig};

use super::verifier::danger::{AvaClientCertVerifier, AvaServerCertVerifier};
use crate::error::{Error, Result};
use crate::identity::Identity;

/// The single supported protocol version: TLS 1.3 only.
const TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];

/// Build the server-side `rustls` config from a local staking identity.
///
/// TLS 1.3 only, mutual auth (requires a client cert via
/// [`AvaClientCertVerifier`]), presents our staking cert + key, no ALPN.
///
/// # Errors
/// [`Error::TlsConfig`] if the provider/cert cannot be configured.
pub fn server_config(identity: &Identity) -> Result<Arc<ServerConfig>> {
    let provider = Arc::new(default_provider());
    let verifier = AvaClientCertVerifier::new(&provider);

    let config = ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(TLS13_ONLY)
        .map_err(|e| Error::TlsConfig(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.rustls_cert()], identity.rustls_key())
        .map_err(|e| Error::TlsConfig(e.to_string()))?;

    // No ALPN (Go sets none); leave `alpn_protocols` empty.
    Ok(Arc::new(config))
}

/// Build the client-side `rustls` config from a local staking identity.
///
/// TLS 1.3 only, presents our staking cert + key (so the server's mutual-auth
/// requirement is satisfied), uses the custom [`AvaServerCertVerifier`] (no CA
/// chain / no SNI), no ALPN.
///
/// # Errors
/// [`Error::TlsConfig`] if the provider/cert cannot be configured.
pub fn client_config(identity: &Identity) -> Result<Arc<ClientConfig>> {
    let provider = Arc::new(default_provider());
    let verifier = AvaServerCertVerifier::new(&provider);

    let config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(TLS13_ONLY)
        .map_err(|e| Error::TlsConfig(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![identity.rustls_cert()], identity.rustls_key())
        .map_err(|e| Error::TlsConfig(e.to_string()))?;

    Ok(Arc::new(config))
}

/// The crypto provider used by both configs (the `ring` default provider).
/// Exposed so the verifiers / tests can reference the same provider.
#[must_use]
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(default_provider())
}

/// The exact set of TLS protocol versions both configs enable. rustls keeps the
/// enabled-version set private on the built config, so this thin accessor lets
/// callers/tests assert the TLS1.3-only policy at the source of truth
/// (`specs/05` §4.1). It is always `[TLS13]`.
#[must_use]
pub fn enabled_protocol_versions() -> &'static [&'static rustls::SupportedProtocolVersion] {
    TLS13_ONLY
}

/// Whether the server config mandates a client certificate (mutual TLS). Always
/// `true` — mirrors Go's `ClientAuth = RequireAnyClientCert` (`specs/05` §4.4).
#[must_use]
pub fn server_requires_client_cert() -> bool {
    use rustls::server::danger::ClientCertVerifier;
    AvaClientCertVerifier::new(&provider()).client_auth_mandatory()
}
