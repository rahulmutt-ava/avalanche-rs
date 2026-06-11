// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Transport-layer middleware for the node HTTP server (specs 12 §3.1,
//! 14 §1.3/§16.3).
//!
//! These mirror the Go `api/server` middleware one-for-one:
//! - [`allowed_hosts`] — `allowed_hosts.go::filterInvalidHosts`: reject a
//!   request whose `Host` header is not in `http-allowed-hosts` with `403`
//!   `"invalid host specified"`. A `*` entry accepts all; empty and bare-IP
//!   hosts are always accepted (DNS-rebinding defence only targets named hosts).
//! - [`node_id_header`] — set the `node-id` response header on **every**
//!   response, including error responses (`server.go`, 14 §16.3).
//! - [`not_bootstrapped`] — until a chain reaches [`EngineState::NormalOp`], its
//!   routes return `503 "API call rejected because chain is not done
//!   bootstrapping"` (`server.go:263`).

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use ava_snow::{ConsensusContext, EngineState};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The wildcard token that, when present in `http-allowed-hosts`, accepts every
/// host (Go `server.wildcard`).
pub const WILDCARD: &str = "*";

/// The `403` body emitted for a disallowed `Host` header (`allowed_hosts.go:75`).
pub const INVALID_HOST_MSG: &str = "invalid host specified";

/// The `503` body emitted while a chain has not reached `NormalOp`
/// (`server.go:263`).
pub const NOT_BOOTSTRAPPED_MSG: &str = "API call rejected because chain is not done bootstrapping";

/// The response header carrying this node's id, set on every response
/// (14 §1.3/§16.3).
pub const NODE_ID_HEADER: &str = "node-id";

/// The lowercased, parsed `http-allowed-hosts` policy.
///
/// Built once from the configured list; [`AllowedHosts::is_allowed`] mirrors
/// `allowedHostsHandler.ServeHTTP`.
#[derive(Clone, Debug)]
pub struct AllowedHosts {
    /// `true` when the configured list contained the `*` wildcard.
    wildcard: bool,
    /// The lowercased set of explicitly-allowed hostnames.
    hosts: Arc<Vec<String>>,
}

impl AllowedHosts {
    /// Builds the policy from the raw `http-allowed-hosts` list.
    ///
    /// A `*` entry sets the wildcard (all hosts accepted). Other entries are
    /// lowercased and matched exactly (Go `set.Add(strings.ToLower(host))`).
    #[must_use]
    pub fn new(allowed: &[String]) -> Self {
        let mut wildcard = false;
        let mut hosts = Vec::with_capacity(allowed.len());
        for host in allowed {
            if host == WILDCARD {
                wildcard = true;
            } else {
                hosts.push(host.to_lowercase());
            }
        }
        Self {
            wildcard,
            hosts: Arc::new(hosts),
        }
    }

    /// Returns whether a request carrying the given `Host` header value should
    /// be served, mirroring `allowedHostsHandler.ServeHTTP`.
    ///
    /// - An empty host is accepted (DNS-rebinding attacks rely on the header).
    /// - The port suffix is stripped (Go `net.SplitHostPort`); on failure the
    ///   whole value is used as the host.
    /// - A bare IP (v4/v6) is accepted unconditionally.
    /// - A named host is accepted only if the wildcard is set or it is in the
    ///   lowercased allow-list.
    #[must_use]
    pub fn is_allowed(&self, host: &str) -> bool {
        if host.is_empty() {
            return true;
        }
        if self.wildcard {
            return true;
        }

        let stripped = strip_port(host);

        if IpAddr::from_str(stripped).is_ok() {
            return true;
        }

        let lower = stripped.to_lowercase();
        self.hosts.contains(&lower)
    }
}

/// Strips the `:port` suffix from a `Host` header value, mirroring Go's
/// `net.SplitHostPort` fallback (on error the whole value is the host).
///
/// Handles `host:port`, bracketed IPv6 (`[::1]:port` / `[::1]`), and bare hosts.
fn strip_port(host: &str) -> &str {
    // Bracketed IPv6 literal, optionally with a port.
    if let Some(rest) = host.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return &rest[..end];
        }
        // Malformed bracket — Go's SplitHostPort errors and the raw value is
        // used; mirror that by returning the original host.
        return host;
    }

    // For a bare value, only treat a trailing `:port` as a port when there is
    // exactly one colon (more than one colon is an un-bracketed IPv6 literal,
    // which Go's SplitHostPort rejects → host = whole value).
    match host.rfind(':') {
        Some(idx) if host[..idx].find(':').is_none() => &host[..idx],
        _ => host,
    }
}

/// axum middleware: reject requests whose `Host` header is not allowed with a
/// `403 "invalid host specified"` (mirror `filterInvalidHosts`).
pub async fn allowed_hosts(
    State(policy): State<AllowedHosts>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if policy.is_allowed(host) {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, INVALID_HOST_MSG).into_response()
    }
}

/// axum middleware: set the `node-id` header on every response (14 §16.3).
///
/// The header value is precomputed once (the node id is immutable), so this is
/// an infallible header insert on both success and error responses.
pub async fn node_id_header(
    State(value): State<HeaderValue>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(NODE_ID_HEADER, value);
    response
}

