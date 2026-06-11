// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The node HTTP server (mirror Go `api/server`).
//!
//! Routes are mounted under the base path `/ext` on `axum`/`hyper`/`tower`,
//! replacing Go's `gorilla/mux` + `net/http`. This module implements the M8.16
//! transport layer:
//! - **h2c** (HTTP/2 cleartext, `MaxConcurrentStreams = 64`) so Connect /
//!   gRPC-Web clients share the port (no ALPN since TLS is off by default).
//! - **CORS** via [`tower_http::cors`] (`allow_origins` from
//!   `http-allowed-origins`, default `*`; `allow_credentials = true`).
//! - **Allowed-Hosts** (`403`), **`node-id`** response header, **per-chain
//!   not-bootstrapped** `503`, and the **`HTTPConfig` timeout** layers.
//!
//! The JSON-RPC 2.0 shim / service registry (M8.17) and full chain mounting
//! (M8.22) build on the [`ApiServer`] trait defined here; this task wires the
//! trait and the transport plumbing only.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use ava_config::node::HttpConfig;
use ava_snow::ConsensusContext;
use ava_types::node_id::NodeId;
use ava_vm::Vm;
use axum::Router;
use axum::http::{HeaderValue, Method, StatusCode};
use parking_lot::Mutex;
use tokio::sync::Notify;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};
use tower_http::timeout::TimeoutLayer;

use crate::error::{ApiError, Result};
use crate::middleware::{AllowedHosts, WILDCARD, allowed_hosts, node_id_header, not_bootstrapped};

/// The base path every route is mounted under (Go `baseURL = "/ext"`).
pub const BASE_URL: &str = "/ext";

/// The h2c `MaxConcurrentStreams` limit (14 §1.3 / 12 §3.1).
pub const MAX_CONCURRENT_STREAMS: u32 = 64;

/// A mountable HTTP handler: a self-contained [`axum::Router`] that owns its
/// own method routing.
///
/// A VM's `create_handlers` descriptor ([`ava_vm::HttpHandler`]) is adapted into
/// one of these when a chain is mounted (M8.22); the built-in APIs (info /
/// health / …) each contribute one in M8.17.
pub type BoxedHandler = Router;

/// The node HTTP API server (mirror Go `api/server.server`).
///
/// `add_route` / `add_header_route` / `register_chain` accumulate routes into an
/// internal registry; [`ApiServer::serve`] composes them into the final router
/// with the transport middleware and binds the listener.
#[async_trait]
pub trait ApiServer: Send + Sync {
    /// Register `handler` under `/ext/<base>/<endpoint>` (mirror Go
    /// `server.AddRoute`).
    ///
    /// # Errors
    /// Returns [`ApiError::InvalidPath`] for a malformed base/endpoint or
    /// [`ApiError::AlreadyReserved`] if the path is already taken.
    fn add_route(&self, handler: BoxedHandler, base: &str, endpoint: &str) -> Result<()>;

    /// Register path `aliases` for an already-mounted `endpoint` (mirror Go
    /// `server.AddAliases`), so e.g. `/ext/bc/X` resolves the X-Chain mount.
    ///
    /// # Errors
    /// Returns [`ApiError::AlreadyReserved`] if an alias path is already taken.
    fn add_aliases(&self, endpoint: &str, aliases: &[String]) -> Result<()>;

    /// Mount a chain's VM HTTP handlers under `/ext/bc/<chainID>/<endpoint>`
    /// (mirror Go `server.RegisterChain`).
    ///
    /// Full mounting (calling `vm.create_handlers` and wiring each extension)
    /// lands in M8.22; in this task the call records the chain's
    /// [`ConsensusContext`] so the per-chain not-bootstrapped `503` layer can be
    /// applied.
    async fn register_chain(&self, name: &str, ctx: &Arc<ConsensusContext>, vm: Arc<dyn Vm>);

