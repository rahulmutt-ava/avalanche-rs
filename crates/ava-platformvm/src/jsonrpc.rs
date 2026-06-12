// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! A crate-local gorilla-json2 JSON-RPC 2.0 shim over the buffered in-process
//! [`VmHttpService`] seam (M8.22), byte-parity with `ava-api/src/jsonrpc.rs`
//! (specs 12 §3.2, 14 §1.1/§16.1/§16.3).
//!
//! ## Why a local copy exists (M8.22 cycle note)
//!
//! The canonical shim lives in `ava-api`, but `ava-api → ava-config →
//! ava-genesis → ava-platformvm` is a hard package cycle, so this crate cannot
//! depend on `ava-api`. The `#[rpc_service]` registration macro IS shared (the
//! leaf proc-macro crate `ava-api-macros`), so the registered method set
//! cannot drift; only the ~150-line dispatch core is duplicated, with parity
//! tests pinning the exact wire behavior the `ava-api` shim implements
//! (uppercase-method guard, `params[0]` unwrap, json2 error codes, HTTP
//! 200/405/415 split). Consolidating both copies into a shared crate below
//! `ava-config` is a recorded M8.23 follow-up.
//!
//! ## Wire contract (gorilla `rpc/v2` + the avalanchego `utils/json` shim)
//!
//! - **Request** `{"jsonrpc":"2.0","id":1,"method":"<service>.<Method>",
//!   "params":[{…}]}`. The service segment matches case-insensitively; the
//!   method segment's first letter is uppercased and the remainder matched
//!   EXACTLY (an already-uppercase first letter is rejected → `-32601`).
//! - **Success** `{"jsonrpc":"2.0","id":1,"result":{…}}`; **error**
//!   `{"jsonrpc":"2.0","id":1,"error":{"code":…,"message":"…","data":null}}`.
//! - A handler/domain error is **HTTP 200** with a json2 error body; only the
//!   pre-dispatch transport checks use real status codes: `405` (non-POST) and
//!   `415` (unrecognized `Content-Type`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use ava_vm::vm::{VmHttpService, VmRequest, VmResponse};

/// The gorilla json2 error codes (`gorilla/rpc/v2/json2/json.go`).
pub mod json2_code {
    /// `E_PARSE` — the request body is not valid JSON.
    pub const PARSE: i32 = -32700;
    /// `E_INVALID_REQ` — the request envelope is malformed (bad version).
    pub const INVALID_REQ: i32 = -32600;
    /// `E_NO_METHOD` — unknown / ill-formed method.
    pub const NO_METHOD: i32 = -32601;
    /// `E_BAD_PARAMS` — the params failed to decode into the Args object.
    pub const BAD_PARAMS: i32 = -32602;
    /// `E_INTERNAL` — reserved internal error.
    pub const INTERNAL: i32 = -32603;
    /// `E_SERVER` — the gorilla default for any handler-returned error.
    pub const SERVER: i32 = -32000;
}

/// The error a registered JSON-RPC method returns: the full on-wire shape
/// (code / message / data). The common path is [`RpcError::server`] (`-32000`),
/// the gorilla default for a handler-returned Go error (14 §16.1).
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
    /// A generic server error (`-32000`).
    #[must_use]
    pub fn server(message: impl Into<String>) -> Self {
        Self {
            code: json2_code::SERVER,
            message: message.into(),
            data: None,
        }
    }

    /// An invalid-params error (`-32602`): the `params` object failed to
    /// deserialize into the method's `Args`.
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: json2_code::BAD_PARAMS,
            message: message.into(),
            data: None,
        }
    }

    /// A method-not-found error (`-32601`).
    #[must_use]
    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: json2_code::NO_METHOD,
            message: message.into(),
            data: None,
        }
    }

    /// A reserved internal error (`-32603`).
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: json2_code::INTERNAL,
            message: message.into(),
            data: None,
        }
    }

    /// The json2 error object (`data` is an explicit `null` when absent).
    fn to_wire(&self) -> Value {
        json!({
            "code": self.code,
            "message": self.message,
            "data": self.data,
        })
    }
}

/// A registered JSON-RPC method: deserialized `params[0]` in, a JSON value or
/// an [`RpcError`] out. Boxed and `'static` so the registry owns it.
pub type BoxedRpcMethod = Box<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = std::result::Result<Value, RpcError>> + Send>>
        + Send
        + Sync,
