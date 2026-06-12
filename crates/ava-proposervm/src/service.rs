// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The proposervm JSON-RPC API service (Go `vms/proposervm/service.go`
//! `jsonrpcService`, mounted by `CreateHandlers` at `/proposervm`,
//! `vm.go:255-280`).
//!
//! Two methods, registered under the gorilla service name `proposervm`:
//!
//! - `proposervm.getProposedHeight` → `{"height": <json.Uint64>}` (Go reply
//!   type `api.GetHeightResponse`): the P-Chain height a child block proposed
//!   right now would embed (`blk.selectChildPChainHeight`).
//! - `proposervm.getCurrentEpoch` → `{"number","startTime","pChainHeight"}`
//!   (all `json.Uint64`, Go `GetEpochResponse`, `service.go:111-115`): the
//!   preferred block's ACP-181 epoch (`vm.getCurrentEpoch`).
//!
//! The service is transport-free over the [`ProposerApi`] data seam; the VM
//! provides the live implementation (`vm.rs`), and the same seam backs the
//! Connect transport ([`crate::connect`], Go `connectrpcService`).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize, Serializer};

use ava_api::{RpcError, ServiceRegistry, rpc_service};

use crate::block::Epoch;

/// The errors the [`ProposerApi`] data seam surfaces. The `Display` strings
/// mirror the Go wrap sites byte-for-byte (`service.go`):
/// `jsonrpcService.GetProposedHeight` wraps with "failed to get preferred
/// block" / "failed to get child p-chain height", and `vm.getCurrentEpoch`
/// wraps with "couldn't get preferred block" / "couldn't get preferred block
/// epoch".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// `GetProposedHeight`: the preferred block could not be resolved.
    #[error("failed to get preferred block: {0}")]
    PreferredBlock(String),
    /// `GetProposedHeight`: the child P-Chain height could not be computed.
    #[error("failed to get child p-chain height: {0}")]
    ChildPChainHeight(String),
    /// `getCurrentEpoch`: the preferred block could not be resolved.
    #[error("couldn't get preferred block: {0}")]
    EpochPreferredBlock(String),
}

/// The transport-free data seam behind both proposervm API transports
/// (JSON-RPC here, Connect in [`crate::connect`]). The VM implements it over
/// its preferred-block / epoch state (Go reads the same state under
/// `vm.ctx.Lock`).
#[async_trait]
pub trait ProposerApi: Send + Sync {
    /// The P-Chain height a child of the preferred block proposed right now
    /// would embed (Go `blk.selectChildPChainHeight`, `service.go:86-108`).
    async fn proposed_height(&self) -> Result<u64, ApiError>;

    /// The ACP-181 epoch a child of the preferred block proposed right now
    /// would belong to (Go `vm.getCurrentEpoch`, `service.go:137-167`).
    async fn current_epoch(&self) -> Result<Epoch, ApiError>;
}

/// Serialize a `u64` as a quoted decimal string (Go `json.Uint64`).
fn serialize_u64<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

/// Go `api.GetHeightResponse` — `{"height": <json.Uint64>}`.
#[derive(Debug, Clone, Serialize)]
pub struct GetHeightResponse {
    /// The proposed P-Chain height (`json.Uint64` ⇒ quoted string).
    #[serde(serialize_with = "serialize_u64")]
    pub height: u64,
}

/// Go `GetEpochResponse` (`service.go:111-115`) — all fields `json.Uint64`.
#[derive(Debug, Clone, Serialize)]
pub struct GetEpochResponse {
    /// `number` — the epoch number (`json.Uint64` ⇒ quoted string).
    #[serde(serialize_with = "serialize_u64")]
    pub number: u64,
    /// `startTime` — the epoch start time, Unix seconds. Go converts the
    /// `int64` with `json.Uint64(epoch.StartTime)` (two's-complement wrap);
    /// `as u64` mirrors that exactly.
    #[serde(rename = "startTime", serialize_with = "serialize_u64")]
    pub start_time: u64,
    /// `pChainHeight` — the epoch's sealed P-Chain height (`json.Uint64`).
    #[serde(rename = "pChainHeight", serialize_with = "serialize_u64")]
    pub p_chain_height: u64,
}

/// The empty args object (Go `*struct{}`): `[]` / absent / `[{}]` all accept.
#[derive(Debug, Default, Deserialize)]
pub struct EmptyArgs {}

/// The JSON-RPC service (Go `jsonrpcService`), generic only through the boxed
/// [`ProposerApi`] seam so the `#[rpc_service]` registration stays concrete.
pub struct Service {
    api: Arc<dyn ProposerApi>,
}

impl Service {
    /// Builds the service over the VM's [`ProposerApi`] seam.
    #[must_use]
    pub fn new(api: Arc<dyn ProposerApi>) -> Self {
        Self { api }
    }
}

#[rpc_service("proposervm")]
impl Service {
    /// `proposervm.getProposedHeight` (Go `jsonrpcService.GetProposedHeight`,
    /// `service.go:86`). The snake_case ident pascalizes to the exact Go wire
    /// name `GetProposedHeight` (no acronym override needed).
    ///
    /// # Errors
    /// `-32000` carrying the Go-parity message on a data-seam failure.
    pub async fn get_proposed_height(
        &self,
        _args: EmptyArgs,
    ) -> Result<GetHeightResponse, RpcError> {
        let height = self
            .api
            .proposed_height()
            .await
            .map_err(|e| RpcError::server(e.to_string()))?;
        Ok(GetHeightResponse { height })
    }

