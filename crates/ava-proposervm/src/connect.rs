// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The proposervm Connect transport + header-route composition (Go
//! `vms/proposervm/vm.go:282-311` `NewHTTPHandler` + `service.go`
//! `connectrpcService`).
//!
//! Serves the two `proposervm.ProposerVM` procedures as **Connect-unary**
//! handlers (plan 12 §3.7 / 14 §11.1):
//!
//! - `POST /proposervm.ProposerVM/GetProposedHeight`
//! - `POST /proposervm.ProposerVM/GetCurrentEpoch`
//!
//! with `content-type: application/json` (proto3 JSON: 64-bit ints as quoted
//! strings, lowerCamelCase names, zero-valued fields omitted — Go connectrpc
//! marshals with `protojson` defaults, `connect-go codec.go protoJSONCodec`)
//! or `content-type: application/proto` (prost binary). The handler is
//! hand-rolled over the buffered [`VmHttpService`] seam rather than embedding
//! tonic/tonic-web: there are exactly two empty-request methods, the seam is
//! buffered (not a tower service), and no in-process gRPC client is needed —
//! the Connect-unary wire contract is implemented and tested directly.
//!
//! gRPC reflection (`grpcreflect.NewStaticReflector`, Go `vm.go:292`) is a
//! recorded **deferral** (`tests/PORTING.md`).
//!
//! Composition with the inner VM's header handler mirrors Go's multi-valued
//! `Avalanche-Api-Route` dispatch (`vm.go:297-309`):
//! `len(route) < 2` → inner handler (404 if none); `len == 2 &&
//! route[1] == "proposervm"` → the Connect mux; anything else → 404.

use std::sync::Arc;

use async_trait::async_trait;
use prost::Message;
use serde_json::{Map, Value, json};

use ava_api::HTTP_HEADER_ROUTE;
use ava_vm::vm::{VmHttpService, VmRequest, VmResponse};

use crate::pb;
use crate::service::{ApiError, ProposerApi};

/// The second `Avalanche-Api-Route` value selecting the proposervm Connect mux
/// (Go `HTTPHeaderRoute`, `vm.go:47`).
pub const HTTP_HEADER_ROUTE_VALUE: &str = "proposervm";

/// The Connect procedure path for `GetProposedHeight`.
pub const GET_PROPOSED_HEIGHT_PATH: &str = "/proposervm.ProposerVM/GetProposedHeight";
/// The Connect procedure path for `GetCurrentEpoch`.
pub const GET_CURRENT_EPOCH_PATH: &str = "/proposervm.ProposerVM/GetCurrentEpoch";

/// The request/response codec selected by the unary `content-type`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Codec {
    /// `application/json` — proto3 JSON (`protojson` semantics).
    Json,
    /// `application/proto` — binary protobuf.
    Proto,
}

impl Codec {
    fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Proto => "application/proto",
        }
    }
}

/// The `proposervm.ProposerVM` Connect mux (Go `proposerMux`, `vm.go:289`).
pub struct ConnectService {
    api: Arc<dyn ProposerApi>,
}

impl ConnectService {
    /// Builds the Connect mux over the VM's [`ProposerApi`] seam.
    #[must_use]
    pub fn new(api: Arc<dyn ProposerApi>) -> Self {
        Self { api }
    }

    async fn get_proposed_height(&self, codec: Codec) -> VmResponse {
        match self.api.proposed_height().await {
            Ok(height) => {
                let reply = pb::GetProposedHeightReply { height };
                match codec {
                    // proto3 JSON: uint64 ⇒ quoted string, zero ⇒ omitted
                    // (protojson `EmitUnpopulated=false`, the connect-go
                    // default).
                    Codec::Json => {
                        let mut obj = Map::new();
                        if reply.height != 0 {
                            obj.insert("height".to_string(), json!(reply.height.to_string()));
                        }
                        json_message_response(&Value::Object(obj))
                    }
                    Codec::Proto => proto_response(reply.encode_to_vec()),
                }
            }
            Err(e) => connect_error(&e),
        }
    }

