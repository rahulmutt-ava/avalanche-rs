// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! In-process HTTP bridges for the C-Chain RPC handlers (M8.22).
//!
//! Bridges the existing direct-`serde_json` handler bodies
//! ([`EthRpc`]/[`AvaxRpc`]/[`AdminRpc`], M6.23/M6.24) onto the buffered
//! [`VmHttpService`] seam so [`crate::vm::EvmVm`]'s `create_handlers` can
//! return real handlers (Go `coreth/plugin/evm/vm.go:1029` `CreateHandlers` +
//! the atomic wrapper `atomic/vm/vm.go:337`).
//!
//! Two wire protocols, exactly as coreth serves them:
//!
//! - **`/rpc` + `/ws`** — the Ethereum JSON-RPC 2.0 envelope (geth
//!   `rpc.Server`, coreth `vm.go:1067-1068`): positional `params`, batch
//!   arrays, `eth_*`/`debug_*` method names. [`EthHttpService`] dispatches on
//!   the method string to the [`EthRpc`] bodies. The `/ws` mount serves the
//!   SAME dispatch: the node's WS adapter bridges frames as buffered POSTs
//!   (see `ava-api`'s register adapters), which is the correct wire behavior
//!   for request/response calls; `eth_subscribe` push streams are a
//!   documented deferral.
//! - **`/avax` + `/admin`** — the gorilla-json2 envelope (avalanchego
//!   `utils/rpc.NewHandler`): object params under a service-qualified method
//!   (`avax.issueTx`). [`avax_service`]/[`admin_service`] register thin serde
//!   wrappers into `ava-api`'s [`ServiceRegistry`] (the proposervm M8.22
//!   precedent) with the exact Go wire names (`GetUTXOs`, `StartCPUProfiler`,
//!   …; acronyms need `#[rpc(name = …)]`, exact-remainder matching).

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use ruint::aliases::U256 as RuintU256;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use ava_api::{RpcError, ServiceRegistry, registry_service, rpc_service};
use ava_evm_reth::{Address, B256};
use ava_types::id::Id;
use ava_vm::vm::{VmHttpService, VmRequest, VmResponse};

use crate::rpc::admin::AdminRpc;
use crate::rpc::avax::{AvaxRpc, IssueTxArgs};
use crate::rpc::eth::{BlockTag, CallRequest, EthRpc, FeeHistoryArgs};

// ─── Extension endpoints (coreth vm.go:122-124; atomic/vm/vm.go:74) ──────────

/// `ethRPCEndpoint` — the geth JSON-RPC mount (coreth `vm.go:123`).
pub const ETH_RPC_ENDPOINT: &str = "/rpc";
/// `ethWSEndpoint` — the websocket mount of the same server (coreth `vm.go:124`).
pub const ETH_WS_ENDPOINT: &str = "/ws";
/// `adminEndpoint` — the coreth admin API mount (coreth `vm.go:122`).
pub const ADMIN_ENDPOINT: &str = "/admin";
/// `avaxEndpoint` — the atomic `avax.*` API mount (coreth `atomic/vm/vm.go:74`).
pub const AVAX_ENDPOINT: &str = "/avax";

// ─── Ethereum JSON-RPC 2.0 dispatch (geth rpc.Server envelope) ────────────────

/// JSON-RPC 2.0 error codes (geth `rpc/errors.go`).
mod code {
    /// `-32700` — request body is not parseable JSON.
    pub const PARSE: i32 = -32700;
    /// `-32600` — the request envelope is malformed.
    pub const INVALID_REQUEST: i32 = -32600;
    /// `-32601` — unknown method.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// `-32602` — the params failed to decode.
    pub const INVALID_PARAMS: i32 = -32602;
    /// `-32000` — a handler/domain error (geth's default server error).
    pub const SERVER: i32 = -32000;
}

/// A dispatch failure carrying its JSON-RPC error code + message.
struct Failure {
    code: i32,
    message: String,
}

