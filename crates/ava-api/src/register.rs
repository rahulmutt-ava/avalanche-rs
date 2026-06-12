// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Chain mounting — `register_chain` (mirror Go `api/server/server.go:153
//! RegisterChain`; 12 §3.1, 14 §1.2/§13).
//!
//! The chain manager calls [`crate::ApiServer::register_chain`], which:
//! 1. calls `vm.create_handlers()` and mounts each returned extension at
//!    `/ext/bc/<chainID>/<extension>` (`""` ⇒ `/ext/bc/<chainID>`; the
//!    extension is validated like Go's `url.ParseRequestURI` — a malformed
//!    extension is skipped with an error log, `server.go:175`);
//! 2. calls `vm.new_http_handler()` and, when non-nil, registers it as the
//!    **header-route** handler for `<chainID>` (EVM `/rpc`/`/ws`, proposervm
//!    Connect; `server.go:190-211`);
//! 3. registers the chain's primary alias (`P`/`X`/`C`) as a path alias so
//!    `/ext/bc/P` etc. resolve (14 §13);
//! 4. wraps handlers in the per-chain middleware (`wrapMiddleware`,
//!    `server.go:226`): the not-bootstrapped `503` reject layer is applied
//!    (path routes via the chain-prefix matching in `Server::build_router`,
//!    the header route at registration time); the per-chain HTTP metrics and
//!    OTel trace wrappers are deferred (no HTTP metrics interceptor /
//!    tracer in `ava-api` yet — see the M8.22 report).
//!
//! In-process VM handlers ride the buffered [`VmHttpService`] seam
//! (`ava-vm`); [`vm_service_router`] adapts one onto an [`axum::Router`],
//! including the WebSocket upgrade path (12 §3.8): each WS text/binary frame
//! is dispatched through the buffered handler as a `POST` and the response
//! body returned as a text frame (JSON-RPC over WS; server-push subscriptions
//! are a follow-up).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{OriginalUri, Request, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use ava_snow::ConsensusContext;
use ava_vm::vm::{Vm, VmHttpService, VmRequest, VmResponse};

use crate::middleware::not_bootstrapped;
use crate::server::Server;