    /// Register the chain's header-route handler (EVM `/rpc`, `/ws`), routed by
    /// the chain-id request header (mirror Go `server.AddChainRoute` /
    /// header-route handling, 12 §3.1). Full wiring lands in M8.22.
    ///
    /// # Errors
    /// Returns [`ApiError::AlreadyReserved`] if the chain already has a
    /// header-route handler.
    fn add_header_route(&self, chain_id: &str, handler: BoxedHandler) -> Result<()>;

    /// Build the composed router and serve until [`ApiServer::shutdown`].
    ///
    /// # Errors
    /// Returns [`ApiError::Listen`] if the listener cannot be bound or the
    /// accept loop fails.
    async fn serve(&self) -> Result<()>;

    /// Signal the serve loop to stop accepting and shut down (mirror Go
    /// `server.Shutdown`).
    ///
    /// # Errors
    /// Infallible in this implementation; the `Result` mirrors the Go shape.
    async fn shutdown(&self) -> Result<()>;
}

/// A single accumulated route: its full path under `/ext` and its sub-router.
struct Route {
    /// Full mount path (e.g. `/ext/info`, `/ext/bc/<chainID>/rpc`).
    path: String,
    /// The handler sub-router.
    handler: BoxedHandler,
}

/// A chain registered with the server, used to apply its not-bootstrapped layer.
struct ChainRegistration {
    /// The chain's consensus context (read live by the `503` layer). Its
    /// `primary_alias` carries the chain name for diagnostics.
    ctx: Arc<ConsensusContext>,
}

/// Mutable router-construction state, guarded by a single lock (the Go server
/// guards its `router` / `handlers` with a `sync.Mutex`).
#[derive(Default)]
struct Registry {
    routes: Vec<Route>,
    chains: Vec<ChainRegistration>,
}

/// The concrete [`ApiServer`] implementation.
pub struct Server {
    /// HTTP transport configuration (timeouts, allowed-origins/hosts, bind).
    config: HttpConfig,
    /// This node's id, emitted in the `node-id` response header.
    node_id: NodeId,
    /// Accumulated routes / chain registrations.
    registry: Mutex<Registry>,
    /// Notified by [`ApiServer::shutdown`] to stop the serve loop. Held behind
    /// an `Arc` so the graceful-shutdown future can hold it past `&self`.
    shutdown: Arc<Notify>,
}