>;

/// The dispatch table mapping `"service.Method"` to its handler (gorilla's
/// `serviceMap`). The service segment of the key is lowercased (gorilla
/// lowercases service names); the method segment keeps its exact registered
/// casing (the Go wire name, e.g. `GetStakingAssetID`).
#[derive(Default)]
pub struct ServiceRegistry {
    /// `"<service-lowercased>.<Method-exact>"` -> handler.
    methods: HashMap<String, BoxedRpcMethod>,
}

impl ServiceRegistry {
    /// A new, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `handler` under the gorilla wire name `"<service>.<Method>"`
    /// (the `#[rpc_service]` macro calls this once per `pub async fn`).
    pub fn register<F>(&mut self, wire_method: impl Into<String>, handler: F)
    where
        F: Fn(Value) -> Pin<Box<dyn Future<Output = std::result::Result<Value, RpcError>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let wire = wire_method.into();
        let key = match wire.split_once('.') {
            // Lowercase only the service segment; keep the method segment exact.
            Some((service, method)) => format!("{}.{}", service.to_ascii_lowercase(), method),
            None => wire.to_ascii_lowercase(),
        };
        self.methods.insert(key, Box::new(handler));
    }

    /// Looks up the handler for `service.method`: `service` case-insensitively,
    /// `method` EXACTLY as registered (callers first-letter-normalize it).
    #[must_use]
    pub fn lookup(&self, service: &str, method: &str) -> Option<&BoxedRpcMethod> {
        let key = format!("{}.{}", service.to_ascii_lowercase(), method);
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

/// The gorilla first-letter-uppercasing shim applied to the method segment
/// (`utils/json/codec.go`): empty passes through (fails later as `-32601`);
/// an uppercase first rune is the `errUppercaseMethod` rejection (`None`);
/// otherwise the first letter is uppercased, remainder verbatim.
fn normalize_method(method: &str) -> Option<String> {
    let mut chars = method.chars();
    let Some(first) = chars.next() else {
        return Some(String::new());
    };
    if first.is_uppercase() {
        return None;
    }
    let mut out = String::with_capacity(method.len());
    out.extend(first.to_uppercase());
    out.push_str(chars.as_str());
    Some(out)
}

/// Extracts the `Args` object from `params` (gorilla json2's `ReadRequest`):
/// a single-element array unwraps to element 0; a bare object passes through;
/// an empty array / absent params becomes `null` (a `*struct{}` method
/// accepts it).
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

/// Whether the `Content-Type` selects gorilla's json codec: `application/json`
/// with an optional `charset` parameter, case-insensitive (14 §16.3).
fn is_json_media_type(value: &str) -> bool {
    let media = value.split(';').next().unwrap_or("").trim();
    media.eq_ignore_ascii_case("application/json")
}

/// Builds the HTTP-200 json2 error body.
fn error_body(id: Value, e: &RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "error": e.to_wire(), "id": id })
}

/// The dispatch core: one request body in, the HTTP-200 JSON body out
/// (success or json2 error) per 14 §16.1.
async fn dispatch_body(registry: &ServiceRegistry, body: &[u8]) -> Value {
    // -32700 parse error: the body is not valid JSON / not the envelope shape.
    #[derive(serde::Deserialize)]
    struct Req {
        #[serde(default)]
        jsonrpc: String,
        #[serde(default)]
        method: String,
        #[serde(default)]
        params: Value,
        #[serde(default)]
        id: Value,
    }
    let req: Req = match serde_json::from_slice(body) {
        Ok(req) => req,
        Err(_) => {
            return error_body(
                Value::Null,
                &RpcError {
                    code: json2_code::PARSE,
                    message: "parse error".to_string(),
                    data: None,
                },
            );
        }
    };

    // -32600 invalid request: gorilla only accepts protocol version 2.0.
    if req.jsonrpc != "2.0" {
        return error_body(
            req.id,
            &RpcError {
                code: json2_code::INVALID_REQ,
                message: "invalid request: jsonrpc must be \"2.0\"".to_string(),
                data: None,
            },
        );
    }

    // gorilla splits on the FIRST '.'; a missing '.' never resolves → -32601.
    let Some((service, rpc_method)) = req.method.split_once('.') else {
        return error_body(
            req.id,
            &RpcError::method_not_found(format!("rpc: can't find method {}", req.method)),
        );
    };

    // The uppercase-METHOD guard (`errUppercaseMethod`): an uppercase first
    // rune on the METHOD segment is rejected; otherwise the first letter is
    // uppercased and the remainder matched EXACTLY (14 §16.1).
    let Some(matched_method) = normalize_method(rpc_method) else {
        return error_body(
            req.id,
            &RpcError::method_not_found(format!(
                "rpc: service/method ill-formed: \"{}\"",
                req.method
            )),
        );
    };

    let Some(handler) = registry.lookup(service, &matched_method) else {
        return error_body(
            req.id,
            &RpcError::method_not_found(format!("rpc: can't find method {}", req.method)),
        );
    };

    match handler(first_param(req.params)).await {
        Ok(result) => json!({ "jsonrpc": "2.0", "result": result, "id": req.id }),
        // Domain / handler error: HTTP 200 + json2 error body (14 §16.1).
        Err(e) => error_body(req.id, &e),
    }
}