impl Failure {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: code::INVALID_PARAMS,
            message: message.into(),
        }
    }
}

/// The buffered Ethereum JSON-RPC service serving `/rpc` and `/ws` (the geth
/// `rpc.Server` seat, coreth `vm.go:1030/1067-1068`): single requests and
/// batch arrays, positional params, HTTP 200 for every JSON-RPC-level reply.
pub struct EthHttpService {
    eth: EthRpc,
}

impl EthHttpService {
    /// Builds the service over the [`EthRpc`] handler set.
    #[must_use]
    pub fn new(eth: EthRpc) -> Self {
        Self { eth }
    }

    /// Dispatches one request envelope to its handler, producing the response
    /// envelope (always carrying the request `id`, `null` when absent).
    fn dispatch_one(&self, req: &Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = req.get("method").and_then(Value::as_str) else {
            return error_envelope(id, code::INVALID_REQUEST, "invalid request");
        };
        let empty = Vec::new();
        let params = req
            .get("params")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        match self.call(method, params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(f) => error_envelope(id, f.code, f.message),
        }
    }

    /// Routes `method` to the matching [`EthRpc`] body (the method set the
    /// M6.23 handlers implement; everything else is `-32601`).
    fn call(&self, method: &str, params: &[Value]) -> Result<Value, Failure> {
        let domain = |r: crate::error::Result<Value>| {
            r.map_err(|e| Failure {
                code: code::SERVER,
                message: e.to_string(),
            })
        };
        match method {
            "eth_chainId" => Ok(self.eth.chain_id()),
            "eth_blockNumber" => domain(self.eth.block_number()),
            "eth_getBalance" => {
                let addr = addr_param(params, 0)?;
                domain(self.eth.get_balance(addr, tag_param(params, 1)?))
            }
            "eth_getTransactionCount" => {
                let addr = addr_param(params, 0)?;
                domain(self.eth.get_transaction_count(addr, tag_param(params, 1)?))
            }
            "eth_getCode" => {
                let addr = addr_param(params, 0)?;
                domain(self.eth.get_code(addr, tag_param(params, 1)?))
            }
            "eth_getStorageAt" => {
                let addr = addr_param(params, 0)?;
                let slot = b256_param(params, 1)?;
                domain(self.eth.get_storage_at(addr, slot, tag_param(params, 2)?))
            }
            "eth_call" => {
                let req = call_param(params, 0)?;
                domain(self.eth.call(req, tag_param(params, 1)?))
            }
            "eth_estimateGas" => {
                let req = call_param(params, 0)?;
                domain(self.eth.estimate_gas(req, tag_param(params, 1)?))
            }
            "eth_getProof" => {
                let addr = addr_param(params, 0)?;
                let slots = slots_param(params, 1)?;
                domain(self.eth.get_proof(addr, &slots, tag_param(params, 2)?))
            }
            "eth_gasPrice" => domain(self.eth.gas_price()),
            "eth_maxPriorityFeePerGas" => domain(self.eth.max_priority_fee_per_gas()),
            "eth_feeHistory" => {
                let args = FeeHistoryArgs {
                    block_count: u64_param(params, 0)?,
                    newest_block: tag_param(params, 1)?,
                    reward_percentiles: percentiles_param(params, 2)?,
                };
                domain(self.eth.fee_history(args))
            }
            "debug_traceTransaction" => {
                let hash = b256_param(params, 0)?;
                domain(self.eth.debug_trace_transaction(hash))
            }
            other => Err(Failure {
                code: code::METHOD_NOT_FOUND,
                message: format!("the method {other} does not exist/is not available"),
            }),
        }
    }
}

