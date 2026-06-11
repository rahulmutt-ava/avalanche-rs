// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The gorilla-`json2`-parity JSON-RPC 2.0 shim and service registry
//! (specs 12 §3.2, 14 §1.1/§16.1/§16.3/§16.5).
//!
//! avalanchego's non-EVM services are served by `gorilla/rpc/v2` with the v1
//! `json` codec. This module reproduces that wire shape exactly so existing
//! clients/SDKs are unaffected:
//!
//! - **Request** `{"jsonrpc":"2.0","id":1,"method":"<service>.<Method>",
//!   "params":[{…}]}`. The method segment matches **case-insensitively**
//!   (gorilla lowercases it), and `params` is a **single-element array**
//!   wrapping the `Args` object; an absent / empty `params` maps to `null`.
//! - **Success** `{"jsonrpc":"2.0","id":1,"result":{…}}`.
//! - **Error** `{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"…",
//!   "data":null}}`.
//!
//! **HTTP status nuance (14 §16.1).** A handler / domain error returns **HTTP
//! 200** with a JSON-RPC error body — `json2.writeServerResponse` ignores the
//! `400` the outer server passes it. Only *pre-dispatch* failures use real HTTP
//! status codes: `405` (non-POST) and `415` (unrecognized `Content-Type`)
//! per 14 §16.3.
//!
//! Services register through [`ServiceRegistry`]; the `#[rpc_service("name")]`
//! macro ([`ava_api_macros::rpc_service`]) generates the registration so the
//! method set cannot drift from the trait (12 §3.2). [`dispatch`] is the axum
//! `POST` handler mounted by the server.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use serde_json::Value;

use crate::error::{IntoJsonRpcError, JsonRpcError, json2_code};

/// The error a registered JSON-RPC method returns. It carries the full on-wire
/// shape (code / message / data) so a handler can surface any json2 code, but
/// the common path is [`RpcError::server`] (`-32000`) built from a domain error
/// via [`From`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("json-rpc error {code}: {message}")]
pub struct RpcError {
    /// The json2 error code (see [`json2_code`]).
    pub code: i32,
    /// The human-readable message (byte-stable for client-parsed messages).
    pub message: String,
    /// Optional structured data; serialized as explicit `null` when absent.
    pub data: Option<Value>,
}

impl RpcError {
    /// A generic server error (`-32000`) — the gorilla default for any
    /// handler-returned error (14 §16.1).
    #[must_use]
    pub fn server(message: impl Into<String>) -> Self {
        Self {
            code: json2_code::SERVER,
            message: message.into(),
            data: None,
        }
    }

    /// An invalid-params error (`-32602`, `errInvalidArg`): the `params` object
    /// failed to deserialize into the method's `Args` (14 §16.1).
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: json2_code::BAD_PARAMS,
            message: message.into(),
            data: None,
        }
    }

    /// A method-not-found error (`-32601`, `E_NO_METHOD`).
    #[must_use]
    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: json2_code::NO_METHOD,
            message: message.into(),
            data: None,
        }
    }

    /// A reserved internal error (`-32603`, `E_INTERNAL`).
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: json2_code::INTERNAL,
            message: message.into(),
            data: None,
        }
    }

    /// The on-wire [`JsonRpcError`] view of this error.
    #[must_use]
    pub fn to_wire(&self) -> JsonRpcError {
        JsonRpcError {
            code: self.code,
            message: self.message.clone(),
            data: self.data.clone(),
        }
    }
}

impl From<JsonRpcError> for RpcError {
    fn from(e: JsonRpcError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            data: e.data,
        }
    }
}

/// A registered JSON-RPC method: deserialized `params[0]` in, a JSON value or an
/// [`RpcError`] out. Boxed and `'static` so the registry owns it.
pub type BoxedRpcMethod =
    Box<dyn Fn(Value) -> BoxFuture<'static, std::result::Result<Value, RpcError>> + Send + Sync>;

