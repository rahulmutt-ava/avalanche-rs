// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

#![allow(unused_crate_dependencies)] // integration tests don't use every workspace dep (specs/01 §7.3)
#![allow(clippy::expect_used)] // tests assert via expect()

//! M9.15 — a peer presenting an X.509 **v1 RSA** staking cert (avalanchego's
//! RSA stakers) must complete the TLS 1.3 mutual handshake. webpki rejects v1
//! certs with `UnsupportedCertVersion`; our verifiers must accept them via the
//! raw-key signature path (`specs/05` §4.4/§4.5).

use std::sync::Arc;

use ava_network::Identity;
use ava_network::peer::tls_config::{client_config, provider, server_config};
use ava_network::peer::upgrader::Upgrader;
use ava_network::peer::verifier::danger::{AvaClientCertVerifier, AvaServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::{ClientConfig, ServerConfig};

const TLS13: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];

/// Load the vendored v1 RSA staking cert + PKCS#1 key.
fn rsa_v1_identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert_pem = include_bytes!("fixtures/staker_rsa_v1.crt");
    let key_pem = include_bytes!("fixtures/staker_rsa_v1.key");
    let cert = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .next()
        .expect("a CERTIFICATE block")
        .expect("valid cert DER");
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .expect("parse key PEM")
        .expect("a PRIVATE KEY block");
    (cert, key)
}

/// Build a [`CertifiedKey`] for the v1 RSA pair without going through
/// `CertifiedKey::from_der`, which calls `ParsedCertificate::try_from` (webpki)
/// and rejects v1 certs. We load the private key directly and bypass the
/// SPKI-consistency check that would fail for X.509 v1.
fn rsa_v1_certified_key(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> Arc<CertifiedKey> {
    let prov = provider();
    let signing_key = prov
        .key_provider
        .load_private_key(key)
        .expect("load RSA private key");
    Arc::new(CertifiedKey::new(vec![cert], signing_key))
}

/// Server config that presents the given cert/key, with the real Ava mutual-auth
/// client verifier (mirrors `tls_config::server_config`, but for a raw RSA pair).
fn rsa_server_config(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> Arc<ServerConfig> {
    let prov = provider();
    let certified_key = rsa_v1_certified_key(cert, key);
    let verifier = AvaClientCertVerifier::new(&prov);
    Arc::new(
        ServerConfig::builder_with_provider(prov)
            .with_protocol_versions(TLS13)
            .expect("tls13 server")
            .with_client_cert_verifier(verifier)
            .with_cert_resolver(Arc::new(SingleCertAndKey::from(certified_key))),
    )
}

/// Client config that presents the given cert/key, with the real Ava server
/// verifier (mirrors `tls_config::client_config`, but for a raw RSA pair).
fn rsa_client_config(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> Arc<ClientConfig> {
    let prov = provider();
    let certified_key = rsa_v1_certified_key(cert, key);
    let verifier = AvaServerCertVerifier::new(&prov);
    Arc::new(
        ClientConfig::builder_with_provider(prov)
            .with_protocol_versions(TLS13)
            .expect("tls13 client")
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_cert_resolver(Arc::new(SingleCertAndKey::from(certified_key))),
    )
}

// The server presents a v1 RSA cert; the (ECDSA) client must verify it and the
// handshake must complete. Exercises `AvaServerCertVerifier::verify_tls13_signature`.
#[tokio::test]
async fn rsa_v1_server_cert_completes_handshake() {
    let (cert, key) = rsa_v1_identity();
    let server_cfg = rsa_server_config(cert, key);
    let client_cfg = client_config(&Identity::generate().expect("client id")).expect("client cfg");

    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move { Upgrader::server(server_cfg).upgrade(server_io).await });
    let client = tokio::spawn(async move { Upgrader::client(client_cfg).upgrade(client_io).await });

    server
        .await
        .expect("server task")
        .expect("server upgrade ok (v1 RSA server cert accepted)");
    client
        .await
        .expect("client task")
        .expect("client upgrade ok (v1 RSA server cert accepted)");
}

// The client presents a v1 RSA cert; the (ECDSA) server must verify it.
// Exercises `AvaClientCertVerifier::verify_tls13_signature`.
#[tokio::test]
async fn rsa_v1_client_cert_completes_handshake() {
    let (cert, key) = rsa_v1_identity();
    let server_cfg = server_config(&Identity::generate().expect("server id")).expect("server cfg");
    let client_cfg = rsa_client_config(cert, key);

    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move { Upgrader::server(server_cfg).upgrade(server_io).await });
    let client = tokio::spawn(async move { Upgrader::client(client_cfg).upgrade(client_io).await });

    server
        .await
        .expect("server task")
        .expect("server upgrade ok (v1 RSA client cert accepted)");
    client
        .await
        .expect("client task")
        .expect("client upgrade ok (v1 RSA client cert accepted)");
}