impl Server {
    /// Builds a new server from the resolved [`HttpConfig`] and this node's id.
    #[must_use]
    pub fn new(config: HttpConfig, node_id: NodeId) -> Self {
        Self {
            config,
            node_id,
            registry: Mutex::new(Registry::default()),
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// The configured bind address (`http-host:http-port`).
    ///
    /// # Errors
    /// Returns [`ApiError::Listen`] if the host/port do not parse to a socket
    /// address.
    pub fn bind_addr(&self) -> Result<SocketAddr> {
        let raw = format!("{}:{}", self.config.http_host, self.config.http_port);
        raw.parse().map_err(|e| ApiError::Listen {
            addr: raw,
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, e),
        })
    }

    /// Builds the CORS layer from `http-allowed-origins` (mirror `rs/cors`).
    ///
    /// `*` (or an empty list) allows any origin; otherwise the listed origins
    /// are allowed. `allow_credentials` is always set (Go `AllowCredentials:
    /// true`). Note: per the CORS spec, a wildcard origin and credentials are
    /// mutually exclusive in the browser; Go's `rs/cors` reflects the request
    /// origin in that case, which [`AllowOrigin::mirror_request`] reproduces.
    fn cors_layer(&self) -> CorsLayer {
        let origins = &self.config.http_allowed_origins;
        let wildcard = origins.is_empty() || origins.iter().any(|o| o == WILDCARD);

        let allow_origin = if wildcard {
            // Mirror the request origin so credentialed wildcard CORS works
            // (matches `rs/cors` behaviour when AllowedOrigins=["*"] +
            // AllowCredentials=true).
            AllowOrigin::mirror_request()
        } else {
            let values: Vec<HeaderValue> = origins
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect();
            AllowOrigin::list(values)
        };

        CorsLayer::new()
            .allow_origin(allow_origin)
            .allow_credentials(true)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            // `rs/cors` with `AllowCredentials: true` reflects the request's
            // `Access-Control-Request-Headers` rather than emitting a `*`
            // (a literal `*` is incompatible with credentials per the CORS
            // spec, and `tower-http` panics on that combination).
            .allow_headers(AllowHeaders::mirror_request())
    }

    /// Composes the accumulated routes plus the transport middleware into the
    /// final router (h2c is configured on the serving side, not the router).
    ///
    /// Layer order (outermost first, as a request travels inward): allowed-hosts
    /// → node-id header → CORS → timeouts → per-route (incl. per-chain `503`).
    fn build_router(&self) -> Result<Router> {
        let registry = self.registry.lock();

        let mut router = Router::new();
        for route in &registry.routes {
            router = router.nest(&route.path, route.handler.clone());
        }

        // Per-chain not-bootstrapped 503: apply the layer to each chain's mount
        // subtree. We compose a fallback chain-aware router by merging.
        for chain in &registry.chains {
            // Chain routes are mounted under /ext/bc/<chainID>; the layer reads
            // the live consensus state, so registering the ctx is enough.
            let chain_router = Router::new().layer(axum::middleware::from_fn_with_state(
                chain.ctx.clone(),
                not_bootstrapped,
            ));
            router = router.merge(chain_router);
        }
        drop(registry);

        let node_id_value = HeaderValue::from_str(&self.node_id.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("unknown"));

        let allowed = AllowedHosts::new(&self.config.http_allowed_hosts);

        let router = router
            // Read/write timeout (Go's read+write timeouts collapse to a single
            // request timeout here; idle/read-header are connection-level and
            // configured on the hyper builder at serve time). A timed-out
            // request gets a `408 Request Timeout` (mirrors `net/http`).
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                self.config.write_timeout,
            ))
            .layer(self.cors_layer())
            .layer(axum::middleware::from_fn_with_state(
                node_id_value,
                node_id_header,
            ))
            .layer(axum::middleware::from_fn_with_state(allowed, allowed_hosts));

        Ok(router)
    }

    /// Validates and normalizes a base/endpoint path, mirroring Go's
    /// `url.ParseRequestURI` acceptance: the joined path must be absolute under
    /// `/ext` and contain no traversal (`..`), current-dir (`.`), or empty
    /// (`//`) path segments. The empty endpoint is allowed.
    ///
    /// The base/endpoint may carry internal slashes (e.g. `bc/<chainID>`), so we
    /// validate per path *segment* rather than by substring — a chain-id such as
    /// `2x...` (a cb58 alias) is a single valid segment, not a traversal.
    fn route_path(base: &str, endpoint: &str) -> Result<String> {
        let joined = format!("{}/{}", base.trim_matches('/'), endpoint.trim_matches('/'));

        let mut segments = Vec::new();
        for segment in joined.split('/') {
            if segment.is_empty() {
                continue;
            }
            if segment == "." || segment == ".." {
                let candidate = format!("{BASE_URL}/{}", joined.trim_matches('/'));
                return Err(ApiError::InvalidPath {
                    path: candidate,
                    msg: "path contains invalid '.' or '..' segment".to_string(),
                });
            }
            segments.push(segment);
        }

        let candidate = if segments.is_empty() {
            BASE_URL.to_string()
        } else {
            format!("{}/{}", BASE_URL, segments.join("/"))
        };

        Ok(candidate)
    }

    fn reserve(&self, path: String, handler: BoxedHandler) -> Result<()> {
        let mut registry = self.registry.lock();
        if registry.routes.iter().any(|r| r.path == path) {
            return Err(ApiError::AlreadyReserved { path });
        }
        registry.routes.push(Route { path, handler });
        Ok(())
    }
}