    /// `proposervm.getCurrentEpoch` (Go `jsonrpcService.GetCurrentEpoch`,
    /// `service.go:117`).
    ///
    /// # Errors
    /// `-32000` with Go's "couldn't get current epoch: …" wrap on failure.
    pub async fn get_current_epoch(&self, _args: EmptyArgs) -> Result<GetEpochResponse, RpcError> {
        let epoch = self
            .api
            .current_epoch()
            .await
            .map_err(|e| RpcError::server(format!("couldn't get current epoch: {e}")))?;
        Ok(GetEpochResponse {
            number: epoch.number,
            // Go: `json.Uint64(epoch.StartTime)` — int64 → uint64 wrap.
            start_time: epoch.start_time as u64,
            p_chain_height: epoch.p_chain_height,
        })
    }
}

/// Builds the registry serving exactly the two proposervm methods (the body of
/// Go's `server.RegisterService(&jsonrpcService{vm}, "proposervm")`,
/// `vm.go:270`).
#[must_use]
pub fn registry(api: Arc<dyn ProposerApi>) -> ServiceRegistry {
    let mut registry = ServiceRegistry::new();
    Arc::new(Service::new(api)).register_rpc(&mut registry);
    registry
}

#[cfg(test)]
// `serde_json::Value` indexing returns `Value::Null` on a missing key rather
// than panicking; it is the idiomatic way to assert on JSON-RPC bodies.
#[allow(clippy::indexing_slicing)]
mod tests {
    use ava_api::registry_service;
    use ava_vm::vm::VmRequest;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

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
            height: Ok(1337),
            epoch: Ok(Epoch {
                p_chain_height: 7,
                number: 3,
                start_time: 2_000,
            }),
        })
    }

    async fn post(api: Arc<dyn ProposerApi>, body: Value) -> Value {
        let service = registry_service(Arc::new(registry(api)));
        let resp = service
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: "/proposervm".to_string(),
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: serde_json::to_vec(&body).expect("serialize"),
            })
            .await;
        assert_eq!(resp.status, 200, "JSON-RPC always answers HTTP 200");
        serde_json::from_slice(&resp.body).expect("json body")
    }

    // TDD target 1a: the registered method set is EXACTLY
    // {GetProposedHeight, GetCurrentEpoch} under service "proposervm".
    #[test]
    fn method_set_matches_go() {
        let reg = registry(fixed());
        assert_eq!(reg.len(), 2, "exactly two methods registered");
        assert!(reg.lookup("proposervm", "GetProposedHeight").is_some());
        assert!(reg.lookup("proposervm", "GetCurrentEpoch").is_some());
        // Exact-remainder matching: no stray casings.
        assert!(reg.lookup("proposervm", "getProposedHeight").is_none());
    }

    // TDD target 1b: gorilla wire envelope; json.Uint64 ⇒ quoted string.
    #[tokio::test]
    async fn get_proposed_height_wire_shape() {
        let body = post(
            fixed(),
            json!({
                "jsonrpc": "2.0",
                "method": "proposervm.getProposedHeight",
                "params": [{}],
                "id": 1,
            }),
        )
        .await;
        assert_eq!(
            body,
            json!({
                "jsonrpc": "2.0",
                "result": { "height": "1337" },
                "id": 1,
            }),
            "proposervm.getProposedHeight envelope"
        );
    }

    // TDD target 2: GetCurrentEpoch reply shape — number/startTime/pChainHeight
    // all json.Uint64 strings (Go GetEpochResponse, service.go:111-115).
    #[tokio::test]
    async fn get_current_epoch_wire_shape() {
        let body = post(
            fixed(),
            json!({
                "jsonrpc": "2.0",
                "method": "proposervm.getCurrentEpoch",
                "params": [{}],
                "id": 2,
            }),
        )
        .await;
        assert_eq!(
            body,
            json!({
                "jsonrpc": "2.0",
                "result": {
                    "number": "3",
                    "startTime": "2000",
                    "pChainHeight": "7",
                },
                "id": 2,
            }),
            "proposervm.getCurrentEpoch envelope"
        );
    }

    // Domain errors surface as -32000 with the Go-parity message (HTTP 200).
    #[tokio::test]
    async fn errors_carry_go_messages() {
        let api: Arc<dyn ProposerApi> = Arc::new(FixedApi {
            height: Err(ApiError::PreferredBlock("not found".to_string())),
            epoch: Err(ApiError::EpochPreferredBlock("not found".to_string())),
        });

        let body = post(
            Arc::clone(&api),
            json!({
                "jsonrpc": "2.0",
                "method": "proposervm.getProposedHeight",
                "params": [{}],
                "id": 1,
            }),
        )
        .await;
        assert_eq!(body["error"]["code"], -32000, "gorilla server error code");
        assert_eq!(
            body["error"]["message"], "failed to get preferred block: not found",
            "Go service.go:97 wrap"
        );

        let body = post(
            api,
            json!({
                "jsonrpc": "2.0",
                "method": "proposervm.getCurrentEpoch",
                "params": [{}],
                "id": 2,
            }),
        )
        .await;
        assert_eq!(
            body["error"]["message"],
            "couldn't get current epoch: couldn't get preferred block: not found",
            "Go service.go:124 + getCurrentEpoch wrap"
        );
    }

    // Negative StartTime wraps like Go's json.Uint64(int64) conversion.
    #[tokio::test]
    async fn start_time_wraps_like_go() {
        let api: Arc<dyn ProposerApi> = Arc::new(FixedApi {
            height: Ok(0),
            epoch: Ok(Epoch {
                p_chain_height: 0,
                number: 1,
                start_time: -1,
            }),
        });
        let body = post(
            api,
            json!({
                "jsonrpc": "2.0",
                "method": "proposervm.getCurrentEpoch",
                "params": [{}],
                "id": 3,
            }),
        )
        .await;
        assert_eq!(
            body["result"]["startTime"],
            u64::MAX.to_string(),
            "uint64(int64(-1)) two's-complement wrap"
        );
    }
}
