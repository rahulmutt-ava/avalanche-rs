// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The dual GET / JSON-RPC health handler (mirror Go `api/health/handler.go` +
//! `service.go`; specs 12 §3.4, 14 §5/§16.3).
//!
//! [`handler`] returns the router the node mounts at `/ext/health`
//! (`node/node.go::initHealthAPI`):
//!
//! - `/` — the dual handler (`NewGetAndPostHandler`): **GET** serves the
//!   *health* report (`200` healthy / `503` unhealthy, body
//!   `{checks, healthy}`, `?tag=` filtered); anything else falls through to
//!   the gorilla JSON-RPC shim (`health.health` / `health.readiness` /
//!   `health.liveness`, POST-only with a real `405` otherwise; 14 §16.3).
//! - `/health`, `/readiness`, `/liveness` — per-reporter GET-style handlers
//!   (`NewGetHandler`). Go's `NewGetHandler` does **not** branch on the HTTP
//!   method, so any method serves the report here too.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_api_macros::rpc_service;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use tracing::debug;

use crate::health::types::{APIArgs, APIReply};
use crate::health::{Health, types};
use crate::jsonrpc::{RpcError, ServiceRegistry, dispatch};

/// The axum state behind the health routes: the health service plus the
/// JSON-RPC registry for the dual root handler.
#[derive(Clone)]
struct HealthState {
    /// The health service.
    health: Arc<Health>,
    /// The `health.*` JSON-RPC dispatch table.
    registry: Arc<ServiceRegistry>,
}

/// The `health.*` JSON-RPC service (mirror Go `api/health/service.go`). The
/// snake_case method idents pascalize to Go's exact exported names (`Health`,
/// `Readiness`, `Liveness`), so no `#[rpc(name = ...)]` overrides are needed.
struct HealthService {
    /// The health service queried by every method.
    health: Arc<Health>,
}

#[rpc_service("health")]
impl HealthService {
    /// `health.health` — a summation of the health of the node.
    pub async fn health(&self, args: APIArgs) -> std::result::Result<APIReply, RpcError> {
        debug!(service = "health", method = "health", tags = ?args.tags, "API called");
        let (checks, healthy) = self.health.health(&args.tags);
        Ok(APIReply { checks, healthy })
    }

    /// `health.readiness` — whether the node has finished initializing.
    pub async fn readiness(&self, args: APIArgs) -> std::result::Result<APIReply, RpcError> {
        debug!(service = "health", method = "readiness", tags = ?args.tags, "API called");
        let (checks, healthy) = self.health.readiness(&args.tags);
        Ok(APIReply { checks, healthy })
    }

    /// `health.liveness` — whether the node is in need of a restart.
    pub async fn liveness(&self, args: APIArgs) -> std::result::Result<APIReply, RpcError> {
        debug!(service = "health", method = "liveness", tags = ?args.tags, "API called");
        let (checks, healthy) = self.health.liveness(&args.tags);
        Ok(APIReply { checks, healthy })
    }
}

/// Builds the `/ext/health` router (see the module docs for the route map).
pub fn handler(health: Arc<Health>) -> Router {
    let mut registry = ServiceRegistry::new();
    Arc::new(HealthService {
        health: Arc::clone(&health),
    })
    .register_rpc(&mut registry);
    let state = HealthState {
        health,
        registry: Arc::new(registry),
    };
    Router::new()
        .route("/", any(root))
        .route("/health", any(report_health))
        .route("/readiness", any(report_readiness))
        .route("/liveness", any(report_liveness))
        .with_state(state)
}

/// The repeated `?tag=` query values (Go `r.URL.Query()["tag"]`).
fn tag_values(query: Vec<(String, String)>) -> Vec<String> {
    query
        .into_iter()
        .filter_map(|(key, value)| (key == "tag").then_some(value))
        .collect()
}

