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
use crate::header_route::{HeaderRoutes, header_route};
use crate::middleware::{AllowedHosts, WILDCARD, allowed_hosts, node_id_header, not_bootstrapped};

/// The base path every route is mounted under (Go `baseURL = "/ext"`).
pub const BASE_URL: &str = "/ext";

/// The h2c `MaxConcurrentStreams` limit (14 §1.3 / 12 §3.1), applied to the
/// HTTP/2 side of the connection builder in [`ApiServer::serve`].
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
    /// (mirror Go `server.RegisterChain`, `api/server/server.go:153`; full
    /// contract in [`crate::register`]). The VM sits behind a `tokio` mutex
    /// because `Vm::create_handlers`/`new_http_handler` take `&mut self` (the
    /// Rust shape of Go's `ctx.Lock` around the calls, `server.go:154`); the
    /// chain manager holds VMs the same way.
    async fn register_chain(
        &self,
        name: &str,
        ctx: &Arc<ConsensusContext>,
        vm: Arc<tokio::sync::Mutex<dyn Vm>>,
    );

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
    /// The chain's mount prefix under `/ext` (e.g. `/ext/bc/<chainID>`). Every
    /// route mounted at or beneath this prefix is wrapped with this chain's
    /// not-bootstrapped `503` layer.
    prefix: String,
    /// The chain's consensus context (read live by the `503` layer). Its
    /// `primary_alias` carries the chain name for diagnostics.
    ctx: Arc<ConsensusContext>,
}

/// A recorded alias mapping (mirror Go `router.aliases`): a later registration
/// of `canonical` propagates the handler to `alias`.
struct AliasMapping {
    /// The canonical mount path the alias mirrors (e.g. `/ext/bc/<chainID>`).
    canonical: String,
    /// The alias mount path (e.g. `/ext/bc/X`).
    alias: String,
}

/// Mutable router-construction state, guarded by a single lock (the Go server
/// guards its `router` / `handlers` with a `sync.Mutex`).
#[derive(Default)]
struct Registry {
    routes: Vec<Route>,
    chains: Vec<ChainRegistration>,
    /// Recorded alias mappings whose canonical route may or may not exist yet.
    /// Mirrors Go's `router.aliases`; a later canonical registration propagates
    /// to every recorded alias.
    aliases: Vec<AliasMapping>,
    /// Reserved alias names (mirror Go `router.reservedRoutes`): an alias name,
    /// once reserved, cannot be reserved again or used as a canonical route.
    reserved_aliases: Vec<String>,
}

/// The concrete [`ApiServer`] implementation.
pub struct Server {
    /// HTTP transport configuration (timeouts, allowed-origins/hosts, bind).
    config: HttpConfig,
    /// The precomputed `node-id` header value (the node id is immutable and its
    /// string form is always ASCII, so this never fails after construction).
    node_id_value: HeaderValue,
    /// Accumulated routes / chain registrations.
    registry: Mutex<Registry>,
    /// The header-route table (Go `router.headerRoutes`), dispatched by the
    /// `Avalanche-Api-Route` header before path routing (M8.22; 14 §1.2).
    header_routes: HeaderRoutes,
    /// Notified by [`ApiServer::shutdown`] to stop the serve loop. Held behind
    /// an `Arc` so the graceful-shutdown future can hold it past `&self`.
    shutdown: Arc<Notify>,
}