    async fn get_current_epoch(&self, codec: Codec) -> VmResponse {
        match self.api.current_epoch().await {
            Ok(epoch) => {
                let reply = pb::GetCurrentEpochReply {
                    number: epoch.number,
                    p_chain_height: epoch.p_chain_height,
                    start_time: epoch.start_time,
                };
                match codec {
                    Codec::Json => {
                        let mut obj = Map::new();
                        if reply.number != 0 {
                            obj.insert("number".to_string(), json!(reply.number.to_string()));
                        }
                        if reply.p_chain_height != 0 {
                            obj.insert(
                                "pChainHeight".to_string(),
                                json!(reply.p_chain_height.to_string()),
                            );
                        }
                        if reply.start_time != 0 {
                            obj.insert(
                                "startTime".to_string(),
                                json!(reply.start_time.to_string()),
                            );
                        }
                        json_message_response(&Value::Object(obj))
                    }
                    Codec::Proto => proto_response(reply.encode_to_vec()),
                }
            }
            Err(e) => connect_error(&e),
        }
    }
}

#[async_trait]
impl VmHttpService for ConnectService {
    async fn serve_http(&self, req: VmRequest) -> VmResponse {
        // The mux matches the path exactly (query excluded). An unknown path —
        // including the skipped gRPC-reflection service — is Go's
        // `http.NotFound` (`mux` fallthrough).
        let path = req.uri.split('?').next().unwrap_or("");
        let is_height = path == GET_PROPOSED_HEIGHT_PATH;
        let is_epoch = path == GET_CURRENT_EPOCH_PATH;
        if !is_height && !is_epoch {
            return not_found();
        }

        // Connect unary is POST-only: 405 + Allow (connect-go
        // protocol_connect.go).
        if !req.method.eq_ignore_ascii_case("POST") {
            let mut resp = VmResponse::status_only(405);
            resp.headers.push(("allow".to_string(), "POST".to_string()));
            return resp;
        }

        // Codec selection by content-type (charset parameters tolerated);
        // anything else is 415 per the Connect protocol.
        let media = req
            .header("content-type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let codec = match media.as_str() {
            "application/json" => Codec::Json,
            "application/proto" => Codec::Proto,
            _ => {
                let mut resp = VmResponse::status_only(415);
                resp.headers.push((
                    "accept-post".to_string(),
                    "application/json, application/proto".to_string(),
                ));
                return resp;
            }
        };

        // Decode the (empty-message) request. JSON must be valid (unknown
        // fields are discarded — connect-go unmarshals with
        // `protojson.UnmarshalOptions{DiscardUnknown: true}`); proto bytes
        // must decode (unknown fields are skipped by prost).
        match codec {
            Codec::Json => {
                if !req.body.is_empty()
                    && serde_json::from_slice::<Value>(&req.body)
                        .map(|v| !v.is_object())
                        .unwrap_or(true)
                {
                    return invalid_argument("invalid request message");
                }
            }
            Codec::Proto => {
                if pb::GetProposedHeightRequest::decode(req.body.as_slice()).is_err() {
                    return invalid_argument("invalid request message");
                }
            }
        }

        if is_height {
            self.get_proposed_height(codec).await
        } else {
            self.get_current_epoch(codec).await
        }
    }
}

/// A `200` Connect-unary JSON message response.
fn json_message_response(value: &Value) -> VmResponse {
    VmResponse::ok(
        Codec::Json.content_type(),
        serde_json::to_vec(value).unwrap_or_default(),
    )
}

/// A `200` Connect-unary binary-proto message response.
fn proto_response(body: Vec<u8>) -> VmResponse {
    VmResponse::ok(Codec::Proto.content_type(), body)
}