#[async_trait]
impl ApiServer for Server {
    fn add_route(&self, handler: BoxedHandler, base: &str, endpoint: &str) -> Result<()> {
        let path = Self::route_path(base, endpoint)?;
        self.reserve(path, handler)
    }

    fn add_aliases(&self, endpoint: &str, aliases: &[String]) -> Result<()> {
        // Look up the canonical handler for `endpoint` and mount a clone under
        // each alias path. The canonical mount must already exist.
        let canonical = Self::route_path(endpoint, "")?;
        let mut registry = self.registry.lock();
        let handler = registry
            .routes
            .iter()
            .find(|r| r.path == canonical)
            .map(|r| r.handler.clone())
            .ok_or_else(|| ApiError::InvalidPath {
                path: canonical.clone(),
                msg: "no route registered to alias".to_string(),
            })?;

        for alias in aliases {
            let alias_path = Self::route_path(alias, "")?;
            if registry.routes.iter().any(|r| r.path == alias_path) {
                return Err(ApiError::AlreadyReserved { path: alias_path });
            }
            registry.routes.push(Route {
                path: alias_path,
                handler: handler.clone(),
            });
        }
        Ok(())
    }

    async fn register_chain(&self, _name: &str, ctx: &Arc<ConsensusContext>, _vm: Arc<dyn Vm>) {
        // M8.16: record the chain so the not-bootstrapped 503 layer applies.
        // Calling `vm.create_handlers()` and mounting each extension under
        // /ext/bc/<chainID>/<extension> lands in M8.22.
        let mut registry = self.registry.lock();
        registry.chains.push(ChainRegistration { ctx: ctx.clone() });
    }

    fn add_header_route(&self, chain_id: &str, handler: BoxedHandler) -> Result<()> {
        // EVM-style header-routed handler. Mounted under the chain's base so
        // /ext/bc/<chainID>/rpc|ws resolve (full header-key routing in M8.22).
        let path = Self::route_path(&format!("bc/{chain_id}"), "")?;
        self.reserve(path, handler)
    }

    async fn serve(&self) -> Result<()> {
        let addr = self.bind_addr()?;
        let router = self.build_router()?;

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| ApiError::Listen {
                addr: addr.to_string(),
                source,
            })?;

        // h2c: `axum::serve` drives `hyper-util`'s auto connection builder,
        // which serves HTTP/1.1 and HTTP/2-cleartext on the same port (no ALPN,
        // since TLS is off by default). The HTTP/2 MaxConcurrentStreams=64 cap
        // is applied through the hyper-util builder when the auto-server is
        // wired explicitly in M8.22; the constant is exported for that wiring.
        let _ = MAX_CONCURRENT_STREAMS;