#[async_trait]
impl VmHttpService for EthHttpService {
    async fn serve_http(&self, req: VmRequest) -> VmResponse {
        // geth's HTTP transport is POST-only (`rpc/http.go` → 405).
        if !req.method.eq_ignore_ascii_case("POST") {
            return VmResponse {
                status: 405,
                headers: vec![(
                    "content-type".to_string(),
                    "text/plain; charset=utf-8".to_string(),
                )],
                body: b"method not allowed\n".to_vec(),
            };
        }
        let reply = match serde_json::from_slice::<Value>(&req.body) {
            // Batch array (geth serves each entry; an empty batch is invalid).
            Ok(Value::Array(reqs)) => {
                if reqs.is_empty() {
                    error_envelope(Value::Null, code::INVALID_REQUEST, "empty batch")
                } else {
                    Value::Array(reqs.iter().map(|r| self.dispatch_one(r)).collect())
                }
            }
            Ok(obj @ Value::Object(_)) => self.dispatch_one(&obj),
            Ok(_) => error_envelope(Value::Null, code::INVALID_REQUEST, "invalid request"),
            Err(e) => error_envelope(Value::Null, code::PARSE, format!("parse error: {e}")),
        };
        // Serializing a built `Value` cannot fail; the fallback keeps the
        // no-unwrap library convention.
        let body = serde_json::to_vec(&reply).unwrap_or_default();
        VmResponse::ok("application/json", body)
    }
}

/// Builds a JSON-RPC error response envelope.
fn error_envelope(id: Value, error_code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": error_code, "message": message.into() },
    })
}

// ─── Positional-param decoding helpers ────────────────────────────────────────

/// The string at `params[i]`, or `-32602` naming the missing/mistyped slot.
fn str_param(params: &[Value], i: usize) -> Result<&str, Failure> {
    params
        .get(i)
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::invalid_params(format!("missing or non-string argument {i}")))
}

/// A 20-byte `0x…` address at `params[i]`.
fn addr_param(params: &[Value], i: usize) -> Result<Address, Failure> {
    let s = str_param(params, i)?;
    Address::from_str(s)
        .map_err(|e| Failure::invalid_params(format!("invalid address argument {i}: {e}")))
}

/// A 32-byte `0x…` word (storage slot / tx hash) at `params[i]`.
fn b256_param(params: &[Value], i: usize) -> Result<B256, Failure> {
    let s = str_param(params, i)?;
    B256::from_str(s)
        .map_err(|e| Failure::invalid_params(format!("invalid 32-byte argument {i}: {e}")))
}

/// The block tag at `params[i]` (string tag / hex number; a JSON number also
/// accepts). A missing slot defaults to `latest` (the geth convention).
fn tag_param(params: &[Value], i: usize) -> Result<BlockTag, Failure> {
    match params.get(i) {
        None | Some(Value::Null) => Ok(BlockTag::Latest),
        Some(Value::String(s)) => BlockTag::parse(s)
            .map_err(|e| Failure::invalid_params(format!("invalid block argument {i}: {e}"))),
        Some(Value::Number(n)) => n
            .as_u64()
            .map(BlockTag::Number)
            .ok_or_else(|| Failure::invalid_params(format!("invalid block argument {i}"))),
        Some(_) => Err(Failure::invalid_params(format!(
            "invalid block argument {i}"
        ))),
    }
}

/// A `u64` quantity at `params[i]`: `0x`-hex / decimal string, or JSON number.
fn u64_param(params: &[Value], i: usize) -> Result<u64, Failure> {
    match params.get(i) {
        Some(Value::String(s)) => {
            let parsed = if let Some(hex) = s.strip_prefix("0x") {
                u64::from_str_radix(hex, 16)
            } else {
                s.parse::<u64>()
            };
            parsed.map_err(|e| Failure::invalid_params(format!("invalid quantity {i}: {e}")))
        }
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| Failure::invalid_params(format!("invalid quantity {i}"))),
        _ => Err(Failure::invalid_params(format!(
            "missing quantity argument {i}"
        ))),
    }
}