impl Server {
    /// Builds a new server from the resolved [`HttpConfig`] and this node's id.
    #[must_use]
    pub fn new(config: HttpConfig, node_id: NodeId) -> Self {
        // The node id's string form is always ASCII (cb58 / `NodeID-…`), so this
        // conversion never fails; the `unknown` fallback is unreachable defence.
        let node_id_value = HeaderValue::from_str(&node_id.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("unknown"));
        Self {
            config,
            node_id_value,
            registry: Mutex::new(Registry::default()),
            header_routes: HeaderRoutes::new(),
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
    /// Layer order (outermost first, as a request travels inward): node-id
    /// header → allowed-hosts → CORS → timeouts → per-route (incl. per-chain
    /// `503`). The node-id header is outermost so it is set on **every**
    /// response, including the allowed-hosts `403` short-circuit (mirror Go,
    /// where the node-id wrapper is the outermost handler; 14 §16.3).
    pub(crate) fn build_router(&self) -> Result<Router> {
        let registry = self.registry.lock();

        let mut router = Router::new();
        for route in &registry.routes {
            // Per-chain not-bootstrapped 503: if this route is mounted at or
            // beneath a registered chain's prefix, wrap its sub-router with that
            // chain's `503` layer. The layer reads the live consensus state, so
            // a chain that finishes bootstrapping starts serving without any
            // re-registration. (axum's `.layer()` only wraps routes already
            // present on the router it is called on — here, the chain's own
            // sub-router — so the guard actually applies, unlike a `.layer()` on
            // an empty merged router.)
            let mut handler = route.handler.clone();
            // Resolve an alias-mounted path back to its canonical form so the
            // chain's `503` layer also guards `/ext/bc/P/...` (Go wraps the
            // handler itself before the alias fan-out — `wrapMiddleware`,
            // server.go:226 — so alias copies carry the reject layer too).
            let canonical_path = registry
                .aliases
                .iter()
                .find_map(|a| alias_route_path(&a.alias, &a.canonical, &route.path))
                .unwrap_or_else(|| route.path.clone());
            if let Some(chain) = registry.chains.iter().find(|c| {
                canonical_path == c.prefix || canonical_path.starts_with(&format!("{}/", c.prefix))
            }) {
                handler = handler.layer(axum::middleware::from_fn_with_state(
                    chain.ctx.clone(),
                    not_bootstrapped,
                ));
            }
            router = router.nest(&route.path, handler);
        }
        drop(registry);

        let allowed = AllowedHosts::new(&self.config.http_allowed_hosts);

        let router = router
            // Explicit 404 fallback: axum only runs `.layer()` middleware on
            // requests that match a route (or an explicit fallback added
            // before the layer), but the header-route dispatch below must see
            // EVERY request — Go checks the header before path routing
            // (`router.ServeHTTP`, router.go:53-58).
            .fallback(|| async { StatusCode::NOT_FOUND })
            // Header-based VM routing runs BEFORE path routing (Go
            // `router.ServeHTTP`, router.go:53), but inside the transport
            // wrappers (Go `wrapHandler` wraps the router). Innermost layer.
            .layer(axum::middleware::from_fn_with_state(
                self.header_routes.clone(),
                header_route,
            ))
            // Read/write timeout (Go's read+write timeouts collapse to a single
            // request timeout here; idle/read-header are connection-level and
            // configured on the hyper builder at serve time). A timed-out
            // request gets a `408 Request Timeout` (mirrors `net/http`).
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                self.config.write_timeout,
            ))
            .layer(self.cors_layer())
            .layer(axum::middleware::from_fn_with_state(allowed, allowed_hosts))
            // node-id header is applied LAST so it is the OUTERMOST layer (axum
            // applies layers inside-out): it wraps the allowed-hosts `403` too,
            // guaranteeing the header on every response.
            .layer(axum::middleware::from_fn_with_state(
                self.node_id_value.clone(),
                node_id_header,
            ));

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

    /// The live header-route table (used by `register_chain`, M8.22).
    pub(crate) fn header_routes(&self) -> &HeaderRoutes {
        &self.header_routes
    }

    /// Records a chain registration (its mount prefix + ctx) so `build_router`
    /// wraps every route at/beneath `/ext/bc/<chainID>` with the chain's
    /// not-bootstrapped `503` layer.
    pub(crate) fn record_chain(&self, ctx: &Arc<ConsensusContext>) {
        let prefix = Self::route_path(&format!("bc/{}", ctx.chain.chain_id), "")
            .unwrap_or_else(|_| BASE_URL.to_string());
        let mut registry = self.registry.lock();
        registry.chains.push(ChainRegistration {
            prefix,
            ctx: ctx.clone(),
        });
    }

    fn reserve(&self, path: String, handler: BoxedHandler) -> Result<()> {
        let mut registry = self.registry.lock();
        Self::reserve_locked(&mut registry, path, handler)
    }

    /// Records `path -> handler`, rejecting a path already taken by a route or
    /// reserved as an alias name (mirror Go `router.addRouter`: it errors if
    /// `reservedRoutes.Contains(base)`), then propagates the handler to every
    /// alias base at or above this path (mirror `forceAddRouter`'s alias
    /// fan-out, `router.go:142-149`: EVERY endpoint registered under an
    /// aliased base — current and future — is mirrored under each alias).
    fn reserve_locked(registry: &mut Registry, path: String, handler: BoxedHandler) -> Result<()> {
        if registry.routes.iter().any(|r| r.path == path)
            || registry.reserved_aliases.contains(&path)
        {
            return Err(ApiError::AlreadyReserved { path });
        }

        // Propagate to every alias whose canonical base covers this path (the
        // alias base names were reserved up-front by `add_aliases`).
        let alias_paths: Vec<String> = registry
            .aliases
            .iter()
            .filter_map(|a| alias_route_path(&a.canonical, &a.alias, &path))
            .collect();
        for alias in alias_paths {
            // A directly-registered route may already occupy the alias path;
            // keep it (Go's `forceAddRouter` reports the collision for the
            // alias copy only and keeps going, router.go:128).
            if registry.routes.iter().any(|r| r.path == alias) {
                continue;
            }
            registry.routes.push(Route {
                path: alias,
                handler: handler.clone(),
            });
        }

        registry.routes.push(Route { path, handler });
        Ok(())
    }
}

/// The alias-mounted copy of `path` for the mapping `canonical -> alias`:
/// `Some(alias + suffix)` when `path` is at or beneath the `canonical` base,
/// else `None`. (Go keys routes by `(base, endpoint)` so the alias fan-out is
/// a map walk, router.go:142/171; here routes carry full paths, so the
/// endpoint suffix is recomputed by prefix-stripping.)
fn alias_route_path(canonical: &str, alias: &str, path: &str) -> Option<String> {
    let suffix = path.strip_prefix(canonical)?;
    (suffix.is_empty() || suffix.starts_with('/')).then(|| format!("{alias}{suffix}"))
}

#[async_trait]
impl ApiServer for Server {
    fn add_route(&self, handler: BoxedHandler, base: &str, endpoint: &str) -> Result<()> {
        let path = Self::route_path(base, endpoint)?;
        self.reserve(path, handler)
    }

