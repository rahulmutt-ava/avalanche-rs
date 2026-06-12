// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Header-based VM routing (mirror Go `api/server/router.go`; 12 §3.1, 14
//! §1.2/§16.3).
//!
//! Go's `router.ServeHTTP` checks the `Avalanche-Api-Route` request header
//! **before** path routing: when present, the request is dispatched to the
//! handler registered for the header's first value (the chain ID string),
//! bypassing the path router entirely. The EVM `/rpc`/`/ws` mounts and the
//! proposervm Connect endpoint ride this channel (a second header value
//! selects the route *within* the VM's handler, e.g. `proposervm`; Go
//! `vms/proposervm/vm.go:297`).
//!
//! Status codes (14 §16.3, Go `router.go:53-75`):
//! - header absent → fall back to path routing;
//! - header present with an **empty value** → `400` (empty body);
//! - no handler registered for the value → `404` (empty body).

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use parking_lot::RwLock;
use tower::ServiceExt;

/// The routing header key (Go `api/server/router.go:17`,
/// `HTTPHeaderRoute = "Avalanche-Api-Route"`).
///
/// > Spec drift note: 14 §1.2 names this `X-Avalanche-Vm-Route`; the Go source
/// > of record uses `Avalanche-Api-Route`, which wins for wire parity.
pub const HTTP_HEADER_ROUTE: &str = "Avalanche-Api-Route";

/// The registered header routes (Go `router.headerRoutes`): chain-ID string →
/// handler. Shared and live — Go's `RegisterChain` may run after `Dispatch`,
/// so additions must be visible to an already-serving router.
#[derive(Clone, Default)]
pub struct HeaderRoutes {
    /// `route value (chain ID string)` → mounted sub-router.
    routes: Arc<RwLock<HashMap<String, Router>>>,
}

impl HeaderRoutes {
    /// An empty route table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `handler` for `route`, returning `false` if the route is
    /// already taken (Go `router.AddHeaderRoute`; aliasing is not supported).
    #[must_use]
    pub fn add(&self, route: &str, handler: Router) -> bool {
        let mut routes = self.routes.write();
        if routes.contains_key(route) {
            return false;
        }
        routes.insert(route.to_string(), handler);
        true
    }

    /// The handler for `route`, if registered.
    #[must_use]
    fn get(&self, route: &str) -> Option<Router> {
        self.routes.read().get(route).cloned()
    }

    /// Whether any header route is registered (test/diagnostic helper).
    #[must_use]
    pub fn contains(&self, route: &str) -> bool {
        self.routes.read().contains_key(route)
    }
}

/// The axum middleware mirroring Go `router.ServeHTTP` (`router.go:53-75`):
/// dispatch by the `Avalanche-Api-Route` header when present, else fall back
/// to the legacy path-based routing (`next`).
pub async fn header_route(
    State(routes): State<HeaderRoutes>,
    req: Request,
    next: Next,
) -> Response {
    let mut values = req.headers().get_all(HTTP_HEADER_ROUTE).iter();
    let Some(first) = values.next() else {
        // No routing header: legacy path-based routing.
        return next.run(req).await;
    };

    // Key present but no usable value → 400 with an empty body
    // (Go `router.go:64` `len(route) < 1`; over the wire an empty-valued
    // header is the observable form of "key present, no value").
    let first = first.to_str().unwrap_or("");
    if first.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let Some(handler) = routes.get(first) else {
        // No handler registered for the route value → 404, empty body
        // (Go `router.go:70`).
        return StatusCode::NOT_FOUND.into_response();
    };

    match handler.oneshot(req).await {
        Ok(resp) => resp.into_response(),
        // `Router`'s service error is `Infallible`; keep the contract total.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::any;
    use pretty_assertions::assert_eq;
    use tower::ServiceExt;

    use super::*;

    /// A router that exercises the full Go `router.ServeHTTP` decision table
    /// (14 §16.3): header → handler; empty value → 400; unknown value → 404;
    /// absent header → path routing.
    fn test_router(routes: HeaderRoutes) -> Router {
        Router::new()
            .route("/ext/info", any(|| async { "path-routed" }))
            // Mirror `Server::build_router`: an explicit 404 fallback BEFORE
            // the layer, so the header dispatch sees requests whose path
            // matches no route (Go checks the header before path routing).
            .fallback(|| async { StatusCode::NOT_FOUND })
            .layer(axum::middleware::from_fn_with_state(routes, header_route))
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn route_by_header() {
        let routes = HeaderRoutes::new();
        assert!(
            routes.add(
                "2JVSBoinj9C2J33VntvzYtVJNZdN2NKiwwKjcumHUWEb5DbBrm",
                // Path-agnostic, like a Go `http.Handler`: the dispatched
                // request keeps its ORIGINAL path (router.go:74 passes it
                // unchanged), so the handler must not path-match.
                Router::new().fallback_service(any(|| async { "proposervm" })),
            ),
            "HeaderRoutes::add (first registration)"
        );
        // Duplicate registration is rejected (Go AddHeaderRoute -> false).
        assert!(
            !routes.add(
                "2JVSBoinj9C2J33VntvzYtVJNZdN2NKiwwKjcumHUWEb5DbBrm",
                Router::new(),
            ),
            "HeaderRoutes::add (duplicate rejected)"
        );
        let router = test_router(routes);

        // Header value matches a registered route → dispatched to the handler
        // regardless of the request path (header routing bypasses path routing).
        let req = HttpRequest::builder()
            .uri("/anything")
            .header(
                HTTP_HEADER_ROUTE,
                "2JVSBoinj9C2J33VntvzYtVJNZdN2NKiwwKjcumHUWEb5DbBrm",
            )
            .body(Body::empty())
            .expect("request");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK, "header route dispatch");
        assert_eq!(body_string(resp).await, "proposervm");

        // Empty header value → 400 with an empty body (router.go:64, 14 §16.3).
        let req = HttpRequest::builder()
            .uri("/ext/info")
            .header(HTTP_HEADER_ROUTE, "")
            .body(Body::empty())
            .expect("request");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "empty value -> 400");
        assert_eq!(body_string(resp).await, "", "400 body is empty");

        // Unknown route value → 404 with an empty body (router.go:70).
        let req = HttpRequest::builder()
            .uri("/ext/info")
            .header(HTTP_HEADER_ROUTE, "unknown-chain")
            .body(Body::empty())
            .expect("request");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "missing -> 404");
        assert_eq!(body_string(resp).await, "", "404 body is empty");

        // No header → legacy path routing.
        let req = HttpRequest::builder()
            .uri("/ext/info")
            .body(Body::empty())
            .expect("request");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK, "fall back to path routing");
        assert_eq!(body_string(resp).await, "path-routed");

        // Multi-valued header: dispatch is by the FIRST value (router.go:70
        // `headerRoutes[route[0]]`); the rest are the VM's concern.
        let req = HttpRequest::builder()
            .uri("/anything")
            .header(
                HTTP_HEADER_ROUTE,
                "2JVSBoinj9C2J33VntvzYtVJNZdN2NKiwwKjcumHUWEb5DbBrm",
            )
            .header(HTTP_HEADER_ROUTE, "proposervm")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK, "multi-value first-value");
        assert_eq!(body_string(resp).await, "proposervm");
    }
}