/// A Connect unary error: always a JSON `{"code","message"}` body (regardless
/// of the request codec) with the code's HTTP status. A plain handler error is
/// `unknown` ⇒ HTTP 500 (connect-go wraps non-`*connect.Error`s as
/// `CodeUnknown`).
fn connect_error(e: &ApiError) -> VmResponse {
    let body = json!({ "code": "unknown", "message": e.to_string() });
    VmResponse {
        status: 500,
        headers: vec![("content-type".to_string(), "application/json".to_string())],
        body: serde_json::to_vec(&body).unwrap_or_default(),
    }
}

/// A Connect `invalid_argument` error (HTTP 400).
fn invalid_argument(message: &str) -> VmResponse {
    let body = json!({ "code": "invalid_argument", "message": message });
    VmResponse {
        status: 400,
        headers: vec![("content-type".to_string(), "application/json".to_string())],
        body: serde_json::to_vec(&body).unwrap_or_default(),
    }
}

/// Go `http.NotFound`: `404` with the canonical text body.
fn not_found() -> VmResponse {
    VmResponse {
        status: 404,
        headers: vec![(
            "content-type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        )],
        body: b"404 page not found\n".to_vec(),
    }
}

/// The composed header-route handler returned by
/// `ProposerVm::new_http_handler` (Go `vm.go:296-309`): dispatches on the
/// **multi-valued** `Avalanche-Api-Route` header.
pub struct HeaderRouteService {
    /// The inner VM's header-route handler (`nil`-able in Go).
    inner: Option<Arc<dyn VmHttpService>>,
    /// The proposervm Connect mux.
    proposer: ConnectService,
}

impl HeaderRouteService {
    /// Composes the inner VM's header handler (if any) with the proposervm
    /// Connect mux.
    #[must_use]
    pub fn new(inner: Option<Arc<dyn VmHttpService>>, api: Arc<dyn ProposerApi>) -> Self {
        Self {
            inner,
            proposer: ConnectService::new(api),
        }
    }
}

#[async_trait]
impl VmHttpService for HeaderRouteService {
    async fn serve_http(&self, req: VmRequest) -> VmResponse {
        // Go reads `r.Header[server.HTTPHeaderRoute]` — every value of the
        // repeated header, in order (`vm.go:297`); the buffered seam preserves
        // multiplicity (`VmRequest::header_values`).
        let route: Vec<&str> = req.header_values(HTTP_HEADER_ROUTE).collect();
        match route.as_slice() {
            // `len(route) < 2 && innerHandler != nil` → the inner VM.
            [] | [_] if self.inner.is_some() => match &self.inner {
                Some(inner) => inner.serve_http(req).await,
                None => VmResponse::status_only(404),
            },
            // `len(route) == 2 && route[1] == HTTPHeaderRoute` → the mux.
            [_, second] if *second == HTTP_HEADER_ROUTE_VALUE => {
                self.proposer.serve_http(req).await
            }
            // Everything else (including short routes with no inner handler).
            _ => VmResponse::status_only(404),
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::block::Epoch;

    use super::*;

    /// A fixed-data [`ProposerApi`] stub.
    struct FixedApi {
        height: Result<u64, ApiError>,
        epoch: Result<Epoch, ApiError>,
    }

    #[async_trait]
    impl ProposerApi for FixedApi {
        async fn proposed_height(&self) -> Result<u64, ApiError> {
            self.height.clone()
        }
        async fn current_epoch(&self) -> Result<Epoch, ApiError> {
            self.epoch.clone()
        }
    }

    fn fixed() -> Arc<dyn ProposerApi> {
        Arc::new(FixedApi {
            height: Ok(123),
            epoch: Ok(Epoch {
                p_chain_height: 7,
                number: 3,
                start_time: 2_000,
            }),
        })
    }

    fn post(path: &str, content_type: &str, body: Vec<u8>) -> VmRequest {
        VmRequest {
            method: "POST".to_string(),
            uri: path.to_string(),
            headers: vec![("content-type".to_string(), content_type.to_string())],
            body,
        }
    }

    // TDD target 4: Connect-unary JSON — proto3 JSON maps 64-bit ints to
    // STRINGS (proto3 JSON mapping; Go connectrpc marshals via protojson).
    #[tokio::test]
    async fn connect_json_get_proposed_height() {
        let svc = ConnectService::new(fixed());
        let resp = svc
            .serve_http(post(
                GET_PROPOSED_HEIGHT_PATH,
                "application/json",
                b"{}".to_vec(),
            ))
            .await;
        assert_eq!(resp.status, 200, "Connect unary success");
        let body: Value = serde_json::from_slice(&resp.body).expect("json");
        assert_eq!(
            body,
            json!({ "height": "123" }),
            "uint64 serializes as a quoted string (proto3 JSON)"
        );
    }

    #[tokio::test]
    async fn connect_json_get_current_epoch() {
        let svc = ConnectService::new(fixed());
        let resp = svc
            .serve_http(post(
                GET_CURRENT_EPOCH_PATH,
                "application/json",
                b"{}".to_vec(),
            ))
            .await;
        assert_eq!(resp.status, 200);
        let body: Value = serde_json::from_slice(&resp.body).expect("json");
        assert_eq!(
            body,
            json!({
                "number": "3",
                "pChainHeight": "7",
                "startTime": "2000",
            }),
            "lowerCamelCase names + quoted 64-bit ints"
        );
    }

    // protojson omits zero-valued fields (EmitUnpopulated=false, the
    // connect-go default).
    #[tokio::test]
    async fn connect_json_omits_zero_fields() {
        let api: Arc<dyn ProposerApi> = Arc::new(FixedApi {
            height: Ok(0),
            epoch: Ok(Epoch::default()),
        });
        let svc = ConnectService::new(api);

        let resp = svc
            .serve_http(post(
                GET_PROPOSED_HEIGHT_PATH,
                "application/json",
                b"{}".to_vec(),
            ))
            .await;
        let body: Value = serde_json::from_slice(&resp.body).expect("json");
        assert_eq!(body, json!({}), "zero height omitted");

        let resp = svc
            .serve_http(post(
                GET_CURRENT_EPOCH_PATH,
                "application/json",
                b"{}".to_vec(),
            ))
            .await;
        let body: Value = serde_json::from_slice(&resp.body).expect("json");
        assert_eq!(body, json!({}), "zero epoch fields omitted");
    }

    // application/proto round-trips through prost.
    #[tokio::test]
    async fn connect_proto_round_trip() {
        let svc = ConnectService::new(fixed());

        let resp = svc
            .serve_http(post(
                GET_PROPOSED_HEIGHT_PATH,
                "application/proto",
                Vec::new(),
            ))
            .await;
        assert_eq!(resp.status, 200);
        assert_eq!(
            resp.headers,
            vec![("content-type".to_string(), "application/proto".to_string())]
        );
        let reply = pb::GetProposedHeightReply::decode(resp.body.as_slice()).expect("decode");
        assert_eq!(reply.height, 123, "proto reply height");

        let resp = svc
            .serve_http(post(GET_CURRENT_EPOCH_PATH, "application/proto", Vec::new()))
            .await;
        let reply = pb::GetCurrentEpochReply::decode(resp.body.as_slice()).expect("decode");
        assert_eq!(
            (reply.number, reply.p_chain_height, reply.start_time),
            (3, 7, 2_000),
            "proto reply epoch"
        );
    }

    // Connect transport errors: 404 unknown procedure, 405 non-POST, 415 bad
    // content-type, 500 unknown on a handler error.
    #[tokio::test]
    async fn connect_transport_errors() {
        let svc = ConnectService::new(fixed());

        let resp = svc
            .serve_http(post("/proposervm.ProposerVM/Nope", "application/json", vec![]))
            .await;
        assert_eq!(resp.status, 404, "unknown procedure");

        let mut req = post(GET_PROPOSED_HEIGHT_PATH, "application/json", vec![]);
        req.method = "GET".to_string();
        let resp = svc.serve_http(req).await;
        assert_eq!(resp.status, 405, "non-POST unary");

        let resp = svc
            .serve_http(post(GET_PROPOSED_HEIGHT_PATH, "text/plain", vec![]))
            .await;
        assert_eq!(resp.status, 415, "unsupported codec");

        let api: Arc<dyn ProposerApi> = Arc::new(FixedApi {
            height: Err(ApiError::PreferredBlock("not found".to_string())),
            epoch: Err(ApiError::EpochPreferredBlock("not found".to_string())),
        });
        let svc = ConnectService::new(api);
        let resp = svc
            .serve_http(post(
                GET_PROPOSED_HEIGHT_PATH,
                "application/json",
                b"{}".to_vec(),
            ))
            .await;
        assert_eq!(resp.status, 500, "unknown-code error status");
        let body: Value = serde_json::from_slice(&resp.body).expect("json");
        assert_eq!(body["code"], "unknown", "Connect error code");
        assert_eq!(
            body["message"],
            "failed to get preferred block: not found",
            "Go connectrpcService wrap (service.go:46)"
        );
    }

    /// A tagged inner header handler.
    struct Tagged(&'static str);

    #[async_trait]
    impl VmHttpService for Tagged {
        async fn serve_http(&self, _req: VmRequest) -> VmResponse {
            VmResponse::ok("text/plain", self.0.as_bytes().to_vec())
        }
    }

    fn with_route(values: &[&str]) -> VmRequest {
        VmRequest {
            method: "POST".to_string(),
            uri: GET_PROPOSED_HEIGHT_PATH.to_string(),
            headers: values
                .iter()
                .map(|v| (HTTP_HEADER_ROUTE.to_string(), (*v).to_string()))
                .chain(std::iter::once((
                    "content-type".to_string(),
                    "application/json".to_string(),
                )))
                .collect(),
            body: b"{}".to_vec(),
        }
    }

    // TDD target 3: the Go vm.go:297-309 header composition table.
    #[tokio::test]
    async fn header_route_composition() {
        let svc = HeaderRouteService::new(Some(Arc::new(Tagged("inner"))), fixed());

        // No route header -> inner handler.
        let resp = svc.serve_http(with_route(&[])).await;
        assert_eq!(
            (resp.status, resp.body.as_slice()),
            (200, b"inner".as_slice()),
            "len(route) == 0 -> inner"
        );

        // One value -> still the inner handler.
        let resp = svc.serve_http(with_route(&["chain-id"])).await;
        assert_eq!(
            (resp.status, resp.body.as_slice()),
            (200, b"inner".as_slice()),
            "len(route) == 1 -> inner"
        );

        // Two values, second == "proposervm" -> the Connect mux.
        let resp = svc.serve_http(with_route(&["chain-id", "proposervm"])).await;
        assert_eq!(resp.status, 200, "len(route) == 2 + proposervm -> mux");
        let body: Value = serde_json::from_slice(&resp.body).expect("json");
        assert_eq!(body, json!({ "height": "123" }), "served by the mux");

        // Two values, second != "proposervm" -> 404.
        let resp = svc.serve_http(with_route(&["chain-id", "elsewhere"])).await;
        assert_eq!(resp.status, 404, "len == 2, wrong second value -> 404");

        // Three values -> 404 (Go's default case).
        let resp = svc
            .serve_http(with_route(&["a", "proposervm", "c"]))
            .await;
        assert_eq!(resp.status, 404, "len(route) > 2 -> 404");

        // Short route with NO inner handler -> 404 (Go: case guard fails
        // through to default).
        let svc = HeaderRouteService::new(None, fixed());
        let resp = svc.serve_http(with_route(&[])).await;
        assert_eq!(resp.status, 404, "no inner handler -> 404");

        // ... but the mux route still works without an inner handler.
        let resp = svc.serve_http(with_route(&["chain-id", "proposervm"])).await;
        assert_eq!(resp.status, 200, "mux independent of the inner handler");
    }
}