    fn add_aliases(&self, endpoint: &str, aliases: &[String]) -> Result<()> {
        // Mirror Go `router.AddAlias`: reserve each alias name, record the
        // canonical->alias mapping, and — if the canonical route already exists
        // — propagate its handler immediately. If the canonical route does NOT
        // exist yet, the mapping is still recorded so a LATER `add_route` /
        // `add_header_route` propagates to the alias (Go does not require the
        // canonical route to pre-exist).
        let canonical = Self::route_path(endpoint, "")?;
        let alias_paths: Vec<String> = aliases
            .iter()
            .map(|a| Self::route_path(a, ""))
            .collect::<Result<_>>()?;

        let mut registry = self.registry.lock();

        // Reject any alias name already reserved or already taken by a route
        // (mirror Go's `reservedRoutes.Contains(alias)` check). Done up-front so
        // a duplicate in the batch reserves nothing (matches Go's two-pass loop).
        for alias_path in &alias_paths {
            if registry.reserved_aliases.contains(alias_path)
                || registry.routes.iter().any(|r| r.path == *alias_path)
            {
                return Err(ApiError::AlreadyReserved {
                    path: alias_path.clone(),
                });
            }
        }

        // Reserve the names and record the mappings.
        for alias_path in &alias_paths {
            registry.reserved_aliases.push(alias_path.clone());
            registry.aliases.push(AliasMapping {
                canonical: canonical.clone(),
                alias: alias_path.clone(),
            });
        }

        // Propagate every endpoint already registered at or beneath the
        // canonical base (Go `AddAlias` force-adds each `routes[base]` endpoint
        // under every alias, router.go:171-178; a collision keeps the
        // directly-registered route, like `reserve_locked`'s fan-out).
        let existing: Vec<(String, BoxedHandler)> = registry
            .routes
            .iter()
            .filter(|r| alias_route_path(&canonical, "", &r.path).is_some())
            .map(|r| (r.path.clone(), r.handler.clone()))
            .collect();
        for alias_path in &alias_paths {
            for (path, handler) in &existing {
                let Some(copy) = alias_route_path(&canonical, alias_path, path) else {
                    continue;
                };
                if registry.routes.iter().any(|r| r.path == copy) {
                    continue;
                }
                registry.routes.push(Route {
                    path: copy,
                    handler: handler.clone(),
                });
            }
        }
        Ok(())
    }

