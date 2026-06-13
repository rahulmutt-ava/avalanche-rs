// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 9 (specs/12 §2.2): the HTTP API server (mirror Go
//! `initAPIServer`).

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use ava_api::server::Server;
use ava_config::node::Config;
use ava_types::node_id::NodeId;
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::init::nat::Nat;

/// The name Go maps the HTTP port under (`constants.AppName + "-http"`).
const HTTP_PORT_NAME: &str = "avalanchego-http";

/// Whether `ip` is publicly routable (mirror Go `ips.IsPublic`): loopback,
/// RFC-1918 / unique-local, and link-local addresses are private.
pub(crate) fn ip_is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast())
        }
        IpAddr::V6(v6) => {
            // Unique-local fc00::/7 and link-local fe80::/10 are private.
            let seg0 = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00
                || (seg0 & 0xffc0) == 0xfe80)
        }
    }
}

/// Step 9: build the API server (mirror Go `initAPIServer`).
///
/// Go binds the HTTP listener here; the Rust [`Server`] binds inside
/// [`ava_api::server::ApiServer::serve`] (M8.30 dispatch), so this step
/// resolves the host's publicness, port-maps the HTTP port when the host is
/// public, and constructs the server + its `avalanche_api` metrics namespace.
/// The returned URI uses the *configured* port; a `--http-port=0` URI is
/// re-resolved once the listener binds (M8.30, `tests/PORTING.md`).
///
/// # Errors
/// - [`crate::error::Error::Io`] when a non-empty `--http-host` fails DNS
///   resolution (Go logs fatal and returns the error).
/// - Metrics-namespace registration failures.
pub async fn init_api_server(
    config: &Config,
    node_id: NodeId,
    metrics: &super::metrics::NodeMetrics,
    nat: &Nat,
    token: &CancellationToken,
) -> Result<(Arc<Server>, String)> {
    tracing::info!("initializing API server");

    let http = &config.http_config;

    // An empty host is a wildcard match-all, considered public.
    let host_is_public = if http.http_host.is_empty() {
        true
    } else {
        // `ips.Lookup` is a blocking DNS resolution; run it off the reactor.
        let host = http.http_host.clone();
        let resolved: Option<IpAddr> = tokio::task::spawn_blocking(move || {
            (host.as_str(), 0u16)
                .to_socket_addrs()
                .ok()
                .and_then(|mut addrs| addrs.next())
                .map(|a| a.ip())
        })
        .await?;
        let Some(ip) = resolved else {
            tracing::error!(host = %http.http_host, "failed to lookup HTTP host");
            return Err(crate::error::Error::Networking(format!(
                "failed to lookup HTTP host: {}",
                http.http_host
            )));
        };
        let public = ip_is_public(ip);
        tracing::debug!(host = %http.http_host, %ip, is_public = public, "finished HTTP host lookup");
        public
    };

    if host_is_public {
        tracing::warn!(
            host = %http.http_host,
            "HTTP server is binding to a potentially public host. You may be vulnerable to a \
             DoS attack if your HTTP port is publicly accessible"
        );
        // Map the configured port (Go maps the *bound* port; binding happens
        // at serve time in Rust — with `--http-port=0` the mapping is skipped,
        // M8.30 re-maps the resolved port).
        if http.http_port != 0 {
            let _handle = nat.mapper.start(
                http.http_port,
                http.http_port,
                HTTP_PORT_NAME,
                token.child_token(),
            );
        }
    }

    let protocol = if http.https_enabled { "https" } else { "http" };
    let api_uri = format!("{protocol}://{}:{}", http.http_host, http.http_port);

    // Go threads an `avalanche_api` registerer into `server.New`; the Rust
    // server keeps its transport metrics deferral (M8.16) — the namespace is
    // registered for layout parity.
    let _api_registry =
        ava_api::metrics::make_and_register(metrics.gatherer.as_ref(), &super::namespace::api())?;

    let server = Arc::new(Server::new(http.clone(), node_id));
    Ok((server, api_uri))
}

/// Resolve a `host:port` pair into the advertised socket address (used for the
/// node's `my_ip` placeholder before networking resolves the public IP).
#[must_use]
pub fn socket_addr_or_unspecified(host: &str, port: u16) -> SocketAddr {
    format!("{host}:{port}")
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], port)))
}