/// The array of 32-byte slots at `params[i]` (`eth_getProof`); missing → empty.
fn slots_param(params: &[Value], i: usize) -> Result<Vec<B256>, Failure> {
    match params.get(i) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| Failure::invalid_params(format!("non-string slot in arg {i}")))
                    .and_then(|s| {
                        B256::from_str(s).map_err(|e| {
                            Failure::invalid_params(format!("invalid slot in arg {i}: {e}"))
                        })
                    })
            })
            .collect(),
        Some(_) => Err(Failure::invalid_params(format!(
            "argument {i} must be an array of slots"
        ))),
    }
}

/// The reward-percentile array at `params[i]` (`eth_feeHistory`); missing →
/// empty.
fn percentiles_param(params: &[Value], i: usize) -> Result<Vec<f64>, Failure> {
    match params.get(i) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_f64().ok_or_else(|| {
                    Failure::invalid_params(format!("non-numeric percentile in arg {i}"))
                })
            })
            .collect(),
        Some(_) => Err(Failure::invalid_params(format!(
            "argument {i} must be an array of percentiles"
        ))),
    }
}

/// The `eth_call`/`eth_estimateGas` transaction object at `params[i]`.
fn call_param(params: &[Value], i: usize) -> Result<CallRequest, Failure> {
    let Some(obj) = params.get(i).and_then(Value::as_object) else {
        return Err(Failure::invalid_params(format!(
            "missing call object argument {i}"
        )));
    };
    let opt_addr = |key: &str| -> Result<Option<Address>, Failure> {
        match obj.get(key).and_then(Value::as_str) {
            None => Ok(None),
            Some(s) => Address::from_str(s)
                .map(Some)
                .map_err(|e| Failure::invalid_params(format!("invalid {key}: {e}"))),
        }
    };
    let opt_quantity = |key: &str| -> Result<Option<u64>, Failure> {
        match obj.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => u64_param(std::slice::from_ref(v), 0)
                .map(Some)
                .map_err(|_| Failure::invalid_params(format!("invalid {key}"))),
        }
    };
    let value = match obj.get("value").and_then(Value::as_str) {
        None => None,
        Some(s) => {
            let hex = s.strip_prefix("0x").unwrap_or(s);
            let radix = if s.starts_with("0x") { 16 } else { 10 };
            Some(
                RuintU256::from_str_radix(hex, radix)
                    .map_err(|e| Failure::invalid_params(format!("invalid value: {e}")))?,
            )
        }
    };
    // geth accepts both `data` and the post-EIP-1559 `input` key.
    let data = match obj
        .get("data")
        .or_else(|| obj.get("input"))
        .and_then(Value::as_str)
    {
        None => None,
        Some(s) => {
            let hex_str = s.strip_prefix("0x").unwrap_or(s);
            let bytes = hex::decode(hex_str)
                .map_err(|e| Failure::invalid_params(format!("invalid data: {e}")))?;
            Some(ava_evm_reth::Bytes::from(bytes))
        }
    };
    Ok(CallRequest {
        from: opt_addr("from")?,
        to: opt_addr("to")?,
        gas: opt_quantity("gas")?,
        value,
        data,
    })
}

// ─── Gorilla `avax.*` service (coreth atomic/vm/api.go via utils/rpc) ─────────

/// Maps a C-Chain domain error onto the gorilla `-32000` server error (the
/// `utils/rpc` handler surfaces Go handler errors the same way, 14 §16.1).
fn server_err(e: crate::error::Error) -> RpcError {
    RpcError::server(e.to_string())
}

/// Parses a wire `txID` (CB58). The empty/absent field maps to [`Id::EMPTY`]
/// so the handler's Go-parity `errNilTxID` check fires (Go's zero `ids.ID`).
fn parse_tx_id(s: &str) -> Result<Id, RpcError> {
    if s.is_empty() {
        return Ok(Id::EMPTY);
    }
    Id::from_str(s).map_err(|e| RpcError::invalid_params(format!("couldn't parse txID: {e}")))
}