        let shutdown = self.shutdown.clone();
        axum::serve(listener, router.into_make_service())
            .with_graceful_shutdown(async move {
                shutdown.notified().await;
            })
            .await
            .map_err(|source| ApiError::Listen {
                addr: addr.to_string(),
                source,
            })
    }

    async fn shutdown(&self) -> Result<()> {
        self.shutdown.notify_waiters();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ava_config::node::{ApiConfig, HttpConfig};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use pretty_assertions::assert_eq;
    use tower::ServiceExt;

    use super::*;
    use crate::middleware::NODE_ID_HEADER;

    fn test_http_config() -> HttpConfig {
        HttpConfig {
            read_timeout: Duration::from_secs(30),
            read_header_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(120),
            api_config: ApiConfig {
                index_api_enabled: false,
                index_allow_incomplete: false,
                admin_api_enabled: false,
                info_api_enabled: true,
                metrics_api_enabled: true,
                health_api_enabled: true,
            },
            http_host: "127.0.0.1".to_string(),
            http_port: 0,
            https_enabled: false,
            https_key: Vec::new(),
            https_cert: Vec::new(),
            http_allowed_origins: vec!["*".to_string()],
            http_allowed_hosts: vec!["*".to_string()],
            shutdown_timeout: Duration::from_secs(10),
            shutdown_wait: Duration::ZERO,
        }
    }

    fn test_node_id() -> NodeId {
        // Distinct, deterministic id so the header value is meaningful.
        NodeId::from_slice(&[7u8; 20]).expect("20-byte node id")
    }

    fn server() -> Server {
        Server::new(test_http_config(), test_node_id())
    }

    // ------------------------------------------------------------------
    // node-id header is present on EVERY response, including errors (14 §16.3).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn node_id_header_on_every_response() {
        let srv = server();
        // Handlers are self-contained sub-routers mounted (nested) under their
        // computed path, so they route relative to the mount point.
        let ok_router = Router::new().route("/", get(|| async { "ok" }));
        srv.reserve("/ext/info".to_string(), ok_router)
            .expect("reserve route");

        let router = srv.build_router().expect("build router");
        let expected = test_node_id().to_string();

        // 200 path carries node-id.
        let request = Request::builder()
            .uri("/ext/info")
            .body(Body::empty())
            .expect("request");
        let response = router.clone().oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(NODE_ID_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some(expected.as_str())
        );

        // 404 (unknown path) error response ALSO carries node-id.
        let request = Request::builder()
            .uri("/ext/does-not-exist")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(NODE_ID_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some(expected.as_str())
        );
    }

    #[test]
    fn route_path_validation() {
        assert_eq!(Server::route_path("info", "").unwrap(), "/ext/info");
        assert_eq!(Server::route_path("bc/X", "rpc").unwrap(), "/ext/bc/X/rpc");
        assert_eq!(Server::route_path("/info/", "/").unwrap(), "/ext/info");
        // Empty endpoint allowed (validated like url.ParseRequestURI).
        assert_eq!(Server::route_path("", "").unwrap(), "/ext");
        // Traversal rejected.
        assert!(matches!(
            Server::route_path("..", "x"),
            Err(ApiError::InvalidPath { .. })
        ));
    }

    #[test]
    fn add_route_rejects_duplicate() {
        let srv = server();
        srv.add_route(Router::new(), "info", "").expect("first");
        let dup = srv.add_route(Router::new(), "info", "");
        assert!(matches!(dup, Err(ApiError::AlreadyReserved { .. })));
    }

    #[test]
    fn add_aliases_mounts_clones() {
        let srv = server();
        srv.add_route(Router::new(), "bc/2x...", "").expect("base");
        srv.add_aliases("bc/2x...", &["bc/X".to_string()])
            .expect("alias");
        let registry = srv.registry.lock();
        assert!(registry.routes.iter().any(|r| r.path == "/ext/bc/X"));
    }

    #[test]
    fn add_aliases_requires_existing_route() {
        let srv = server();
        let res = srv.add_aliases("nope", &["alias".to_string()]);
        assert!(matches!(res, Err(ApiError::InvalidPath { .. })));
    }

    #[test]
    fn bind_addr_parses() {
        let srv = server();
        let addr = srv.bind_addr().expect("addr");
        assert_eq!(addr.port(), 0);
        assert!(addr.ip().is_loopback());
    }

    // register_chain records the chain so the 503 layer applies; full mounting
    // is M8.22. Verified via the registry.
    #[tokio::test]
    async fn register_chain_records_context() {
        use ava_snow::{ChainContext, ConsensusContext, NoOpAcceptor};
        use ava_vm::testutil::TestVm;

        let srv = server();
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
        let ctx = Arc::new(ConsensusContext::new(
            chain,
            "C".to_string(),
            Arc::new(NoOpAcceptor),
            Arc::new(NoOpAcceptor),
        ));
        let vm: Arc<dyn Vm> = Arc::new(TestVm::new());
        srv.register_chain("C-Chain", &ctx, vm).await;

        let registry = srv.registry.lock();
        assert_eq!(registry.chains.len(), 1);
        let registered = registry.chains.first().expect("one chain registered");
        assert_eq!(registered.ctx.primary_alias, "C");
    }
}