/// The dispatch table mapping `"service.method"` (lowercased) to its handler
/// (mirror gorilla's `serviceMap`). Built once at startup, then shared
/// read-only behind an [`Arc`] in axum state.
#[derive(Default)]
pub struct ServiceRegistry {
    /// Lowercased `"service.method"` -> handler. Lowercasing the key is how the
    /// case-insensitive method match (14 §1.1) is realized at lookup time.
    methods: HashMap<String, BoxedRpcMethod>,
}

impl ServiceRegistry {
    /// A new, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `handler` under the gorilla wire name `"<service>.<Method>"`.
    ///
    /// The key is stored lowercased so dispatch matches the method segment
    /// case-insensitively (14 §1.1). The `#[rpc_service]` macro calls this once
    /// per `pub async fn`; manual registration is also supported.
    pub fn register<F>(&mut self, wire_method: impl Into<String>, handler: F)
    where
        F: Fn(Value) -> BoxFuture<'static, std::result::Result<Value, RpcError>>
            + Send
            + Sync
            + 'static,
    {
        let key = wire_method.into().to_ascii_lowercase();
        self.methods.insert(key, Box::new(handler));
    }

    /// Looks up the handler for `service.method` (both segments lowercased).
    #[must_use]
    pub fn lookup(&self, service: &str, method: &str) -> Option<&BoxedRpcMethod> {
        let key = format!(
            "{}.{}",
            service.to_ascii_lowercase(),
            method.to_ascii_lowercase()
        );
        self.methods.get(&key)
    }

    /// Whether any method is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }

    /// The number of registered methods.
    #[must_use]
    pub fn len(&self) -> usize {
        self.methods.len()
    }
}

/// The inbound JSON-RPC request (gorilla v1 codec shape).
#[derive(Debug, serde::Deserialize)]
struct Req {
    /// The protocol tag; gorilla rejects anything but `"2.0"` (`-32600`).
    #[serde(default)]
    jsonrpc: String,
    /// `"<service>.<Method>"`.
    #[serde(default)]
    method: String,
    /// The single-element params array (`params[0]` is the `Args` object). An
    /// absent / empty array maps to `null`, which a `*struct{}`-style method
    /// accepts.
    #[serde(default)]
    params: Value,
    /// The request id, echoed verbatim in the response (any JSON value).
    #[serde(default)]
    id: Value,
}

/// The outbound JSON-RPC response. Exactly one of `result` / `error` is present
/// (gorilla never emits both).
#[derive(Debug, serde::Serialize)]
struct Resp {
    /// Always `"2.0"`.
    jsonrpc: &'static str,
    /// The method result on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    /// The json2 error object on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
    /// The echoed request id.
    id: Value,
}

impl Resp {
    fn ok(result: Value, id: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            result: Some(result),
            error: None,
            id,
        }
    }

    fn err(error: JsonRpcError, id: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            result: None,
            error: Some(error),
            id,
        }
    }
}

/// gorilla extracts `params[0]` (the single Args object). A non-array `params`
/// (e.g. an object some clients send directly) is passed through as-is; an
/// empty array or a missing field becomes `null` (a `*struct{}` method accepts
/// `[]` / absent / `[{}]`).
fn first_param(params: Value) -> Value {
    match params {
        Value::Array(mut arr) => {
            if arr.is_empty() {
                Value::Null
            } else {
                arr.swap_remove(0)
            }
        }
        Value::Null => Value::Null,
        other => other,
    }
}

/// Whether the `Content-Type` is one gorilla's json codecs registered:
/// `application/json` or `application/json;charset=UTF-8` (14 §1.1/§16.3).
/// Matching is case-insensitive on the media type and tolerant of an optional
/// `charset` parameter and surrounding whitespace.
fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::CONTENT_TYPE) else {
        // A missing Content-Type is rejected (gorilla requires the json codec to
        // be selected by the header). 415 pre-dispatch.
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let media = value.split(';').next().unwrap_or("").trim();
    media.eq_ignore_ascii_case("application/json")
}