/// A [`ServiceRegistry`] served through the buffered in-process VM seam, with
/// the gorilla pre-dispatch transport checks (`405` non-POST, `415` bad
/// `Content-Type`; 14 §16.3).
struct RegistryService {
    registry: Arc<ServiceRegistry>,
}

#[async_trait]
impl VmHttpService for RegistryService {
    async fn serve_http(&self, req: VmRequest) -> VmResponse {
        if !req.method.eq_ignore_ascii_case("POST") {
            return VmResponse {
                status: 405,
                headers: vec![(
                    "content-type".to_string(),
                    "text/plain; charset=utf-8".to_string(),
                )],
                body: b"405 must POST\n".to_vec(),
            };
        }
        if !req.header("content-type").is_some_and(is_json_media_type) {
            return VmResponse {
                status: 415,
                headers: vec![(
                    "content-type".to_string(),
                    "text/plain; charset=utf-8".to_string(),
                )],
                body: b"415 unsupported media type\n".to_vec(),
            };
        }
        let reply = dispatch_body(&self.registry, &req.body).await;
        // Serializing a built `Value` cannot fail; the fallback keeps the
        // no-unwrap library convention.
        let body = serde_json::to_vec(&reply).unwrap_or_default();
        VmResponse::ok("application/json; charset=UTF-8", body)
    }
}

/// Wraps a [`ServiceRegistry`] as an in-process VM HTTP handler so
/// `create_handlers` can expose gorilla-parity JSON-RPC mounts (M8.22 / 14 §13).
#[must_use]
pub fn registry_service(registry: Arc<ServiceRegistry>) -> Arc<dyn VmHttpService> {
    Arc::new(RegistryService { registry })
}

#[cfg(test)]
// `serde_json::Value` indexing returns `Value::Null` on a missing key; it is
// the idiomatic way to assert on JSON-RPC bodies (ava-api precedent).
#[allow(clippy::indexing_slicing)]
mod tests {
    //! Parity vectors pinning this shim to the `ava-api/src/jsonrpc.rs`
    //! behavior (the same cases its test module asserts), so the two copies
    //! cannot silently drift (see the module-docs cycle note).

    use ava_api_macros::rpc_service;
    use pretty_assertions::assert_eq;

    use super::*;

    #[derive(Debug, serde::Deserialize)]
    struct EchoArgs {
        value: u64,
    }

    struct TestService;

    #[rpc_service("test")]
    impl TestService {
        #[rpc(name = "GetNodeID")]
        pub async fn get_node_id(&self, _args: Value) -> std::result::Result<Value, RpcError> {
            Ok(json!({ "nodeID": "NodeID-test" }))
        }

        pub async fn echo(&self, args: EchoArgs) -> std::result::Result<Value, RpcError> {
            Ok(json!({ "value": args.value }))
        }

        pub async fn fail(&self, _args: Value) -> std::result::Result<Value, RpcError> {
            Err(RpcError::server("boom"))
        }
    }

    fn registry() -> Arc<ServiceRegistry> {
        let mut reg = ServiceRegistry::new();
        Arc::new(TestService).register_rpc(&mut reg);
        Arc::new(reg)
    }