/// Renders a report as Go's `NewGetHandler` does: `Content-Type:
/// application/json`, `503` when unhealthy (`200` otherwise), the
/// `{checks, healthy}` body with `json.Encoder`'s trailing newline.
fn report_response(report: (BTreeMap<String, types::Result>, bool)) -> Response {
    let (checks, healthy) = report;
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let reply = APIReply { checks, healthy };
    match serde_json::to_vec(&reply) {
        Ok(mut body) => {
            // Go writes the body via json.NewEncoder(w).Encode, which appends
            // a trailing newline; reproduced for byte parity.
            body.push(b'\n');
            (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        // Unreachable for the value types we construct; stay total without
        // panicking in library code.
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The dual root handler (Go `NewGetAndPostHandler`): GET serves the health
/// report; every other method falls through to the JSON-RPC shim (which
/// enforces POST-only with a real `405`; 14 §16.3).
async fn root(
    State(state): State<HealthState>,
    method: Method,
    headers: HeaderMap,
    Query(query): Query<Vec<(String, String)>>,
    body: Bytes,
) -> Response {
    if method == Method::GET {
        return report_response(state.health.health(&tag_values(query)));
    }
    dispatch(State(state.registry), method, headers, body).await
}

/// `/health` — the health reporter (any method; Go `NewGetHandler`).
async fn report_health(
    State(state): State<HealthState>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    report_response(state.health.health(&tag_values(query)))
}

/// `/readiness` — the readiness reporter (any method; Go `NewGetHandler`).
async fn report_readiness(
    State(state): State<HealthState>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    report_response(state.health.readiness(&tag_values(query)))
}

/// `/liveness` — the liveness reporter (any method; Go `NewGetHandler`).
async fn report_liveness(
    State(state): State<HealthState>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    report_response(state.health.liveness(&tag_values(query)))
}

#[cfg(test)]
// `serde_json::Value` indexing returns `Value::Null` on a missing key rather
// than panicking; it is the idiomatic way to assert on JSON response bodies.
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use futures::future::BoxFuture;
    use pretty_assertions::assert_eq;
    use prometheus::Registry;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::health::{APPLICATION_TAG, CheckError, CheckResult, Checker, Health, handler};

    /// A checker that always passes with the given details.
    fn passing(details: Value) -> Arc<dyn Checker> {
        Arc::new(move || -> BoxFuture<'static, CheckResult> {
            let details = details.clone();
            Box::pin(async move { Ok(details) })
        })
    }

    /// A checker that always fails with the given message.
    fn failing(message: &str) -> Arc<dyn Checker> {
        let message = message.to_string();
        Arc::new(move || -> BoxFuture<'static, CheckResult> {
            let message = message.clone();
            Box::pin(async move { Err(CheckError::new(message)) })
        })
    }

    fn new_health() -> Arc<Health> {
        Arc::new(Health::new(&Registry::new()).expect("Health::new()"))
    }

    async fn send(router: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
        let response = router.clone().oneshot(request).await.expect("oneshot");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn get(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        send(router, request).await
    }

    async fn post_json(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        send(router, request).await
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): GET /ext/health -> 200 when healthy / 503 when unhealthy,
    // body {checks, healthy} (14 §5 / §16.3).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn get_returns_200_or_503() {
        let health = new_health();
        health
            .register_health_check("c", passing(json!("ok")), &[])
            .expect("register_health_check(c)");
        let router = handler(health.clone());

        // Before the first run the check is "not yet run" => failing => 503.
        let (status, body) = get(&router, "/").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "GET before first run"
        );
        assert_eq!(body["healthy"], json!(false), "healthy before first run");
        assert_eq!(
            body["checks"]["c"]["error"],
            json!("not yet run"),
            "not-yet-run error string"
        );

        // After a run the passing check flips the report healthy => 200.
        health.run_checks_now().await;
        let (status, body) = get(&router, "/").await;
        assert_eq!(status, StatusCode::OK, "GET after passing run");
        assert_eq!(body["healthy"], json!(true), "healthy after passing run");
        assert_eq!(body["checks"]["c"]["message"], json!("ok"), "check details");

        // A failing check flips it back => 503 with the error surfaced.
        health
            .register_health_check("bad", failing("boom"), &[])
            .expect("register_health_check(bad)");
        health.run_checks_now().await;
        let (status, body) = get(&router, "/").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "GET with failing check"
        );
        assert_eq!(body["healthy"], json!(false), "healthy with failing check");
        assert_eq!(
            body["checks"]["bad"]["error"],
            json!("boom"),
            "failure error"
        );
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): repeated `?tag=` query params filter the GET report; an
    // application-tagged check is always included (worker.Results semantics).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn get_tag_filtering() {
        let health = new_health();
        health
            .register_health_check("a", passing(Value::Null), &["tag1".to_string()])
            .expect("register a");
        health
            .register_health_check("b", failing("b failed"), &["tag2".to_string()])
            .expect("register b");
        health
            .register_health_check("app", passing(Value::Null), &[APPLICATION_TAG.to_string()])
            .expect("register app");
        health.run_checks_now().await;
        let router = handler(health);

        // ?tag=tag1 reports only `a` + the application check: healthy => 200.
        let (status, body) = get(&router, "/?tag=tag1").await;
        assert_eq!(status, StatusCode::OK, "GET ?tag=tag1");
        let checks = body["checks"].as_object().expect("checks object");
        assert_eq!(
            checks.keys().collect::<Vec<_>>(),
            vec!["a", "app"],
            "tag1 check set"
        );

        // No tags = all checks; `b` is failing => 503.
        let (status, body) = get(&router, "/").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "GET all tags");
        assert_eq!(body["healthy"], json!(false));

        // Repeated tag params union: tag1 + tag2 includes the failing b => 503.
        let (status, body) = get(&router, "/?tag=tag1&tag=tag2").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "GET ?tag=tag1&tag=tag2"
        );
        let checks = body["checks"].as_object().expect("checks object");
        assert_eq!(
            checks.keys().collect::<Vec<_>>(),
            vec!["a", "app", "b"],
            "union check set"
        );
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): the /health, /readiness, /liveness subpaths serve their
    // respective reporters on GET (node.go mounts NewGetHandler per reporter).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn get_subpaths() {
        let health = new_health();
        health
            .register_health_check("h", passing(Value::Null), &[])
            .expect("register health check");
        health
            .register_readiness_check("r", passing(Value::Null), &[])
            .expect("register readiness check");
        health
            .register_liveness_check("l", failing("dead"), &[])
            .expect("register liveness check");
        health.run_checks_now().await;
        let router = handler(health);

        let (status, body) = get(&router, "/health").await;
        assert_eq!(status, StatusCode::OK, "GET /health");
        assert!(body["checks"]["h"].is_object(), "/health reports h");
        assert!(body["checks"]["l"].is_null(), "/health does not report l");

        let (status, body) = get(&router, "/readiness").await;
        assert_eq!(status, StatusCode::OK, "GET /readiness");
        assert!(body["checks"]["r"].is_object(), "/readiness reports r");

        let (status, body) = get(&router, "/liveness").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "GET /liveness");
        assert_eq!(
            body["checks"]["l"]["error"],
            json!("dead"),
            "/liveness error"
        );
    }

    // Go parity: `NewGetHandler` does NOT branch on the HTTP method, and
    // node.go mounts it on the subpaths for all methods — a POST to
    // /ext/health/readiness serves the same report (NOT a 405). Verified
    // against avalanchego api/health/handler.go + node/node.go.
    #[tokio::test]
    async fn non_get_on_subpath_serves_report() {
        let health = new_health();
        health
            .register_readiness_check("r", passing(Value::Null), &[])
            .expect("register readiness check");
        health.run_checks_now().await;
        let router = handler(health);

        let (status, body) = post_json(&router, "/readiness", json!({})).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "POST /readiness (Go-parity any-method)"
        );
        assert_eq!(body["healthy"], json!(true));
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): POST JSON-RPC on the root dispatches health.health /
    // health.readiness / health.liveness with APIArgs{tags} (14 §5).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn post_jsonrpc() {
        let health = new_health();
        health
            .register_health_check("h", passing(Value::Null), &["tag1".to_string()])
            .expect("register health check");
        health
            .register_readiness_check("r", passing(Value::Null), &[])
            .expect("register readiness check");
        health
            .register_liveness_check("l", failing("dead"), &[])
            .expect("register liveness check");
        health.run_checks_now().await;
        let router = handler(health);

        // health.health
        let (status, body) = post_json(
            &router,
            "/",
            json!({"jsonrpc": "2.0", "id": 1, "method": "health.health", "params": [{}]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "POST health.health");
        assert_eq!(
            body["result"]["healthy"],
            json!(true),
            "health.health healthy"
        );
        assert!(
            body["result"]["checks"]["h"].is_object(),
            "health.health reports h"
        );

        // health.health with tags filtering (no matching check beyond app set).
        let (status, body) = post_json(
            &router,
            "/",
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "health.health",
                "params": [{"tags": ["other"]}],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "POST health.health tags=[other]");
        assert_eq!(
            body["result"]["checks"],
            json!({}),
            "no checks match tag 'other'"
        );

        // health.readiness
        let (status, body) = post_json(
            &router,
            "/",
            json!({"jsonrpc": "2.0", "id": 3, "method": "health.readiness", "params": [{}]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "POST health.readiness");
        assert_eq!(body["result"]["healthy"], json!(true), "readiness healthy");

        // health.liveness
        let (status, body) = post_json(
            &router,
            "/",
            json!({"jsonrpc": "2.0", "id": 4, "method": "health.liveness", "params": [{}]}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "POST health.liveness (HTTP 200 + body)"
        );
        assert_eq!(
            body["result"]["healthy"],
            json!(false),
            "liveness unhealthy"
        );
        assert_eq!(
            body["result"]["checks"]["l"]["error"],
            json!("dead"),
            "liveness error"
        );

        // The gorilla uppercase-method guard applies: health.Health => -32601.
        let (status, body) = post_json(
            &router,
            "/",
            json!({"jsonrpc": "2.0", "id": 5, "method": "health.Health", "params": [{}]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["error"]["code"],
            json!(crate::error::json2_code::NO_METHOD),
            "uppercase method guard"
        );
    }

    // A non-GET, non-POST method on the root falls through to the JSON-RPC
    // branch, whose gorilla parity is a real 405 (14 §16.3).
    #[tokio::test]
    async fn non_get_non_post_root_is_405() {
        let health = new_health();
        let router = handler(health);
        let request = Request::builder()
            .method(Method::PUT)
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "PUT /ext/health"
        );
    }

    // The GET branch sets Content-Type: application/json (Go handler.go).
    #[tokio::test]
    async fn get_sets_json_content_type() {
        let health = new_health();
        let router = handler(health);
        let request = Request::builder()
            .method(Method::GET)
            .uri("/")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "GET Content-Type"
        );
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): the worker loop runs registered checkers on the configured
    // frequency; failures accumulate contiguousFailures and pin
    // timeOfFirstFailure (worker.go semantics).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn worker_runs_on_frequency() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let health = new_health();
        let runs = Arc::new(AtomicU64::new(0));
        let counted = {
            let runs = Arc::clone(&runs);
            Arc::new(move || -> BoxFuture<'static, CheckResult> {
                let runs = Arc::clone(&runs);
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(Value::Null)
                })
            }) as Arc<dyn Checker>
        };
        health
            .register_health_check("counted", counted, &[])
            .expect("register counted");
        health
            .register_health_check("bad", failing("boom"), &[])
            .expect("register bad");

        health.start(Duration::from_millis(10));
        // Repeated calls to start are no-ops (Go startOnce).
        health.start(Duration::from_millis(10));

        // The loop must run the checks repeatedly (>= 3 ticks).
        tokio::time::timeout(Duration::from_secs(5), async {
            while runs.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("worker loop ran checks repeatedly");

        // contiguousFailures grows and timeOfFirstFailure is pinned to the
        // streak start.
        let (checks, healthy) = health.health(&[]);
        assert!(!healthy, "failing check => unhealthy");
        let bad = checks.get("bad").expect("bad result");
        assert!(bad.contiguous_failures >= 1, "contiguousFailures grows");
        let first_failure = bad.time_of_first_failure.expect("timeOfFirstFailure set");

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (checks, _) = health.health(&[]);
                let bad_now = checks.get("bad").expect("bad result").clone();
                if bad_now.contiguous_failures > bad.contiguous_failures {
                    assert_eq!(
                        bad_now.time_of_first_failure,
                        Some(first_failure),
                        "timeOfFirstFailure pinned across the streak"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("observed a later failing run");

        // Stop halts the loop: the counter freezes.
        health.stop().await;
        let frozen = runs.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(runs.load(Ordering::SeqCst), frozen, "no runs after stop()");
    }
}