/// The axum `POST` handler for a gorilla JSON-RPC mount (12 §3.2).
///
/// Pre-dispatch transport checks (14 §16.3) run first and use real HTTP status
/// codes: `405` for a non-POST method, `415` for an unrecognized
/// `Content-Type`. Once dispatch begins, every outcome — parse error, unknown
/// method, bad params, or a domain error — is returned as **HTTP 200** with a
/// JSON-RPC error body (14 §16.1).
pub async fn dispatch(
    State(registry): State<Arc<ServiceRegistry>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 405: JSON-RPC mounts are POST-only (gorilla `v2/server.go:149`).
    if method != Method::POST {
        return (StatusCode::METHOD_NOT_ALLOWED, "405 must POST\n").into_response();
    }
    // 415: the Content-Type must select the json codec (`v2/server.go:165`).
    if !is_json_content_type(&headers) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "415 unsupported media type\n",
        )
            .into_response();
    }

    dispatch_body(&registry, &body).await
}

/// The dispatch core, split out so tests can drive it without an HTTP request.
/// Always returns an HTTP-200 JSON body (success or json2 error) per 14 §16.1.
async fn dispatch_body(registry: &ServiceRegistry, body: &[u8]) -> Response {
    // -32700 parse error: the body is not valid JSON.
    let req: Req = match serde_json::from_slice(body) {
        Ok(req) => req,
        Err(_) => {
            return json_rpc_response(Resp::err(
                JsonRpcError {
                    code: json2_code::PARSE,
                    message: "parse error".to_string(),
                    data: None,
                },
                Value::Null,
            ));
        }
    };

    // -32600 invalid request: the only protocol version gorilla accepts is 2.0.
    if req.jsonrpc != "2.0" {
        return json_rpc_response(Resp::err(
            JsonRpcError {
                code: json2_code::INVALID_REQ,
                message: "invalid request: jsonrpc must be \"2.0\"".to_string(),
                data: None,
            },
            req.id,
        ));
    }

    // gorilla splits on the FIRST '.': "service.method". A missing '.' yields an
    // empty service, which never resolves -> -32601.
    let (service, rpc_method) = match req.method.split_once('.') {
        Some(parts) => parts,
        None => ("", req.method.as_str()),
    };

    // Uppercase-service guard (`utils/json/codec.go::errUppercaseMethod`): the
    // service segment must not begin with an uppercase letter. avalanchego
    // registers lowercased service names, so an uppercase first letter is
    // -32601 (14 §16.1). The method segment itself is matched case-insensitively
    // (handled by `lookup` lowercasing), so it is NOT guarded here.
    if service.chars().next().is_some_and(char::is_uppercase) {
        return json_rpc_response(Resp::err(
            JsonRpcError {
                code: json2_code::NO_METHOD,
                message: format!("rpc: service/method ill-formed: \"{}\"", req.method),
                data: None,
            },
            req.id,
        ));
    }

    let Some(handler) = registry.lookup(service, rpc_method) else {
        return json_rpc_response(Resp::err(
            JsonRpcError {
                code: json2_code::NO_METHOD,
                message: format!("rpc: can't find method {}", req.method),
                data: None,
            },
            req.id,
        ));
    };

    let arg = first_param(req.params);
    match handler(arg).await {
        Ok(result) => json_rpc_response(Resp::ok(result, req.id)),
        // Domain / handler error: HTTP 200 + json2 error body (14 §16.1).
        Err(e) => json_rpc_response(Resp::err(e.to_wire(), req.id)),
    }
}