/// Accepts a `u32` as a JSON number or an avalanchego `json.Uint32` quoted
/// string; absent/`null` → 0 (Go's zero value → "use the max" in the handler).
fn de_flex_u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    match Value::deserialize(d)? {
        Value::Null => Ok(0),
        Value::Number(n) => n
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| serde::de::Error::custom("limit out of range")),
        Value::String(s) => s.parse::<u32>().map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom("limit must be a number or string")),
    }
}

/// Go `api.FormattedTx` — the `avax.issueTx` wire args.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct IssueTxWireArgs {
    /// The encoded signed atomic tx.
    pub tx: String,
    /// The encoding name (`hex`/`hexnc`; empty defaults to `hex`).
    pub encoding: String,
}

/// Go `api.JSONTxID` — the `avax.getAtomicTxStatus` wire args.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct JsonTxIdArgs {
    /// The CB58 tx id (absent → the nil id, Go's zero `ids.ID`).
    #[serde(rename = "txID")]
    pub tx_id: String,
}

/// Go `api.GetTxArgs` — the `avax.getAtomicTx` wire args.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetTxWireArgs {
    /// The CB58 tx id.
    #[serde(rename = "txID")]
    pub tx_id: String,
    /// The reply encoding (`hex`/`hexnc`; empty defaults to `hex`).
    pub encoding: String,
}

/// Go `api.GetUTXOsArgs` — the `avax.getUTXOs` wire args.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetUtxosWireArgs {
    /// The addresses whose UTXOs are fetched.
    pub addresses: Vec<String>,
    /// The source chain alias/id (Go errors when empty).
    #[serde(rename = "sourceChain")]
    pub source_chain: String,
    /// Page size (`json.Uint32`: number or quoted string; 0 → max).
    #[serde(deserialize_with = "de_flex_u32")]
    pub limit: u32,
    /// The pagination cursor — accepted and currently ignored (the indexed
    /// shared-memory fetch is deferred; see [`AvaxRpc::get_utxos`]).
    #[serde(rename = "startIndex")]
    pub start_index: Value,
    /// The reply encoding.
    pub encoding: String,
}

/// The gorilla `avax` service wrapper over [`AvaxRpc`] (Go `AvaxAPI`,
/// coreth `atomic/vm/api.go`).
pub struct AvaxService {
    rpc: AvaxRpc,
}

#[rpc_service("avax")]
impl AvaxService {
    /// `avax.getUTXOs` (Go `AvaxAPI.GetUTXOs`, `atomic/vm/api.go:57`).
    ///
    /// # Errors
    /// Go-parity `-32000` for no addresses / no source chain / handler errors.
    #[rpc(name = "GetUTXOs")]
    pub async fn get_utxos(&self, args: GetUtxosWireArgs) -> Result<Value, RpcError> {
        // Go checks addresses first (api.go:66), then the source chain
        // (api.go:73). Both wire messages are checked HERE so they stay the
        // bare Go strings (the handler body's `Error` Display carries the
        // crate's string-variant prefix).
        if args.addresses.is_empty() {
            return Err(RpcError::server("no addresses provided"));
        }
        if args.source_chain.is_empty() {
            return Err(RpcError::server("no source chain provided"));
        }
        self.rpc
            .get_utxos(&args.addresses, &args.source_chain, args.limit)
            .map_err(server_err)
    }

    /// `avax.issueTx` (Go `AvaxAPI.IssueTx`, `atomic/vm/api.go:151`).
    ///
    /// # Errors
    /// `-32000` on decode/parse/mempool failures (Go surfaces them directly).
    pub async fn issue_tx(&self, args: IssueTxWireArgs) -> Result<Value, RpcError> {
        self.rpc
            .issue_tx(IssueTxArgs {
                tx: args.tx,
                encoding: args.encoding,
            })
            .map_err(server_err)
    }