    async fn post(body: Value) -> Value {
        let svc = registry_service(registry());
        let resp = svc
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: String::new(),
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: serde_json::to_vec(&body).expect("serialize"),
            })
            .await;
        assert_eq!(resp.status, 200, "JSON-RPC always answers HTTP 200");
        serde_json::from_slice(&resp.body).expect("json body")
    }

    // The gorilla envelope: params[0] unwrap, id echo, exact-remainder match.
    #[tokio::test]
    async fn gorilla_wire_shape() {
        let body = post(json!({
            "jsonrpc": "2.0", "id": 1, "method": "test.getNodeID", "params": [{}],
        }))
        .await;
        assert_eq!(body["result"]["nodeID"], "NodeID-test");

        let body = post(json!({
            "jsonrpc": "2.0", "id": "abc", "method": "test.echo",
            "params": [{ "value": 99 }],
        }))
        .await;
        assert_eq!(body["id"], "abc");
        assert_eq!(body["result"]["value"], 99);

        // The acronym override is exact-remainder: getNodeId does NOT match.
        let body = post(json!({
            "jsonrpc": "2.0", "id": 2, "method": "test.getNodeId", "params": [{}],
        }))
        .await;
        assert_eq!(body["error"]["code"], json2_code::NO_METHOD);
    }

    // Uppercase-METHOD guard + case-insensitive SERVICE (utils/json/codec.go).
    #[tokio::test]
    async fn uppercase_method_guard() {
        let body = post(json!({
            "jsonrpc": "2.0", "id": 1, "method": "test.GetNodeID", "params": [{}],
        }))
        .await;
        assert_eq!(body["error"]["code"], json2_code::NO_METHOD);
        assert_eq!(
            body["error"]["message"],
            "rpc: service/method ill-formed: \"test.GetNodeID\""
        );

        let body = post(json!({
            "jsonrpc": "2.0", "id": 2, "method": "Test.getNodeID", "params": [{}],
        }))
        .await;
        assert_eq!(body["result"]["nodeID"], "NodeID-test");
    }

    // Domain error ⇒ HTTP 200, -32000, data explicit null (14 §16.1).
    #[tokio::test]
    async fn domain_error_is_minus_32000_http_200() {
        let body = post(json!({
            "jsonrpc": "2.0", "id": 7, "method": "test.fail", "params": [{}],
        }))
        .await;
        assert_eq!(body["error"]["code"], json2_code::SERVER);
        assert_eq!(body["error"]["message"], "boom");
        assert!(body["error"].get("data").is_some(), "data explicit");
        assert_eq!(body["error"]["data"], Value::Null);
    }

    // -32700 / -32600 / -32602 envelope errors.
    #[tokio::test]
    async fn envelope_error_codes() {
        let svc = registry_service(registry());
        let resp = svc
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: String::new(),
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: b"{ not json".to_vec(),
            })
            .await;
        let body: Value = serde_json::from_slice(&resp.body).expect("json");
        assert_eq!(body["error"]["code"], json2_code::PARSE);
        assert_eq!(body["id"], Value::Null);

        let body = post(json!({
            "jsonrpc": "1.0", "id": 1, "method": "test.echo", "params": [{}],
        }))
        .await;
        assert_eq!(body["error"]["code"], json2_code::INVALID_REQ);

        let body = post(json!({
            "jsonrpc": "2.0", "id": 1, "method": "test.echo",
            "params": [{ "value": "not-a-number" }],
        }))
        .await;
        assert_eq!(body["error"]["code"], json2_code::BAD_PARAMS);
    }

    // Pre-dispatch transport checks: 405 non-POST, 415 bad content-type.
    #[tokio::test]
    async fn transport_status_codes() {
        let svc = registry_service(registry());
        let resp = svc
            .serve_http(VmRequest {
                method: "GET".to_string(),
                uri: String::new(),
                headers: Vec::new(),
                body: Vec::new(),
            })
            .await;
        assert_eq!(resp.status, 405, "non-POST is rejected pre-dispatch");

        let resp = svc
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: String::new(),
                headers: vec![("content-type".to_string(), "text/plain".to_string())],
                body: b"{}".to_vec(),
            })
            .await;
        assert_eq!(resp.status, 415, "bad content-type is rejected pre-dispatch");

        let resp = svc
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: String::new(),
                headers: vec![(
                    "content-type".to_string(),
                    "application/json;charset=UTF-8".to_string(),
                )],
                body: serde_json::to_vec(&json!({
                    "jsonrpc": "2.0", "id": 1, "method": "test.getNodeID", "params": [{}],
                }))
                .expect("serialize"),
            })
            .await;
        assert_eq!(resp.status, 200, "charset parameter accepted");
    }
}