/// Serializes a [`Resp`] into an HTTP-200 `application/json` response. A
/// serialization failure (unreachable for the value types we construct) falls
/// back to a hand-written `-32603` body so the contract is never violated.
fn json_rpc_response(resp: Resp) -> Response {
    match serde_json::to_vec(&resp) {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json; charset=UTF-8")],
            body,
        )
            .into_response(),
        Err(e) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json; charset=UTF-8")],
            format!(
                "{{\"jsonrpc\":\"2.0\",\"error\":{{\"code\":{},\"message\":{},\"data\":null}},\"id\":null}}",
                json2_code::INTERNAL,
                serde_json::Value::String(e.to_string()),
            ),
        )
            .into_response(),
    }
}

/// Convenience alias so a domain error converts straight into an [`RpcError`]
/// (the gorilla default: `-32000`, message = `to_string()`). Service handlers
/// written by hand can therefore use `?` on any [`std::error::Error`].
impl<E: std::error::Error> From<&E> for RpcError {
    fn from(e: &E) -> Self {
        let wire = e.to_json_rpc();
        Self {
            code: wire.code,
            message: wire.message,
            data: wire.data,
        }
    }
}

#[cfg(test)]
// `serde_json::Value` indexing (`body["error"]["code"]`) returns `Value::Null`
// on a missing key rather than panicking; it is the idiomatic way to assert on
// JSON-RPC response bodies.
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::sync::Arc;

    use ava_api_macros::rpc_service;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use axum::routing::post;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    // A test domain error whose `Display` is the byte-stable client message.
    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("the primary network isn't a subnet")]
        NotASubnet,
    }

    impl From<TestError> for RpcError {
        fn from(e: TestError) -> Self {
            RpcError::server(e.to_string())
        }
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct EmptyArgs {}

    #[derive(Debug, serde::Serialize)]
    struct NodeIdReply {
        #[serde(rename = "nodeID")]
        node_id: String,
    }

    #[derive(Debug, serde::Deserialize)]
    struct EchoArgs {
        value: u64,
    }

    #[derive(Debug, serde::Serialize)]
    struct EchoReply {
        value: u64,
    }

    // The test service exercised through the `#[rpc_service]` macro: the
    // generated `register_rpc` registers `GetNodeID`, `Echo`, and `Fail` under
    // the `info.*` namespace.
    struct InfoService;

    #[rpc_service("info")]
    impl InfoService {
        pub async fn get_node_id(
            &self,
            _args: EmptyArgs,
        ) -> std::result::Result<NodeIdReply, RpcError> {
            Ok(NodeIdReply {
                node_id: "NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg".to_string(),
            })
        }

        pub async fn echo(&self, args: EchoArgs) -> std::result::Result<EchoReply, RpcError> {
            Ok(EchoReply { value: args.value })
        }

        pub async fn fail(&self, _args: EmptyArgs) -> std::result::Result<EmptyArgs, RpcError> {
            Err(TestError::NotASubnet.into())
        }

        // A non-pub / non-async helper on the same block must be ignored by the
        // macro (it does not get registered).
        #[allow(dead_code)]
        fn helper(&self) -> u8 {
            42
        }
    }

    fn registry() -> Arc<ServiceRegistry> {
        let mut reg = ServiceRegistry::new();
        Arc::new(InfoService).register_rpc(&mut reg);
        Arc::new(reg)
    }

    fn router() -> Router {
        Router::new()
            .route("/", post(dispatch).get(dispatch))
            .with_state(registry())
    }

    async fn post_json(body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
            .expect("request");
        let response = router().oneshot(request).await.expect("oneshot");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): gorilla wire shape — request dispatches to info.GetNodeID
    // (case-insensitive method segment, single-element params array); success
    // is {"jsonrpc":"2.0","id":1,"result":{…}}.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn gorilla_wire_shape() {
        // Lowercased method segment ("getNodeID") matches "GetNodeID".
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "info.getNodeID",
            "params": [{}],
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert_eq!(
            body["result"]["nodeID"],
            "NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg"
        );
        assert!(body.get("error").is_none());

        // Exact PascalCase also matches, and the single-element params array is
        // unwrapped into the Args object.
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "id": "abc",
            "method": "info.Echo",
            "params": [{ "value": 99 }],
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], "abc");
        assert_eq!(body["result"]["value"], 99);

        // A *struct{}-style method accepts an empty params array / absent params.
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "info.getNodeID",
            "params": [],
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["result"]["nodeID"],
            "NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg"
        );
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): a handler-returned domain error => HTTP 200 with
    // {code:-32000, message: err.to_string(), data: null} (14 §16.1).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn domain_error_is_minus_32000_http_200() {
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "info.fail",
            "params": [{}],
        }))
        .await;
        // HTTP 200, NOT 400/500 (the json2 nuance).
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], 7);
        assert_eq!(body["error"]["code"], json2_code::SERVER);
        assert_eq!(
            body["error"]["message"],
            "the primary network isn't a subnet"
        );
        // `data` is an explicit null, never absent.
        assert!(body["error"].get("data").is_some());
        assert_eq!(body["error"]["data"], Value::Null);
        assert!(body.get("result").is_none());
    }

    #[tokio::test]
    async fn malformed_json_is_minus_32700() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b"{ not json".to_vec()))
            .expect("request");
        let response = router().oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["error"]["code"], json2_code::PARSE);
        assert_eq!(body["id"], Value::Null);
    }

    #[tokio::test]
    async fn unknown_method_is_minus_32601() {
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "info.doesNotExist",
            "params": [{}],
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], json2_code::NO_METHOD);
    }

    // The uppercase-SERVICE guard (errUppercaseMethod) => -32601 (14 §16.1).
    #[tokio::test]
    async fn uppercase_service_guard_is_minus_32601() {
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "Info.getNodeID",
            "params": [{}],
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], json2_code::NO_METHOD);
    }

    #[tokio::test]
    async fn wrong_version_is_minus_32600() {
        let (status, body) = post_json(json!({
            "jsonrpc": "1.0",
            "id": 1,
            "method": "info.getNodeID",
            "params": [{}],
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], json2_code::INVALID_REQ);
    }

    #[tokio::test]
    async fn bad_params_is_minus_32602() {
        // `value` should be a u64; a string fails to deserialize -> -32602.
        let (status, body) = post_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "info.echo",
            "params": [{ "value": "not-a-number" }],
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], json2_code::BAD_PARAMS);
    }

    // ------------------------------------------------------------------
    // Pre-dispatch transport status codes (14 §16.3): 405 non-POST, 415 bad
    // content-type — real HTTP status codes, NOT a json2 body.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn non_post_is_405() {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/")
            .body(Body::empty())
            .expect("request");
        let response = router().oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn bad_content_type_is_415() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(b"{}".to_vec()))
            .expect("request");
        let response = router().oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn charset_content_type_accepted() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json;charset=UTF-8")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "info.getNodeID",
                    "params": [{}],
                }))
                .expect("serialize"),
            ))
            .expect("request");
        let response = router().oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
    }

    // The macro registers exactly the three pub-async methods (GetNodeID, Echo,
    // Fail) — the non-pub `helper` is not registered.
    #[test]
    fn macro_registers_only_pub_async_methods() {
        let mut reg = ServiceRegistry::new();
        Arc::new(InfoService).register_rpc(&mut reg);
        assert_eq!(reg.len(), 3);
        assert!(reg.lookup("info", "GetNodeID").is_some());
        assert!(reg.lookup("info", "getnodeid").is_some());
        assert!(reg.lookup("info", "Echo").is_some());
        assert!(reg.lookup("info", "Fail").is_some());
        assert!(reg.lookup("info", "helper").is_none());
    }

    #[test]
    fn first_param_unwraps_single_element_array() {
        assert_eq!(first_param(json!([{ "a": 1 }])), json!({ "a": 1 }));
        assert_eq!(first_param(json!([])), Value::Null);
        assert_eq!(first_param(Value::Null), Value::Null);
        // A bare object (non-array) is passed through.
        assert_eq!(first_param(json!({ "a": 1 })), json!({ "a": 1 }));
    }
}