    async fn register_chain(
        &self,
        name: &str,
        ctx: &Arc<ConsensusContext>,
        vm: Arc<tokio::sync::Mutex<dyn Vm>>,
    ) {
        // Full mounting contract (M8.22): see `crate::register`.
        self.register_chain_impl(name, ctx, &vm).await;
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

        // We drive `hyper-util`'s auto connection builder from an explicit accept
        // loop (instead of `axum::serve`) so we can apply the connection-level
        // HTTP settings the spec requires (12 §3.1), none of which `axum::serve`
        // exposes:
        //   * `.http2().max_concurrent_streams(64)` — the documented h2c
        //     MaxConcurrentStreams cap. The builder serves HTTP/1.1 and
        //     HTTP/2-cleartext on the same port (no ALPN, TLS off by default),
        //     so h2c prior-knowledge clients keep working under the cap.
        //   * `.http1().header_read_timeout(read_header_timeout)` — Go's
        //     `ReadHeaderTimeout`. Requires a `Timer`, so we install `TokioTimer`.
        // The remaining Go `http.Server` timeouts map as follows:
        //   * `WriteTimeout` -> the request-level `TimeoutLayer` in `build_router`.
        //   * `ReadTimeout` -> NOT wired. Go's `ReadTimeout` bounds the time to
        //     read the entire request (headers + body); hyper-util exposes no
        //     equivalent whole-request read deadline, only the header-read
        //     timeout above. A request-level layer cannot faithfully reproduce it
        //     (the body is streamed lazily by handlers, not read up-front), so we
        //     deliberately leave it unwired rather than approximate it.
        //   * `IdleTimeout` -> NOT wired. Go's `IdleTimeout` closes idle
        //     keep-alive connections; hyper-util's only related knobs are the
        //     HTTP/2 keep-alive PING interval/timeout (a liveness probe, not an
        //     idle-close deadline) and there is no HTTP/1 idle-close timer. Since
        //     neither faithfully reproduces "close after N seconds idle", it is
        //     left unwired (see report).
        let mut builder =
            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
        builder
            .http1()
            .timer(hyper_util::rt::TokioTimer::new())
            .header_read_timeout(self.config.read_header_timeout);
        builder
            .http2()
            .max_concurrent_streams(MAX_CONCURRENT_STREAMS);
        let builder = Arc::new(builder);

        // Graceful shutdown: a single task awaits the `Notify` and then drops the
        // `signal_rx`, which broadcasts to the serve loop and every connection
        // task via `signal_tx.closed()`. `close_tx`/`close_rx` track in-flight
        // connection tasks so we can drain them before returning. (This mirrors
        // `axum::serve`'s watch-channel pattern, which we cannot reuse directly
        // since it does not expose the connection builder.)
        let (signal_tx, signal_rx) = tokio::sync::watch::channel(());
        let signal_tx = Arc::new(signal_tx);
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            shutdown.notified().await;
            drop(signal_rx);
        });
        let (close_tx, close_rx) = tokio::sync::watch::channel(());