    /// `avax.getAtomicTxStatus` (Go `AvaxAPI.GetAtomicTxStatus`,
    /// `atomic/vm/api.go:190`).
    ///
    /// # Errors
    /// `-32000` for the nil tx id (Go `errNilTxID`).
    pub async fn get_atomic_tx_status(&self, args: JsonTxIdArgs) -> Result<Value, RpcError> {
        let tx_id = parse_tx_id(&args.tx_id)?;
        self.rpc.get_atomic_tx_status(tx_id).map_err(server_err)
    }

    /// `avax.getAtomicTx` (Go `AvaxAPI.GetAtomicTx`, `atomic/vm/api.go:215`).
    ///
    /// # Errors
    /// `-32000` for the nil tx id / unknown tx (Go `could not find tx <id>`).
    pub async fn get_atomic_tx(&self, args: GetTxWireArgs) -> Result<Value, RpcError> {
        let tx_id = parse_tx_id(&args.tx_id)?;
        self.rpc
            .get_atomic_tx(tx_id, args.encoding)
            .map_err(server_err)
    }
}

/// Builds the `avax` [`ServiceRegistry`] (exactly the four Go wire methods).
#[must_use]
pub fn avax_registry(rpc: AvaxRpc) -> ServiceRegistry {
    let mut registry = ServiceRegistry::new();
    Arc::new(AvaxService { rpc }).register_rpc(&mut registry);
    registry
}

/// The `avax.*` mount as a buffered in-process handler (gorilla envelope).
#[must_use]
pub fn avax_service(rpc: AvaxRpc) -> Arc<dyn VmHttpService> {
    registry_service(Arc::new(avax_registry(rpc)))
}

// ─── Gorilla `admin` service (coreth plugin/evm/admin.go via utils/rpc) ───────

/// The empty gorilla args object (Go `*struct{}`).
#[derive(Debug, Default, Deserialize)]
pub struct EmptyArgs {}

/// Go `client.SetLogLevelArgs` — `{"level": "..."}`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SetLogLevelArgs {
    /// The requested log level string.
    pub level: String,
}

/// The gorilla `admin` service wrapper over [`AdminRpc`] (Go `Admin`,
/// coreth `plugin/evm/admin.go`).
pub struct AdminService {
    rpc: AdminRpc,
}

#[rpc_service("admin")]
impl AdminService {
    /// `admin.startCPUProfiler` (Go `Admin.StartCPUProfiler`, `admin.go:31`).
    ///
    /// # Errors
    /// Currently infallible (no-op profiler; see [`AdminRpc`]).
    #[rpc(name = "StartCPUProfiler")]
    pub async fn start_cpu_profiler(&self, _args: EmptyArgs) -> Result<Value, RpcError> {
        self.rpc.start_cpu_profiler().map_err(server_err)
    }

    /// `admin.stopCPUProfiler` (Go `Admin.StopCPUProfiler`, `admin.go:41`).
    ///
    /// # Errors
    /// Currently infallible (no-op profiler).
    #[rpc(name = "StopCPUProfiler")]
    pub async fn stop_cpu_profiler(&self, _args: EmptyArgs) -> Result<Value, RpcError> {
        self.rpc.stop_cpu_profiler().map_err(server_err)
    }

    /// `admin.memoryProfile` (Go `Admin.MemoryProfile`, `admin.go:51`).
    ///
    /// # Errors
    /// Currently infallible (no-op profiler).
    pub async fn memory_profile(&self, _args: EmptyArgs) -> Result<Value, RpcError> {
        self.rpc.memory_profile().map_err(server_err)
    }

    /// `admin.lockProfile` (Go `Admin.LockProfile`, `admin.go:61`).
    ///
    /// # Errors
    /// Currently infallible (no-op profiler).
    pub async fn lock_profile(&self, _args: EmptyArgs) -> Result<Value, RpcError> {
        self.rpc.lock_profile().map_err(server_err)
    }