impl Server {
    /// The full chain-mounting contract (Go `server.RegisterChain`,
    /// `api/server/server.go:153`). Mirrors Go's error handling: failures are
    /// logged and skipped, never propagated (`RegisterChain` returns nothing).
    pub(crate) async fn register_chain_impl(
        &self,
        name: &str,
        ctx: &Arc<ConsensusContext>,
        vm: &Arc<tokio::sync::Mutex<dyn Vm>>,
    ) {
        let token = CancellationToken::new();
        let chain_id = ctx.chain.chain_id.to_string();

        // Go acquires `ctx.Lock` around CreateHandlers (server.go:154-156);
        // the VM mutex is the Rust equivalent.
        let path_route_handlers = match vm.lock().await.create_handlers(&token).await {
            Ok(handlers) => handlers,
            Err(e) => {
                error!(chain_name = name, error = %e, "failed to create path route handlers");
                return;
            }
        };

        // Record the chain (prefix + ctx) so `build_router` wraps every route
        // at/beneath /ext/bc/<chainID> with its not-bootstrapped 503 layer.
        let default_base = format!("bc/{chain_id}");
        self.record_chain(ctx);

        // Chain alias (P/X/C): register the context's primary alias as a path
        // alias of the canonical /ext/bc/<chainID> mount (14 §13 step 3).
        // Alias-before-route is supported, so order is immaterial.
        let alias = &ctx.primary_alias;
        if !alias.is_empty()
            && *alias != chain_id
            && let Err(e) = crate::ApiServer::add_aliases(
                self,
                &default_base,
                std::slice::from_ref(&format!("bc/{alias}")),
            )
        {
            error!(chain_name = name, alias = %alias, error = %e, "error adding chain alias");
        }

        // Mount each extension at /ext/bc/<chainID>/<extension> (sorted for
        // deterministic registration; Go iterates the map in random order).
        let mut extensions: Vec<_> = path_route_handlers.into_iter().collect();
        extensions.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (extension, handler) in extensions {
            // Validate the route like Go's `url.ParseRequestURI` (server.go:172):
            // "" and "/foo" are ok, a relative path or control bytes are not.
            if !extension.is_empty() && !parse_request_uri_ok(&extension) {
                error!(
                    reason = "route is malformed",
                    extension = %extension.escape_debug(),
                    "could not add route to chain's API handler"
                );
                continue;
            }
            let Some(service) = handler.service else {
                // An opaque (rpcchainvm wire) descriptor has no in-process
                // service to mount; the host-side ghttp proxy is a follow-up
                // (tests/PORTING.md).
                error!(
                    chain_name = name,
                    extension = %extension,
                    "error adding route: handler has no in-process service"
                );
                continue;
            };
            info!(url = %format!("/ext/{default_base}"), endpoint = %extension, "adding route");
            if let Err(e) = crate::ApiServer::add_route(
                self,
                vm_service_router(service),
                &default_base,
                &extension,
            ) {
                error!(chain_name = name, error = %e, "error adding route");
            }
        }

        // Header-route handler (server.go:190-211).
        let header_handler = match vm.lock().await.new_http_handler(&token).await {
            Ok(handler) => handler,
            Err(e) => {
                error!(chain_name = name, error = %e, "failed to create header route handler");
                return;
            }
        };
        let Some(header_handler) = header_handler else {
            return;
        };
        let Some(service) = header_handler.service else {
            error!(
                chain_name = name,
                "failed to add header route: handler has no in-process service"
            );
            return;
        };
        // wrapMiddleware (server.go:226): the not-bootstrapped 503 reject layer
        // is applied here for the header route (path routes get it from the
        // chain-prefix matching in `build_router`).
        let wrapped = vm_service_router(service).layer(axum::middleware::from_fn_with_state(
            ctx.clone(),
            not_bootstrapped,
        ));
        if !self.header_routes().add(&chain_id, wrapped) {
            error!(chain_name = name, "failed to add header route");
        }
    }
}

/// Whether `extension` would be accepted by Go's `url.ParseRequestURI`
/// (`server.go:172`): an absolute path (`/...`) or an absolute URI
/// (`scheme://...`), with no ASCII control bytes or spaces. A bare relative
/// path (`"foo"`) errors in Go (`"invalid URI for request"`) and is rejected
/// here too.
fn parse_request_uri_ok(extension: &str) -> bool {
    if extension.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return false;
    }
    extension.starts_with('/') || extension.contains("://")
}

/// Adapts a buffered in-process VM handler ([`VmHttpService`]) onto an
/// [`axum::Router`] serving every method and sub-path of its mount, plus the
/// WebSocket upgrade path (12 §3.8).
pub fn vm_service_router(service: Arc<dyn VmHttpService>) -> Router {
    let handler = move |ws: Option<WebSocketUpgrade>,
                        OriginalUri(original_uri): OriginalUri,
                        req: Request| {
        let service = service.clone();
        async move {
            match ws {
                Some(upgrade) => {
                    // WS upgrade (EVM /ws-style mounts): bridge each frame
                    // through the buffered handler as a JSON-RPC POST.
                    let headers: Vec<(String, String)> = copy_headers(req.headers());
                    let uri = original_uri.to_string();
                    upgrade
                        .on_upgrade(move |socket| ws_bridge(socket, service, uri, headers))
                        .into_response()
                }
                None => buffered_call(service.as_ref(), original_uri.to_string(), req).await,
            }
        }
    };
    Router::new()
        .route("/", any(handler.clone()))
        .route("/*rest", any(handler))
}