        loop {
            let (stream, _remote) = tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok(conn) => conn,
                    // Transient accept errors must not kill the loop (mirror
                    // `net/http`'s accept retry); a fatal error still surfaces.
                    Err(err) if is_transient_accept_error(&err) => continue,
                    Err(source) => {
                        return Err(ApiError::Listen { addr: addr.to_string(), source });
                    }
                },
                () = signal_tx.closed() => break,
            };

            let io = hyper_util::rt::TokioIo::new(stream);
            let service = hyper_util::service::TowerToHyperService::new(router.clone());
            let builder = builder.clone();
            let signal_tx = signal_tx.clone();
            let close_rx = close_rx.clone();

            tokio::spawn(async move {
                let conn = builder.serve_connection_with_upgrades(io, service);
                tokio::pin!(conn);
                let signal_closed = signal_tx.closed();
                tokio::pin!(signal_closed);
                loop {
                    tokio::select! {
                        _ = conn.as_mut() => break,
                        () = &mut signal_closed => {
                            conn.as_mut().graceful_shutdown();
                        }
                    }
                }
                drop(close_rx);
            });
        }

        // Stop accepting, then wait for in-flight connection tasks to drain.
        drop(close_rx);
        drop(listener);
        close_tx.closed().await;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        // `notify_one` stores a permit if no waiter is registered yet, so a
        // `shutdown()` that races ahead of `serve()` registering its waiter is
        // NOT lost (unlike `notify_waiters`, which drops the signal on the
        // floor). The serve loop and every connection task await this `Notify`.
        self.shutdown.notify_one();
        Ok(())
    }
}