    /// `admin.setLogLevel` (Go `Admin.SetLogLevel`, `admin.go:70`).
    ///
    /// # Errors
    /// Currently infallible (no-op dynamic logger; see [`AdminRpc`]).
    pub async fn set_log_level(&self, args: SetLogLevelArgs) -> Result<Value, RpcError> {
        self.rpc.set_log_level(&args.level).map_err(server_err)
    }
}

/// Builds the `admin` [`ServiceRegistry`]. Go also registers `GetVMConfig`
/// (`admin.go:82`, the live VM config echo); that lands with the EvmVm config
/// plumbing (M8.23/M8.29 follow-up) — [`AdminRpc`] has no config to echo yet.
#[must_use]
pub fn admin_registry(rpc: AdminRpc) -> ServiceRegistry {
    let mut registry = ServiceRegistry::new();
    Arc::new(AdminService { rpc }).register_rpc(&mut registry);
    registry
}

/// The `/admin` mount as a buffered in-process handler (gorilla envelope).
#[must_use]
pub fn admin_service(rpc: AdminRpc) -> Arc<dyn VmHttpService> {
    registry_service(Arc::new(admin_registry(rpc)))
}

#[cfg(test)]
// `serde_json::Value` indexing returns `Value::Null` on a missing key; it is
// the idiomatic way to assert on JSON-RPC bodies (the proposervm precedent).
#[allow(clippy::indexing_slicing)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    // The gorilla method sets match Go exactly (wire-name parity incl. the
    // GetUTXOs / StartCPUProfiler / StopCPUProfiler acronym overrides).
    #[test]
    fn gorilla_method_sets_match_go() {
        let (avax, _mempool) = avax_fixture();
        let reg = avax_registry(avax);
        assert_eq!(reg.len(), 4, "avax registers exactly the four Go methods");
        for m in ["GetUTXOs", "IssueTx", "GetAtomicTxStatus", "GetAtomicTx"] {
            assert!(reg.lookup("avax", m).is_some(), "avax.{m} registered");
        }
        assert!(
            reg.lookup("avax", "GetUtxos").is_none(),
            "exact-remainder matching: no pascalized GetUtxos"
        );

        let reg = admin_registry(AdminRpc::new());
        assert_eq!(reg.len(), 5, "admin registers the five no-op Go methods");
        for m in [
            "StartCPUProfiler",
            "StopCPUProfiler",
            "MemoryProfile",
            "LockProfile",
            "SetLogLevel",
        ] {
            assert!(reg.lookup("admin", m).is_some(), "admin.{m} registered");
        }
    }

    fn avax_fixture() -> (
        AvaxRpc,
        Arc<parking_lot::Mutex<crate::atomic::mempool::AtomicMempool>>,
    ) {
        let mempool = Arc::new(parking_lot::Mutex::new(
            crate::atomic::mempool::AtomicMempool::new(16, Id::from([0xAA; 32])),
        ));
        let canonical = Arc::new(crate::canonical::CanonicalStore::new(Arc::new(
            ava_database::MemDb::new(),
        )));
        let accepted = Arc::new(crate::rpc::avax::AcceptedAtomicTxIndex::new());
        (
            AvaxRpc::new(Arc::clone(&mempool), canonical, accepted),
            mempool,
        )
    }

    // getUTXOs arg gating mirrors Go's check order (api.go:66/73).
    #[tokio::test]
    async fn get_utxos_arg_gating_matches_go() {
        let (avax, _mempool) = avax_fixture();
        let svc = AvaxService { rpc: avax };

        let no_addrs = svc.get_utxos(GetUtxosWireArgs::default()).await;
        assert_eq!(
            no_addrs.err().map(|e| e.message),
            Some("no addresses provided".to_string()),
            "empty addresses → Go errNoAddresses"
        );

        let no_chain = svc
            .get_utxos(GetUtxosWireArgs {
                addresses: vec!["C-avax1...".to_string()],
                ..GetUtxosWireArgs::default()
            })
            .await;
        assert_eq!(
            no_chain.err().map(|e| e.message),
            Some("no source chain provided".to_string()),
            "empty sourceChain → Go errNoSourceChain"
        );
    }

    // json.Uint32 limit accepts both a number and a quoted string.
    #[test]
    fn get_utxos_limit_is_flexible() {
        let args: GetUtxosWireArgs =
            serde_json::from_value(json!({ "limit": 7 })).expect("numeric limit");
        assert_eq!(args.limit, 7, "numeric limit");
        let args: GetUtxosWireArgs =
            serde_json::from_value(json!({ "limit": "9" })).expect("quoted limit");
        assert_eq!(args.limit, 9, "quoted json.Uint32 limit");
    }

    // The eth envelope: unknown method → -32601 with geth's message shape.
    #[tokio::test]
    async fn eth_unknown_method_is_32601() {
        let (_dir, svc) = eth_fixture();
        let body = post_eth(
            &svc,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "eth_nope", "params": [] }),
        )
        .await;
        assert_eq!(body["error"]["code"], -32601, "unknown method code");
        assert_eq!(
            body["error"]["message"], "the method eth_nope does not exist/is not available",
            "geth message shape"
        );
        assert_eq!(body["id"], 1, "request id echoed");
    }

    // Batch arrays dispatch per-entry (geth rpc.Server batch handling).
    #[tokio::test]
    async fn eth_batch_dispatches_each_entry() {
        let (_dir, svc) = eth_fixture();
        let body = post_eth(
            &svc,
            json!([
                { "jsonrpc": "2.0", "id": 1, "method": "eth_maxPriorityFeePerGas", "params": [] },
                { "jsonrpc": "2.0", "id": 2, "method": "eth_nope", "params": [] },
            ]),
        )
        .await;
        assert_eq!(body[0]["result"], "0x0", "zero C-Chain tip");
        assert_eq!(body[1]["error"]["code"], -32601, "per-entry errors");
    }

    // Non-POST is rejected at the transport (geth 405).
    #[tokio::test]
    async fn eth_get_is_405() {
        let (_dir, svc) = eth_fixture();
        let resp = svc
            .serve_http(VmRequest {
                method: "GET".to_string(),
                uri: "/rpc".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            })
            .await;
        assert_eq!(resp.status, 405, "geth HTTP transport is POST-only");
    }

    /// An `EthHttpService` over a temp-Firewood fixture: no state is read by
    /// the methods exercised here (`eth_maxPriorityFeePerGas`, unknown
    /// methods), but the handler set needs a live provider. The returned
    /// guard keeps the temp dir alive for the test's lifetime.
    fn eth_fixture() -> (tempfile::TempDir, EthHttpService) {
        use crate::chainspec::AvaChainSpec;
        use crate::evmconfig::AvaEvmConfig;

        let canonical = Arc::new(crate::canonical::CanonicalStore::new(Arc::new(
            ava_database::MemDb::new(),
        )));
        let config = AvaEvmConfig::new(AvaChainSpec::c_chain(
            1,
            ava_evm_reth::Chain::from_id(43114),
        ));
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = crate::state::FirewoodStateProvider::open(
            dir.path(),
            Arc::new(ava_database::MemDb::new()),
            Arc::new(ava_database::MemDb::new()),
        )
        .expect("open firewood");
        let svc = EthHttpService::new(EthRpc::new(provider, canonical, config, 43114));
        (dir, svc)
    }

    async fn post_eth(svc: &EthHttpService, body: Value) -> Value {
        let resp = svc
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: "/rpc".to_string(),
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: serde_json::to_vec(&body).expect("serialize"),
            })
            .await;
        assert_eq!(resp.status, 200, "JSON-RPC replies are HTTP 200");
        serde_json::from_slice(&resp.body).expect("json body")
    }
}
