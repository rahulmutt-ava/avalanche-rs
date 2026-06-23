// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! TLS `Upgrader` + NodeID-from-cert (`specs/05` §1.6/§4.3).
//!
//! Mirrors Go `network/peer/upgrader.go`: after the TLS 1.3 handshake completes,
//! extract the peer's leaf certificate, strict-parse it (`staking.ParseCertificate`),
//! and derive the peer's NodeID = `RIPEMD160(SHA256(cert.Raw))`
//! (`ids.NodeIDFromCert`). The same upgrade path serves the inbound (server) and
//! outbound (client) directions.

use std::sync::Arc;

use ava_crypto::staking::{Certificate, parse_certificate};
use ava_types::node_id::NodeId;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ServerConfig};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};

use crate::error::{Error, Result};

/// `ids.NodeIDFromCert` over raw DER: `RIPEMD160(SHA256(DER))`. Convenience
/// wrapper that strict-parses the DER first (so a malformed cert is rejected).
///
/// # Errors
/// [`Error::CertificateParse`] if the DER fails the strict staking parser.
pub fn node_id_from_cert_der(der: &[u8]) -> Result<NodeId> {
    // Parse to validate the DER conforms to the staking policy, then derive the
    // ID from the canonical `cert.Raw` bytes.
    let cert = parse_certificate(der)?;
    Ok(ava_crypto::staking::node_id_from_cert(&cert.raw))
}

/// `ids.NodeIDFromCert` over a parsed [`Certificate`].
#[must_use]
pub fn node_id_from_cert(cert: &Certificate) -> NodeId {
    ava_crypto::staking::node_id_from_cert(&cert.raw)
}

/// Which TLS role this upgrader plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgraderSide {
    /// Inbound connection — we are the TLS server.
    Server,
    /// Outbound connection — we are the TLS client.
    Client,
}

/// Upgrades a raw byte stream to a mutually-authenticated TLS 1.3 channel and
/// derives the peer NodeID from its leaf certificate. Thread-safe + cheap to
/// clone (`Arc`-backed configs).
#[derive(Clone)]
pub struct Upgrader {
    side: UpgraderSide,
    acceptor: Option<TlsAcceptor>,
    connector: Option<TlsConnector>,
}

impl Upgrader {
    /// Build a server-side (inbound) upgrader from a [`ServerConfig`].
    #[must_use]
    pub fn server(config: Arc<ServerConfig>) -> Upgrader {
        Upgrader {
            side: UpgraderSide::Server,
            acceptor: Some(TlsAcceptor::from(config)),
            connector: None,
        }
    }

    /// Build a client-side (outbound) upgrader from a [`ClientConfig`].
    #[must_use]
    pub fn client(config: Arc<ClientConfig>) -> Upgrader {
        Upgrader {
            side: UpgraderSide::Client,
            acceptor: None,
            connector: Some(TlsConnector::from(config)),
        }
    }

    /// This upgrader's role.
    #[must_use]
    pub fn side(&self) -> UpgraderSide {
        self.side
    }

    /// Perform the TLS handshake over `stream` and return the peer NodeID, the
    /// established TLS stream, and the parsed peer certificate.
    ///
    /// Generic over the byte stream so the same code drives both production
    /// `TcpStream`s and in-process `tokio::io::duplex` test streams.
    ///
    /// # Errors
    /// - [`Error::Tls`] if the TLS handshake fails (incl. a rejected leaf key).
    /// - [`Error::NoPeerCertificate`] if no peer cert was negotiated.
    /// - [`Error::CertificateParse`] if the leaf fails the strict parser.
    pub async fn upgrade<IO>(&self, stream: IO) -> Result<(NodeId, TlsStream<IO>, Certificate)>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let tls: TlsStream<IO> =
            match self.side {
                UpgraderSide::Server => {
                    let acceptor = self.acceptor.as_ref().ok_or_else(|| {
                        Error::TlsConfig("server upgrader missing acceptor".into())
                    })?;
                    let accepted = acceptor
                        .accept(stream)
                        .await
                        .map_err(|e| Error::Tls(e.to_string()))?;
                    TlsStream::Server(accepted)
                }
                UpgraderSide::Client => {
                    let connector = self.connector.as_ref().ok_or_else(|| {
                        Error::TlsConfig("client upgrader missing connector".into())
                    })?;
                    // No SNI / hostname verification (the custom verifier authenticates
                    // by leaf key, not hostname). A fixed placeholder name is used.
                    let server_name = ServerName::try_from("avalanche")
                        .map_err(|e| Error::TlsConfig(format!("server name: {e}")))?;
                    let connected = connector
                        .connect(server_name, stream)
                        .await
                        .map_err(|e| Error::Tls(e.to_string()))?;
                    TlsStream::Client(connected)
                }
            };

        let (_io, conn) = tls.get_ref();
        let leaf = conn
            .peer_certificates()
            .and_then(<[_]>::first)
            .ok_or(Error::NoPeerCertificate)?;

        let cert = parse_certificate(leaf.as_ref())?;
        let node_id = node_id_from_cert(&cert);

        let key_type = match &cert.public_key {
            ava_crypto::staking::CertPublicKey::EcdsaP256(_) => "ecdsa-p256",
            ava_crypto::staking::CertPublicKey::Rsa { .. } => "rsa",
        };
        tracing::debug!(
            %node_id,
            side = ?self.side,
            key_type,
            "TLS upgrade complete: derived peer NodeID"
        );

        Ok((node_id, tls, cert))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::peer::tls_config;

    /// Regression (M9.15 D3): an outbound client TLS upgrade over a peer that
    /// never speaks TLS (the remote half is dropped immediately) resolves to an
    /// `Err`, not a hang or panic. This is exactly the result arm
    /// `network::net_impl::handle_dial` now matches on and logs ("outbound TLS
    /// upgrade failed") — the live mixed-net handshake investigation depends on
    /// that `Err` surfacing rather than being swallowed.
    #[tokio::test]
    async fn client_upgrade_over_a_silent_peer_is_err_not_a_hang() {
        let identity = Identity::generate().expect("generate identity");
        let client_cfg = tls_config::client_config(&identity).expect("client config");
        let upgrader = Upgrader::client(client_cfg);

        // `local` is the upgrader's stream; dropping `remote` closes the peer
        // end so the TLS ClientHello is met with EOF.
        let (local, remote) = tokio::io::duplex(1 << 16);
        drop(remote);

        let result = upgrader.upgrade(local).await;
        assert!(
            result.is_err(),
            "client upgrade over a peer that closes without a TLS response must \
             return Err (the arm handle_dial logs), got Ok"
        );
    }
}