/// Whether a `TcpListener::accept` error is transient (per-connection) and the
/// accept loop should keep running, vs. fatal. Mirrors `net/http`'s accept
/// retry on connection-level errors.
fn is_transient_accept_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
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

        // An allowed-hosts 403 (a layer that short-circuits BEFORE reaching the
        // route) ALSO carries node-id: the node-id layer is outermost, so it
        // wraps the allowed-hosts rejection. Build a host-restricted server.
        let mut cfg = test_http_config();
        cfg.http_allowed_hosts = vec!["localhost".to_string()];
        let srv = Server::new(cfg, test_node_id());
        srv.reserve(
            "/ext/info".to_string(),
            Router::new().route("/", get(|| async { "ok" })),
        )
        .expect("reserve route");
        let router = srv.build_router().expect("build router");

        let request = Request::builder()
            .uri("/ext/info")
            .header(axum::http::header::HOST, "evil.example.com")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
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

    // Go's `AddAlias` force-adds EVERY endpoint already registered under the
    // canonical base — not just the base itself — under each alias
    // (router.go:171-178). A pre-existing sub-endpoint must be reachable via
    // the alias too.
    #[test]
    fn add_aliases_propagates_preexisting_sub_endpoints() {
        let srv = server();
        srv.add_route(Router::new(), "bc/2x...", "").expect("base");
        srv.add_route(Router::new(), "bc/2x...", "/rpc")
            .expect("sub-endpoint");
        srv.add_aliases("bc/2x...", &["bc/X".to_string()])
            .expect("alias");
        let registry = srv.registry.lock();
        assert!(
            registry.routes.iter().any(|r| r.path == "/ext/bc/X"),
            "alias of the base mount"
        );
        assert!(
            registry.routes.iter().any(|r| r.path == "/ext/bc/X/rpc"),
            "alias of the pre-existing sub-endpoint"
        );
    }

    // Go's `router.AddAlias` does NOT require the canonical route to pre-exist:
    // it reserves the alias name + records the mapping, and a LATER `add_route`
    // propagates the handler to the alias.
    #[test]
    fn add_aliases_before_route_propagates_on_later_registration() {
        let srv = server();
        // Alias registered first, with no canonical route yet — must succeed.
        srv.add_aliases("bc/2x...", &["bc/X".to_string()])
            .expect("alias before route");
        {
            let registry = srv.registry.lock();
            // No route propagated yet (canonical does not exist), but the alias
            // name is reserved and the mapping recorded.
            assert!(!registry.routes.iter().any(|r| r.path == "/ext/bc/X"));
            assert!(registry.reserved_aliases.iter().any(|a| a == "/ext/bc/X"));
        }

        // Registering the canonical route NOW propagates to the alias.
        srv.add_route(Router::new(), "bc/2x...", "")
            .expect("canonical route");
        let registry = srv.registry.lock();
        assert!(registry.routes.iter().any(|r| r.path == "/ext/bc/2x..."));
        assert!(registry.routes.iter().any(|r| r.path == "/ext/bc/X"));
    }

    // Duplicate alias names are still rejected (mirror Go's `reservedRoutes`).
    #[test]
    fn add_aliases_rejects_duplicate_alias() {
        let srv = server();
        srv.add_aliases("bc/2x...", &["bc/X".to_string()])
            .expect("first alias");
        // Re-reserving the same alias name fails, even for a different canonical.
        let dup = srv.add_aliases("bc/other", &["bc/X".to_string()]);
        assert!(matches!(dup, Err(ApiError::AlreadyReserved { .. })));
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
        let vm: Arc<tokio::sync::Mutex<dyn Vm>> = Arc::new(tokio::sync::Mutex::new(TestVm::new()));
        srv.register_chain("C-Chain", &ctx, vm).await;

        let registry = srv.registry.lock();
        assert_eq!(registry.chains.len(), 1);
        let registered = registry.chains.first().expect("one chain registered");
        assert_eq!(registered.ctx.primary_alias, "C");
    }

    // Helper: a ConsensusContext for chain id `chain_id`, in the Initializing
    // (non-NormalOp) phase, with a real chain id so its mount prefix matches the
    // route registered under it.
    fn ctx_for_chain(chain_id: Id) -> Arc<ava_snow::ConsensusContext> {
        use ava_snow::{ChainContext, ConsensusContext, NoOpAcceptor};
        let chain = Arc::new(ChainContext {
            network_id: 1,
            subnet_id: Id::EMPTY,
            chain_id,
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

    // ------------------------------------------------------------------
    // Per-chain 503: a route mounted under a non-NormalOp chain's prefix is
    // rejected with the exact Go message — driven THROUGH `build_router` (the
    // composed router), not just the bare middleware.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn build_router_applies_per_chain_503() {
        use ava_snow::EngineState;

        use crate::middleware::NOT_BOOTSTRAPPED_MSG;

        let chain_id = Id::from_slice(&[3u8; 32]).expect("32-byte chain id");
        let ctx = ctx_for_chain(chain_id);

        let srv = server();
        // Register the chain (records prefix + ctx), then mount a route under
        // its prefix /ext/bc/<chainID>/rpc (what M8.22 will do per VM handler).
        let vm: Arc<tokio::sync::Mutex<dyn Vm>> =
            Arc::new(tokio::sync::Mutex::new(ava_vm::testutil::TestVm::new()));
        srv.register_chain("C-Chain", &ctx, vm).await;
        let rpc_path = Server::route_path(&format!("bc/{chain_id}"), "rpc").expect("path");
        srv.reserve(
            rpc_path.clone(),
            Router::new().route("/", get(|| async { "ok" })),
        )
        .expect("reserve chain route");

        let router = srv.build_router().expect("build router");

        // Initializing => 503 with the exact Go message, through the full router.
        let request = Request::builder()
            .uri(&rpc_path)
            .body(Body::empty())
            .expect("request");
        let response = router.clone().oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(String::from_utf8_lossy(&bytes), NOT_BOOTSTRAPPED_MSG);

        // After NormalOp => the same route is served (live state read).
        ctx.state.store(Arc::new(EngineState::NormalOp));
        let request = Request::builder()
            .uri(&rpc_path)
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
    }

    // A non-chain route (no registered chain prefix) is NOT wrapped by the 503
    // layer — it serves regardless of any chain's bootstrap state.
    #[tokio::test]
    async fn build_router_does_not_503_non_chain_routes() {
        let srv = server();
        // Register an Initializing chain, but mount an unrelated /ext/info route.
        let chain_id = Id::from_slice(&[5u8; 32]).expect("chain id");
        let vm: Arc<tokio::sync::Mutex<dyn Vm>> =
            Arc::new(tokio::sync::Mutex::new(ava_vm::testutil::TestVm::new()));
        srv.register_chain("C-Chain", &ctx_for_chain(chain_id), vm)
            .await;
        srv.reserve(
            "/ext/info".to_string(),
            Router::new().route("/", get(|| async { "ok" })),
        )
        .expect("reserve info");

        let router = srv.build_router().expect("build router");
        let request = Request::builder()
            .uri("/ext/info")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ------------------------------------------------------------------
    // Graceful serve/shutdown round-trip on an ephemeral port: serve() binds,
    // a request succeeds, shutdown() ends the serve future. Also exercises the
    // explicit hyper-util accept loop (h2c builder) end-to-end over HTTP/1.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn serve_and_shutdown_round_trip() {
        // Bind an ephemeral port and discover it, then build a server on it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let mut cfg = test_http_config();
        cfg.http_port = port;
        let srv = Arc::new(Server::new(cfg, test_node_id()));
        srv.reserve(
            "/ext/info".to_string(),
            Router::new().route("/", get(|| async { "ok" })),
        )
        .expect("reserve");

        let serving = srv.clone();
        let handle = tokio::spawn(async move { serving.serve().await });

        // Wait for the listener to come up, then issue a plain HTTP/1 request.
        let body = loop {
            match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                Ok(mut stream) => {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let req = format!(
                        "GET /ext/info HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(req.as_bytes()).await.expect("write");
                    let mut buf = Vec::new();
                    stream.read_to_end(&mut buf).await.expect("read");
                    break String::from_utf8_lossy(&buf).into_owned();
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };
        assert!(body.contains("200 OK"), "expected 200, got: {body}");
        assert!(body.contains("ok"), "expected body, got: {body}");
        // node-id header present on the wire response.
        assert!(
            body.to_lowercase().contains("node-id:"),
            "expected node-id header, got: {body}"
        );

        // Shutdown ends the serve future.
        srv.shutdown().await.expect("shutdown");
        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("serve future finished")
            .expect("join");
        result.expect("serve returned Ok");
    }

    // ------------------------------------------------------------------
    // Shutdown-before-serve race: `notify_one` buffers a permit, so a
    // shutdown() issued before serve() registers its waiter still stops it.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn shutdown_before_serve_is_not_lost() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let mut cfg = test_http_config();
        cfg.http_port = port;
        let srv = Arc::new(Server::new(cfg, test_node_id()));

        // Signal shutdown FIRST — the permit must be buffered.
        srv.shutdown().await.expect("shutdown");

        // serve() must observe the buffered permit and return promptly.
        let serving = srv.clone();
        let result =
            tokio::time::timeout(Duration::from_secs(5), async move { serving.serve().await })
                .await
                .expect("serve returned without hanging");
        result.expect("serve returned Ok");
    }
}