/// axum middleware: reject requests to a chain that has not reached `NormalOp`
/// with `503 "API call rejected because chain is not done bootstrapping"`
/// (`server.go:263`).
///
/// The chain's [`ConsensusContext`] is consulted live on each request, so a
/// chain that finishes bootstrapping starts serving without re-registration.
pub async fn not_bootstrapped(
    State(ctx): State<Arc<ConsensusContext>>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(**ctx.state.load(), EngineState::NormalOp) {
        next.run(request).await
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, NOT_BOOTSTRAPPED_MSG).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ava_snow::{ChainContext, ConsensusContext, EngineState, NoOpAcceptor};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use pretty_assertions::assert_eq;
    use tower::ServiceExt;

    use super::*;

    fn hosts(list: &[&str]) -> AllowedHosts {
        AllowedHosts::new(&list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    fn ok_router_with_hosts(policy: AllowedHosts) -> Router {
        Router::new()
            .route("/ext/info", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(policy, allowed_hosts))
    }

    async fn body_string(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn request_with_host(router: Router, host: &str) -> Response {
        let mut builder = Request::builder().uri("/ext/info");
        if !host.is_empty() {
            builder = builder.header(axum::http::header::HOST, host);
        }
        let request = builder.body(Body::empty()).expect("build request");
        router.oneshot(request).await.expect("oneshot")
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): allowed-hosts filter — 403 "invalid host specified".
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn allowed_hosts_filter() {
        // Host not in `http-allowed-hosts` => 403 "invalid host specified".
        let router = ok_router_with_hosts(hosts(&["localhost"]));
        let response = request_with_host(router, "evil.example.com").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_string(response).await, INVALID_HOST_MSG);

        // An allow-listed host (case-insensitive) is accepted.
        let router = ok_router_with_hosts(hosts(&["localhost"]));
        let response = request_with_host(router, "LOCALHOST:9650").await;
        assert_eq!(response.status(), StatusCode::OK);

        // `*` wildcard accepts every host.
        let router = ok_router_with_hosts(hosts(&["*", "localhost"]));
        let response = request_with_host(router, "anything.invalid").await;
        assert_eq!(response.status(), StatusCode::OK);

        // Bare IPv4 is always accepted.
        let router = ok_router_with_hosts(hosts(&["localhost"]));
        let response = request_with_host(router, "127.0.0.1:9650").await;
        assert_eq!(response.status(), StatusCode::OK);

        // Bare IPv6 (bracketed) is always accepted.
        let router = ok_router_with_hosts(hosts(&["localhost"]));
        let response = request_with_host(router, "[::1]:9650").await;
        assert_eq!(response.status(), StatusCode::OK);

        // Empty host is always accepted (no Host header sent).
        let router = ok_router_with_hosts(hosts(&["localhost"]));
        let response = request_with_host(router, "").await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn allowed_hosts_policy_unit() {
        let policy = hosts(&["localhost", "Example.COM"]);
        assert!(policy.is_allowed("")); // empty accepted
        assert!(policy.is_allowed("localhost"));
        assert!(policy.is_allowed("localhost:9650"));
        assert!(policy.is_allowed("EXAMPLE.com")); // case-insensitive
        assert!(policy.is_allowed("10.0.0.1")); // bare IPv4
        assert!(policy.is_allowed("[fe80::1]:80")); // bracketed IPv6
        assert!(policy.is_allowed("::1")); // un-bracketed IPv6
        assert!(!policy.is_allowed("evil.com"));

        let wild = hosts(&["*"]);
        assert!(wild.is_allowed("anything"));
    }

    // ------------------------------------------------------------------
    // not_bootstrapped — 503 until NormalOp.
    // ------------------------------------------------------------------
    fn test_ctx() -> Arc<ConsensusContext> {
        let chain = Arc::new(ChainContext {
            network_id: 1,
            subnet_id: Id::EMPTY,
            chain_id: Id::EMPTY,
            node_id: NodeId::default(),
            public_key: None,
            network_upgrades: ava_version::upgrade::get_config(1),
            x_chain_id: Id::EMPTY,
            c_chain_id: Id::EMPTY,
            avax_asset_id: Id::EMPTY,
            chain_data_dir: std::path::PathBuf::new(),
        });
        Arc::new(ConsensusContext::new(
            chain,
            "C".to_string(),
            Arc::new(NoOpAcceptor),
            Arc::new(NoOpAcceptor),
        ))
    }

    #[tokio::test]
    async fn not_bootstrapped_503() {
        let ctx = test_ctx();
        let router = Router::new()
            .route("/ext/bc/C/rpc", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                ctx.clone(),
                not_bootstrapped,
            ));

        // Initializing => 503 with the exact Go message.
        let request = Request::builder()
            .uri("/ext/bc/C/rpc")
            .body(Body::empty())
            .expect("build request");
        let response = router.clone().oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_string(response).await, NOT_BOOTSTRAPPED_MSG);

        // After NormalOp => served.
        ctx.state.store(Arc::new(EngineState::NormalOp));
        let request = Request::builder()
            .uri("/ext/bc/C/rpc")
            .body(Body::empty())
            .expect("build request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
    }
}