/// Copies transport headers into the buffered seam's `(name, value)` pairs,
/// preserving multiplicity (Go `http.Header` is a `map[string][]string`).
fn copy_headers(headers: &axum::http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// Buffers an axum request, dispatches it through the [`VmHttpService`] seam,
/// and rebuilds the response.
async fn buffered_call(service: &dyn VmHttpService, uri: String, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let vm_req = VmRequest {
        method: parts.method.as_str().to_string(),
        uri,
        headers: copy_headers(&parts.headers),
        body,
    };
    vm_response_to_http(service.serve_http(vm_req).await)
}

/// Rebuilds an axum [`Response`] from the buffered [`VmResponse`].
fn vm_response_to_http(resp: VmResponse) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    for (name, value) in &resp.headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(resp.body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The WS ↔ buffered-handler bridge: each inbound text/binary frame is served
/// as a `POST` through the seam and the response body sent back as one text
/// frame (the request/response half of JSON-RPC over WS; pub-sub push is a
/// follow-up, 12 §3.8).
async fn ws_bridge(
    mut socket: WebSocket,
    service: Arc<dyn VmHttpService>,
    uri: String,
    headers: Vec<(String, String)>,
) {
    while let Some(Ok(message)) = socket.recv().await {
        let frame = match message {
            Message::Text(text) => text.into_bytes(),
            Message::Binary(bytes) => bytes,
            Message::Ping(payload) => {
                if socket.send(Message::Pong(payload)).await.is_err() {
                    return;
                }
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(_) => return,
        };
        let mut frame_headers = headers.clone();
        frame_headers.push(("content-type".to_string(), "application/json".to_string()));
        let resp = service
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: uri.clone(),
                headers: frame_headers,
                body: frame,
            })
            .await;
        let text = String::from_utf8_lossy(&resp.body).into_owned();
        if socket.send(Message::Text(text)).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use ava_snow::{ChainContext, ConsensusContext, EngineState, NoOpAcceptor};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_vm::testutil::TestVm;
    use ava_vm::vm::{HttpHandler, LockOptions, Vm};
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use pretty_assertions::assert_eq;
    use tower::ServiceExt;

    use super::*;
    use crate::ApiServer;
    use crate::header_route::HTTP_HEADER_ROUTE;

    /// A tiny in-process service that replies `200 text/plain <tag>`.
    struct Tagged(&'static str);

    #[async_trait::async_trait]
    impl VmHttpService for Tagged {
        async fn serve_http(&self, _req: VmRequest) -> VmResponse {
            VmResponse::ok("text/plain", self.0.as_bytes().to_vec())
        }
    }

    fn tagged(tag: &'static str) -> HttpHandler {
        HttpHandler::in_process(LockOptions::WriteLock, Arc::new(Tagged(tag)))
    }

    fn test_server() -> Server {
        use ava_config::node::{ApiConfig, HttpConfig};
        use std::time::Duration;
        let config = HttpConfig {
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
        };
        Server::new(config, NodeId::from_slice(&[7u8; 20]).expect("node id"))
    }

    fn ctx_for(chain_id: Id, alias: &str) -> Arc<ConsensusContext> {
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
            alias.to_string(),
            Arc::new(NoOpAcceptor),
            Arc::new(NoOpAcceptor),
        ))
    }

    async fn get(router: &axum::Router, uri: &str) -> (StatusCode, String) {
        let req = HttpRequest::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The M8.22 red test: the full Go `RegisterChain` contract
    /// (server.go:153) — each `create_handlers` extension mounted at
    /// `/ext/bc/<chainID>/<ext>` (`""` ⇒ `/ext/bc/<chainID>`; malformed
    /// extensions skipped per `url.ParseRequestURI`), the header-route handler
    /// registered for `<chainID>`, and the chain alias resolving (14 §1.2/§13).
    #[tokio::test]
    async fn mounts_create_handlers_under_chain_id() {
        let chain_id = Id::from_slice(&[9u8; 32]).expect("chain id");
        let ctx = ctx_for(chain_id, "P");
        // NormalOp so the per-chain 503 reject layer admits the requests.
        ctx.state.store(Arc::new(EngineState::NormalOp));

        let mut vm = TestVm::new();
        vm.http_handlers = HashMap::from([
            (String::new(), tagged("root")),
            ("/rpc".to_string(), tagged("rpc")),
            // Malformed per Go url.ParseRequestURI: relative path -> skipped.
            ("relative".to_string(), tagged("never")),
            // Malformed: control byte -> skipped.
            ("/bad\next".to_string(), tagged("never")),
        ]);
        vm.http_header_handler = Some(tagged("header"));
        let vm: Arc<tokio::sync::Mutex<dyn Vm>> = Arc::new(tokio::sync::Mutex::new(vm));

        let srv = test_server();
        srv.register_chain("P-Chain", &ctx, vm).await;

        let router = srv.build_router().expect("build router");
        let id = chain_id.to_string();

        // "" extension ⇒ /ext/bc/<chainID>.
        let (status, body) = get(&router, &format!("/ext/bc/{id}")).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "root"),
            "empty extension mounts at /ext/bc/<chainID>"
        );

        // "/rpc" extension ⇒ /ext/bc/<chainID>/rpc.
        let (status, body) = get(&router, &format!("/ext/bc/{id}/rpc")).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "rpc"),
            "/rpc extension mounts at /ext/bc/<chainID>/rpc"
        );

        // Chain alias (P) resolves both mounts (14 §1.2/§13).
        let (status, body) = get(&router, "/ext/bc/P").await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "root"),
            "alias /ext/bc/P resolves the chain mount"
        );
        let (status, body) = get(&router, "/ext/bc/P/rpc").await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "rpc"),
            "alias /ext/bc/P/rpc resolves"
        );

        // Malformed extensions are skipped (server.go:175 logs + continue).
        let (status, _) = get(&router, &format!("/ext/bc/{id}/relative")).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "relative extension is rejected like url.ParseRequestURI"
        );

        // Header-route handler registered for <chainID> (server.go:204).
        assert!(
            srv.header_routes().contains(&id),
            "header route registered under the chain id"
        );
        let req = HttpRequest::builder()
            .uri("/whatever")
            .header(HTTP_HEADER_ROUTE, &id)
            .body(Body::empty())
            .expect("request");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK, "header-route dispatch");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(String::from_utf8_lossy(&bytes), "header", "header handler");
    }

    /// The header-route handler is wrapped in the chain's not-bootstrapped
    /// `503` reject middleware (`wrapMiddleware`, server.go:226).
    #[tokio::test]
    async fn header_route_rejects_before_normal_op() {
        use crate::middleware::NOT_BOOTSTRAPPED_MSG;

        let chain_id = Id::from_slice(&[4u8; 32]).expect("chain id");
        let ctx = ctx_for(chain_id, "C"); // Initializing by default.

        let mut vm = TestVm::new();
        vm.http_header_handler = Some(tagged("header"));
        let vm: Arc<tokio::sync::Mutex<dyn Vm>> = Arc::new(tokio::sync::Mutex::new(vm));

        let srv = test_server();
        srv.register_chain("C-Chain", &ctx, vm).await;
        let router = srv.build_router().expect("build router");

        let req = HttpRequest::builder()
            .uri("/whatever")
            .header(HTTP_HEADER_ROUTE, chain_id.to_string())
            .body(Body::empty())
            .expect("request");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "header route 503 before NormalOp"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(String::from_utf8_lossy(&bytes), NOT_BOOTSTRAPPED_MSG);

        // NormalOp -> served (live state read through the registration wrap).
        ctx.state.store(Arc::new(EngineState::NormalOp));
        let req = HttpRequest::builder()
            .uri("/whatever")
            .header(HTTP_HEADER_ROUTE, chain_id.to_string())
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK, "header route after NormalOp");
    }

    /// `parse_request_uri_ok` mirrors Go's accept/reject set.
    #[test]
    fn extension_validation() {
        assert!(parse_request_uri_ok("/rpc"), "/rpc accepted");
        assert!(parse_request_uri_ok("/proposervm"), "/proposervm accepted");
        assert!(
            parse_request_uri_ok("http://example/x"),
            "absolute URI accepted (ParseRequestURI)"
        );
        assert!(!parse_request_uri_ok("relative"), "relative path rejected");
        assert!(!parse_request_uri_ok("/a b"), "space rejected");
        assert!(!parse_request_uri_ok("\n"), "control byte rejected");
    }
}
