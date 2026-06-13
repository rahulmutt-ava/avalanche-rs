// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (AVM) JSON-RPC **handler** service — port of the avm.* API methods
//! from `vms/avm/service.go` (specs 09 §10, 14 API reference).
//!
//! This module ports the request/response *shapes* (serde types matching Go's
//! JSON field names + encodings) and the handler *logic* over the
//! [`ReadOnlyChain`] state seam.
//!
//! ## Transport (M8.22)
//!
//! [`RpcService`] bridges these typed bodies onto the gorilla-json2
//! [`ServiceRegistry`] under the Go service name `avm` (Go `vms/avm/vm.go:293`
//! `CreateHandlers` registers `&Service{vm}` as `"avm"` at extension `""`);
//! `AvmVm::create_handlers` mounts the [`registry`] through the in-process
//! `HttpHandler` seam. `issueTx` submits through the [`TxIssuer`] seam (the VM
//! implements it over the shared mempool via the gossip admission path). The
//! Go `"/wallet"` extension (keystore) is out of scope — see `tests/PORTING.md`
//! for the method inventory vs Go.
//!
//! ## Encodings (match Go exactly, `vms/avm/service.go`)
//!
//! - Integers use the avalanchego `json.Uint64`/`Uint8` convention: **quoted
//!   decimal strings** (`json.Uint64` ⇒ `"1234"`), via [`avajson`] serde helpers.
//! - [`Id`] serializes through its own `Serialize` impl (CB58), matching `ids.ID`.
//! - Addresses are bech32 chain-prefixed (`X-avax1…`), via
//!   [`ava_crypto::address::format`] with chain prefix `"X"`.
//! - Tx / block bytes are returned as checksummed hex `0x<hex(bytes ++ sha256(bytes)[28..32])>`
//!   (Go `formatting.Encode(formatting.Hex, bytes)` — `Hex` appends 4 checksum bytes before
//!   hex-encoding; `HexNC` skips the checksum). The default encoding is `Hex` (zero value of
//!   `formatting.Encoding`).
//! - Timestamps are RFC3339 (`time.Time` JSON), seconds precision.
//!
//! ## Deferred functionality (spec 09 §10 deferral list)
//!
//! - **`getUTXOs` / `getBalance` / `getAllBalances`** are LIVE as of M8.23b over
//!   the address → UTXO index (`State::utxo_ids`; the Rust
//!   [`get_paginated_utxos`]/[`get_all_utxos`] port of
//!   `vms/components/avax/utxo_fetching.go`) plus the shared-memory atomic path
//!   (`avax.GetAtomicUTXOs` over [`SharedMemory::indexed`]). Remaining recorded
//!   deferrals: the node-level `BCLookup` aliaser (the [`ChainLookup`] seam —
//!   `"P"`/`"C"` aliases need `ava-node` wiring; chain-id strings work) and the
//!   VM asset aliaser (`"AVAX"` does not resolve in `getBalance.assetID`).
//! - **`issueTx` mempool submit**: the service carries only the state handle;
//!   actual mempool add + p2p gossip needs the `AvmVm` handle. The method parses
//!   the tx and returns its id (useful for client round-trip testing), but does
//!   NOT submit to the mempool. Full wiring is the `ava-api` transport task.
//! - **`getTxStatus` mempool-`Processing`**: only accepted-or-unknown is
//!   implementable over the read-only state. Mempool `Processing` status needs
//!   the VM handle (follow-up).
//! - **`getAssetDescription` asset alias lookup**: Go also accepts an alias string
//!   (not just the CB58 asset id); the alias lookup needs the VM's alias store.
//!   The method accepts a raw CB58 asset id only (no alias). The alias lookup is
//!   deferred (follow-up).
//! - **`wallet.*` / keystore methods**: out of scope (not applicable to the Rust
//!   port's threat model / key-management boundary).
//! - **`getHeight` block parsing**: Go re-fetches and parses the block to get its
//!   height (via `chainManager.GetStatelessBlock`). Since the service holds the
//!   state directly, we derive height by parsing the last-accepted block bytes.
//!
//! ## Determinism (00 §6.1)
//!
//! The service only performs read operations; no ordering guarantees are needed
//! beyond the state's own read consistency.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use ava_api_macros::rpc_service;
use ava_crypto::address;
use ava_crypto::hashing::checksum;
use ava_types::constants::get_hrp;
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use ava_utils::clock::{Clock, RealClock};
use ava_vm::components::avax::shared_memory::SharedMemory;
use serde::{Deserialize, Serialize};

use crate::block::Block;
use crate::error::{Error, Result};
use crate::jsonrpc::{RpcError, ServiceRegistry};
use crate::state::chain::ReadOnlyChain;
use crate::txs::Tx;
use crate::txs::codec::Codec;
use crate::txs::components::Output;
use crate::txs::executor::semantic::Utxo;

// ---------------------------------------------------------------------------
// `avajson` — Go `utils/json` numeric encodings (quoted decimal strings)
// ---------------------------------------------------------------------------

/// avalanchego `utils/json` numeric encodings: integers as quoted decimal
/// strings (`json.Uint64` ⇒ `"1234"`, `json.Uint8` ⇒ `"0"`).
pub mod avajson {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize a `u64` as a quoted decimal string (`json.Uint64`).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_u64<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    /// Deserialize a `u64` from a quoted decimal string (`json.Uint64`).
    ///
    /// # Errors
    /// Returns a deserialization error if the string is not a base-10 integer.
    pub fn deserialize_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<u64>().map_err(serde::de::Error::custom)
    }

    /// Serialize an `Option<u64>` as `null` or a quoted decimal string.
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_opt_u64<S: Serializer>(v: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(n) => s.serialize_str(&n.to_string()),
            None => s.serialize_none(),
        }
    }

    /// Deserialize an `Option<u64>` from `null` or a quoted decimal string.
    ///
    /// # Errors
    /// Returns a deserialization error if a present value is not a base-10 integer.
    pub fn deserialize_opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        match opt {
            Some(s) => s.parse::<u64>().map(Some).map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }

    /// Serialize a `u32` as a quoted decimal string (`json.Uint32`).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_u32<S: Serializer>(v: &u32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    /// Deserialize a `u32` from a quoted decimal string **or** a bare JSON
    /// number — Go's `json.Uint32.UnmarshalJSON` trims surrounding quotes
    /// before parsing, so both `"5"` and `5` are accepted on the wire.
    ///
    /// # Errors
    /// Returns a deserialization error if the value is not a base-10 `u32`.
    pub fn deserialize_flex_u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        struct FlexU32;
        impl serde::de::Visitor<'_> for FlexU32 {
            type Value = u32;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a u32 as a number or quoted decimal string")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u32, E> {
                u32::try_from(v).map_err(E::custom)
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<u32, E> {
                v.parse::<u32>().map_err(E::custom)
            }
        }
        d.deserialize_any(FlexU32)
    }

    /// Serialize a `u8` as a quoted decimal string (`json.Uint8`).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_u8<S: Serializer>(v: &u8, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    /// Deserialize a `u8` from a quoted decimal string (`json.Uint8`).
    ///
    /// # Errors
    /// Returns a deserialization error if the string is not a base-10 integer in [0, 255].
    pub fn deserialize_u8<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<u8>().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

/// Formats a block/tx byte slice as `0x<hex(bytes ++ checksum)>` — exactly
/// matching Go's `formatting.Encode(formatting.Hex, bytes)`.
///
/// Go: `Hex` encoding appends `hashing.Checksum(bytes, 4)` (the last 4 bytes of
/// `sha256(bytes)`) then hex-encodes the combined slice with a `"0x"` prefix
/// (see `utils/formatting/encoding.go`).
fn hex_encode(bytes: &[u8]) -> String {
    let cs = checksum(bytes, 4);
    let mut combined = bytes.to_vec();
    combined.extend_from_slice(&cs);
    format!("0x{}", hex::encode(&combined))
}

/// `formatting.Encode(encoding, bytes)` for the encodings the AVM service
/// accepts: `""`/`"hex"`/`"hexc"` (checksummed) and `"hexnc"` (no checksum).
///
/// # Errors
/// Returns [`Error::Service`] with Go's `invalid encoding` message for an
/// unknown encoding name (Go rejects it at `Encoding.UnmarshalJSON` time).
fn encode_formatted_bytes(bytes: &[u8], encoding: &str) -> Result<String> {
    match encoding.to_lowercase().as_str() {
        "" | "hex" | "hexc" => Ok(hex_encode(bytes)),
        "hexnc" => Ok(format!("0x{}", hex::encode(bytes))),
        _ => Err(Error::Service("invalid encoding".to_owned())),
    }
}

/// The `encoding` string echoed in replies (`"hex"` for the default/checksummed
/// forms; otherwise the normalized lowercase name).
fn reply_encoding(encoding: &str) -> &'static str {
    match encoding.to_lowercase().as_str() {
        "" | "hex" | "hexc" => "hex",
        "hexnc" => "hexnc",
        _ => "hex",
    }
}

// ---------------------------------------------------------------------------
// Status enum — `choices.Status` (Go `snow/choices/status.go`)
// ---------------------------------------------------------------------------

/// `choices.Status` — the lifecycle state of an X-Chain transaction
/// (Go `snow/choices/status.go`; used by `GetTxStatus`).
///
/// Only `Accepted` and `Unknown` are implemented by the read-only state seam;
/// `Processing` requires the VM mempool handle (deferred, see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxStatus {
    /// The transaction was accepted (present in the accepted state store).
    Accepted,
    /// The transaction status is unknown (not in the accepted state store).
    Unknown,
    /// The transaction is currently processing in the mempool / preferred
    /// chain. **Deferred**: requires the VM mempool handle.
    Processing,
}

impl TxStatus {
    /// The Go `String()` rendering (also the JSON form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TxStatus::Accepted => "Accepted",
            TxStatus::Unknown => "Unknown",
            TxStatus::Processing => "Processing",
        }
    }
}

// ---------------------------------------------------------------------------
// Request / reply types (mirror Go's `vms/avm/service.go` + `api/` types)
// ---------------------------------------------------------------------------

/// `api.GetHeightResponse` — reply for `avm.getHeight`.
///
/// Go: `reply.Height = avajson.Uint64(block.Height())` — a quoted decimal string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetHeightResponse {
    /// The height of the last-accepted block.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub height: u64,
}

/// Args for `avm.getBlock` (matches Go `api.GetBlockArgs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetBlockArgs {
    /// The block ID (CB58).
    #[serde(rename = "blockID")]
    pub block_id: Id,
    /// The encoding for the returned block bytes (`"hex"` or `"cb58"`).
    /// Defaults to `"hex"` (Go's `formatting.HexEncoding`).
    #[serde(default)]
    pub encoding: String,
}

/// Reply for `avm.getBlock` / `avm.getBlockByHeight`
/// (matches Go `api.GetBlockResponse`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetBlockResponse {
    /// The block bytes encoded as `0x<hex>` by default.
    pub block: String,
    /// The encoding used (`"hex"` by default).
    pub encoding: String,
}

/// Args for `avm.getBlockByHeight` (matches Go `api.GetBlockByHeightArgs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetBlockByHeightArgs {
    /// The block height.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub height: u64,
    /// The encoding for the returned block bytes. Defaults to `"hex"`.
    #[serde(default)]
    pub encoding: String,
}

/// Args for `avm.getTx` (matches Go `api.GetTxArgs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTxArgs {
    /// The tx ID (CB58).
    #[serde(rename = "txID")]
    pub tx_id: Id,
    /// The encoding for the returned tx bytes. Defaults to `"hex"`.
    #[serde(default)]
    pub encoding: String,
}

/// Reply for `avm.getTx` (matches Go `api.GetTxReply`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTxReply {
    /// The tx bytes encoded as `0x<hex>` by default.
    pub tx: String,
    /// The encoding used (`"hex"` by default).
    pub encoding: String,
}

/// Args for `avm.getTxStatus` (matches Go `api.JSONTxID`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTxStatusArgs {
    /// The tx ID (CB58).
    #[serde(rename = "txID")]
    pub tx_id: Id,
}

/// Reply for `avm.getTxStatus` (matches Go `avm.GetTxStatusReply`).
///
/// Go: `type GetTxStatusReply struct { Status choices.Status \`json:"status"\` }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTxStatusReply {
    /// The transaction status (`"Accepted"` or `"Unknown"`).
    pub status: TxStatus,
}

/// Args for `avm.issueTx` (matches Go `api.FormattedTx`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueTxArgs {
    /// The encoded tx bytes (hex `0x…` or CB58 depending on `encoding`).
    pub tx: String,
    /// The encoding of the `tx` field. Defaults to `"hex"`.
    #[serde(default)]
    pub encoding: String,
}

/// Reply for `avm.issueTx` (matches Go `api.JSONTxID`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueTxReply {
    /// The tx id of the parsed / submitted transaction.
    #[serde(rename = "txID")]
    pub tx_id: Id,
}

/// Args for `avm.getAssetDescription` (matches Go `avm.GetAssetDescriptionArgs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAssetDescriptionArgs {
    /// The asset id (CB58). Go also accepts an asset alias here; alias lookup
    /// is **deferred** (needs the VM alias store; follow-up task).
    #[serde(rename = "assetID")]
    pub asset_id: String,
}

/// Reply for `avm.getAssetDescription`
/// (matches Go `avm.GetAssetDescriptionReply`).
///
/// Go:
/// ```go
/// type GetAssetDescriptionReply struct {
///     FormattedAssetID
///     Name         string        `json:"name"`
///     Symbol       string        `json:"symbol"`
///     Denomination avajson.Uint8 `json:"denomination"`
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAssetDescriptionReply {
    /// The canonical asset id (CB58).
    #[serde(rename = "assetID")]
    pub asset_id: Id,
    /// The asset's human-readable name.
    pub name: String,
    /// The asset's short symbol (uppercase ASCII).
    pub symbol: String,
    /// The denomination (`0`–`32`), serialized as a quoted string.
    #[serde(
        serialize_with = "avajson::serialize_u8",
        deserialize_with = "avajson::deserialize_u8"
    )]
    pub denomination: u8,
}

/// Args for `avm.getUTXOs` (matches Go `api.GetUTXOsArgs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetUTXOsArgs {
    /// The addresses whose UTXOs to fetch.
    pub addresses: Vec<String>,
    /// The source chain for atomic UTXOs (empty = this chain; an alias or
    /// chain id resolves through the [`ChainLookup`] seam → shared memory).
    #[serde(default, rename = "sourceChain")]
    pub source_chain: String,
    /// Max results per page (`json.Uint32` — quoted string or bare number;
    /// `0` / out-of-range values clamp to the 1024 page max).
    #[serde(
        default,
        serialize_with = "avajson::serialize_u32",
        deserialize_with = "avajson::deserialize_flex_u32"
    )]
    pub limit: u32,
    /// Pagination start index (address + utxo id).
    #[serde(default, rename = "startIndex")]
    pub start_index: UtxoIndex,
    /// The encoding for returned UTXO bytes.
    #[serde(default)]
    pub encoding: String,
}

/// A pagination cursor (address + utxo id).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtxoIndex {
    /// The address cursor.
    pub address: String,
    /// The UTXO id cursor (CB58).
    pub utxo: String,
}

/// Reply for `avm.getUTXOs` (matches Go `api.GetUTXOsReply`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetUTXOsReply {
    /// Number of UTXOs returned.
    #[serde(
        rename = "numFetched",
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub num_fetched: u64,
    /// The encoded UTXO bytes.
    pub utxos: Vec<String>,
    /// Pagination end index.
    #[serde(rename = "endIndex")]
    pub end_index: UtxoIndex,
    /// The encoding used.
    pub encoding: String,
}

/// Args for `avm.getBalance` (matches Go `avm.GetBalanceArgs`).
///
/// Go:
/// ```go
/// type GetBalanceArgs struct {
///     Address        string `json:"address"`
///     AssetID        string `json:"assetID"`
///     IncludePartial bool   `json:"includePartial"`
/// }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetBalanceArgs {
    /// The X-Chain bech32 address (`X-avax1…`).
    pub address: String,
    /// The asset id (CB58) or alias.
    #[serde(rename = "assetID")]
    pub asset_id: String,
    /// Whether to include partially-owned / locked UTXOs.
    #[serde(default, rename = "includePartial")]
    pub include_partial: bool,
}

/// Reply for `avm.getBalance` (matches Go `avm.GetBalanceReply`).
///
/// Go:
/// ```go
/// type GetBalanceReply struct {
///     Balance avajson.Uint64 `json:"balance"`
///     UTXOIDs []avax.UTXOID  `json:"utxoIDs"`
/// }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetBalanceReply {
    /// The total balance of the asset held by the address.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub balance: u64,
    /// The UTXOs contributing to the balance.
    #[serde(rename = "utxoIDs")]
    pub utxo_ids: Vec<UtxoIdReply>,
}

/// `avax.UTXOID` as it appears in JSON replies (`json:"txID"` CB58 string +
/// `json:"outputIndex"` bare number; the runtime `Symbol` field is `json:"-"`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtxoIdReply {
    /// `UTXOID.TxID` — the id of the tx that produced the UTXO (CB58).
    #[serde(rename = "txID")]
    pub tx_id: Id,
    /// `UTXOID.OutputIndex` — a plain (unquoted) JSON number, matching Go's
    /// raw `uint32` (NOT `json.Uint32`).
    #[serde(rename = "outputIndex")]
    pub output_index: u32,
}

/// A single asset balance entry inside [`GetAllBalancesReply`]
/// (matches Go `avm.Balance`).
///
/// Go:
/// ```go
/// type Balance struct {
///     AssetID string         `json:"asset"`
///     Balance avajson.Uint64 `json:"balance"`
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBalance {
    /// The asset id (CB58), serialized as `"asset"` (matching Go's `Balance.AssetID`
    /// JSON tag `json:"asset"`).
    #[serde(rename = "asset")]
    pub asset_id: String,
    /// The balance of the asset.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub balance: u64,
}

/// Args for `avm.getAllBalances` (matches Go `avm.GetAllBalancesArgs`).
///
/// Go:
/// ```go
/// type GetAllBalancesArgs struct {
///     api.JSONAddress
///     IncludePartial bool `json:"includePartial"`
/// }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAllBalancesArgs {
    /// The X-Chain bech32 address (`X-avax1…`).
    pub address: String,
    /// Whether to include partially-owned / locked UTXOs.
    #[serde(default, rename = "includePartial")]
    pub include_partial: bool,
}

/// Reply for `avm.getAllBalances` (matches Go `avm.GetAllBalancesReply`).
///
/// Go:
/// ```go
/// type GetAllBalancesReply struct {
///     Balances []Balance `json:"balances"`
/// }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAllBalancesReply {
    /// The balances per asset.
    pub balances: Vec<AssetBalance>,
}

/// Reply for `avm.getTxFee` (matches Go `avm.GetTxFeeReply`).
///
/// Go:
/// ```go
/// type GetTxFeeReply struct {
///     TxFee            avajson.Uint64 `json:"txFee"`
///     CreateAssetTxFee avajson.Uint64 `json:"createAssetTxFee"`
/// }
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTxFeeReply {
    /// `Config.TxFee` — the fee burned by a non-asset-creation tx (nAVAX).
    #[serde(
        rename = "txFee",
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub tx_fee: u64,
    /// `Config.CreateAssetTxFee` — the fee burned by a `CreateAssetTx` (nAVAX).
    #[serde(
        rename = "createAssetTxFee",
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub create_asset_tx_fee: u64,
}

// ---------------------------------------------------------------------------
// Chain-alias lookup seam (`ctx.BCLookup`)
// ---------------------------------------------------------------------------

/// The blockchain alias lookup seam (`snow.Context.BCLookup` / `ids.Aliaser`):
/// resolves a chain alias (`"P"`, `"X"`, a chain-id string, …) to its chain id.
///
/// The node-level aliaser is an M8 `ava-node` wiring follow-up; until then the
/// service falls back to (a) the alias `"X"` for its own chain and (b) parsing
/// the string as a CB58 chain id (Go registers every chain's id string as an
/// alias of itself).
pub trait ChainLookup: Send + Sync {
    /// `Lookup(alias)` — the chain id `alias` refers to, if known.
    fn lookup(&self, alias: &str) -> Option<Id>;
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// The X-Chain (AVM) API service over a [`ReadOnlyChain`] state view.
///
/// Mirrors the handler methods of Go's `avm.Service` (port of
/// `vms/avm/service.go`); each method here is the typed handler body, served
/// on the wire by [`RpcService`]/[`registry`].
///
/// The service is deliberately constructed over the *read-only* state surface
/// (matching Go's `s.vm.state`), so it runs over either an owned
/// `State<D>` snapshot (tests) or the VM's live, lock-guarded state (`vm.rs`
/// forwards each read under the block-manager mutex). Mempool submission is
/// the separate [`TxIssuer`] seam.
pub struct Service {
    /// The X-Chain state view.
    state: Arc<dyn ReadOnlyChain>,
    /// The network id (for bech32 HRP derivation).
    network_id: u32,
    /// This chain's id (`ctx.ChainID`; local-address + `sourceChain` checks).
    chain_id: Id,
    /// The cross-chain shared-memory read handle (`ctx.SharedMemory`), backing
    /// the `getUTXOs` `sourceChain` atomic path; `None` ⇒ atomic reads error.
    shared_memory: Option<Arc<dyn SharedMemory>>,
    /// The blockchain alias lookup (`ctx.BCLookup`); `None` ⇒ the built-in
    /// fallback (`"X"` + chain-id strings) only.
    chain_lookup: Option<Arc<dyn ChainLookup>>,
    /// `Config.TxFee` (nAVAX), served by `getTxFee`.
    tx_fee: u64,
    /// `Config.CreateAssetTxFee` (nAVAX), served by `getTxFee`.
    create_asset_tx_fee: u64,
    /// The clock backing the `getBalance`/`getAllBalances` locktime check
    /// (Go `vm.clock.Unix()`).
    clock: Arc<dyn Clock>,
}

impl Service {
    /// Builds a service over a shared state view with mainnet-default fees, no
    /// shared memory / alias lookup, and the real clock. Use the `with_*`
    /// builders to wire the VM context (`vm.rs::create_handlers`).
    #[must_use]
    pub fn new(state: Arc<dyn ReadOnlyChain>, network_id: u32) -> Self {
        let fees = crate::config::Config::default();
        Self {
            state,
            network_id,
            chain_id: Id::EMPTY,
            shared_memory: None,
            chain_lookup: None,
            tx_fee: fees.tx_fee,
            create_asset_tx_fee: fees.create_asset_tx_fee,
            clock: Arc::new(RealClock),
        }
    }

    /// Sets this chain's id (`ctx.ChainID`).
    #[must_use]
    pub fn with_chain_id(mut self, chain_id: Id) -> Self {
        self.chain_id = chain_id;
        self
    }

    /// Supplies the cross-chain shared-memory read handle (`ctx.SharedMemory`).
    #[must_use]
    pub fn with_shared_memory(mut self, shared_memory: Arc<dyn SharedMemory>) -> Self {
        self.shared_memory = Some(shared_memory);
        self
    }

    /// Supplies the blockchain alias lookup (`ctx.BCLookup`).
    #[must_use]
    pub fn with_chain_lookup(mut self, lookup: Arc<dyn ChainLookup>) -> Self {
        self.chain_lookup = Some(lookup);
        self
    }

    /// Sets the fee schedule served by `getTxFee` (`Config.TxFee` /
    /// `Config.CreateAssetTxFee`).
    #[must_use]
    pub fn with_fees(mut self, tx_fee: u64, create_asset_tx_fee: u64) -> Self {
        self.tx_fee = tx_fee;
        self.create_asset_tx_fee = create_asset_tx_fee;
        self
    }

    /// Replaces the clock (tests pin the `getBalance` locktime check).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The bech32 HRP for this service's network.
    fn hrp(&self) -> &'static str {
        get_hrp(self.network_id)
    }

    /// Formats a 20-byte secp256k1 address as an X-Chain bech32 string
    /// (`X-<hrp>1…`) — the address encoding used by `getBalance` / `getUTXOs`.
    ///
    /// # Errors
    /// Returns [`Error::Service`] if bech32 encoding fails.
    pub fn format_address(&self, addr: &[u8]) -> Result<String> {
        address::format("X", self.hrp(), addr)
            .map_err(|e| Error::Service(format!("format address: {e}")))
    }

    /// `ctx.BCLookup.Lookup(alias)` — resolve a chain alias to its chain id:
    /// the [`ChainLookup`] seam first, then the built-in fallback (`"X"` is
    /// this chain; a CB58 chain-id string resolves to itself, mirroring Go
    /// registering every chain's id string as an alias).
    ///
    /// # Errors
    /// [`Error::Api`] with Go's `ids.Aliaser` message
    /// (`there is no ID with alias %s`) when unresolvable.
    fn lookup_chain(&self, alias: &str) -> Result<Id> {
        if let Some(lookup) = &self.chain_lookup
            && let Some(id) = lookup.lookup(alias)
        {
            return Ok(id);
        }
        if alias == "X" {
            return Ok(self.chain_id);
        }
        if let Ok(id) = alias.parse::<Id>() {
            return Ok(id);
        }
        Err(Error::Api(format!("there is no ID with alias {alias}")))
    }

    /// `avax.addressManager.ParseLocalAddress` — parse a chain-prefixed bech32
    /// address (`X-avax1…`) and require it to belong to this chain + network.
    ///
    /// # Errors
    /// [`Error::Api`] carrying the Go error strings (separator / alias lookup /
    /// `mismatched chainIDs` / `expected hrp`).
    fn parse_local_address(&self, addr_str: &str) -> Result<ShortId> {
        let (alias, hrp, addr_bytes) =
            address::parse(addr_str).map_err(|e| Error::Api(e.to_string()))?;
        let chain_id = self.lookup_chain(&alias)?;
        if chain_id != self.chain_id {
            return Err(Error::Api(format!(
                "mismatched chainIDs: expected {:?} but got {:?}",
                self.chain_id.to_string(),
                chain_id.to_string(),
            )));
        }
        let expected_hrp = self.hrp();
        if hrp != expected_hrp {
            return Err(Error::Api(format!(
                "expected hrp {expected_hrp:?} but got {hrp:?}"
            )));
        }
        ShortId::from_slice(&addr_bytes).map_err(|e| Error::Api(e.to_string()))
    }

    /// `avax.ParseServiceAddress` — a raw CB58 short id **or** a localized
    /// bech32 address.
    ///
    /// # Errors
    /// [`Error::Api`] `couldn't parse address %q: %w` (Go wrap) when both fail.
    fn parse_service_address(&self, addr_str: &str) -> Result<ShortId> {
        if let Ok(short) = addr_str.parse::<ShortId>() {
            return Ok(short);
        }
        self.parse_local_address(addr_str)
            .map_err(|e| Error::Api(format!("couldn't parse address {addr_str:?}: {e}")))
    }

    /// `avax.ParseServiceAddresses` — parse a batch into a sorted set.
    fn parse_service_addresses(&self, addr_strs: &[String]) -> Result<BTreeSet<ShortId>> {
        let mut addrs = BTreeSet::new();
        for addr_str in addr_strs {
            addrs.insert(self.parse_service_address(addr_str)?);
        }
        Ok(addrs)
    }

    /// `vm.lookupAssetID` — resolve an asset alias or CB58 id string.
    ///
    /// As-built: there is no VM asset aliaser yet (recorded deferral — Go can
    /// also resolve `"AVAX"`), so only the CB58 form resolves; the failure
    /// message is Go's (`asset '%s' not found`).
    ///
    /// # Errors
    /// [`Error::Api`] when the string is neither a known alias nor a CB58 id.
    fn lookup_asset_id(&self, asset: &str) -> Result<Id> {
        asset
            .parse::<Id>()
            .map_err(|_| Error::Api(format!("asset '{asset}' not found")))
    }

    /// `vm.clock.Unix()` — current Unix seconds via the clock seam.
    fn unix_now(&self) -> u64 {
        self.clock
            .now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    // -----------------------------------------------------------------------
    // `avm.getHeight` (Go `service.go GetHeight`)
    // -----------------------------------------------------------------------

    /// `avm.getHeight` — the height of the last-accepted block.
    ///
    /// Go re-fetches the block via `chainManager.GetStatelessBlock(blockID)` to
    /// read `block.Height()`. Here we parse the block bytes from the accepted
    /// block store directly (the service holds the state but not the block
    /// manager's in-memory height cache).
    ///
    /// # Errors
    /// Returns [`Error::Service`] if the last-accepted block cannot be parsed.
    pub fn get_height(&self) -> Result<GetHeightResponse> {
        let block_id = self.state.get_last_accepted();
        let height = if block_id == Id::EMPTY {
            // Fresh (uninitialized) chain — height 0.
            0
        } else {
            let bytes = self
                .state
                .get_block(block_id)
                .map_err(|e| Error::Service(format!("get block bytes for height: {e}")))?;
            Block::parse(Codec(), &bytes)
                .map(|b| b.height())
                .map_err(|e| Error::Service(format!("parse last accepted block for height: {e}")))?
        };
        Ok(GetHeightResponse { height })
    }

    // -----------------------------------------------------------------------
    // `avm.getBlock` (Go `service.go GetBlock`)
    // -----------------------------------------------------------------------

    /// `avm.getBlock` — the bytes of the block with `block_id`.
    ///
    /// Returns the raw block bytes encoded as `0x<hex>` (Go
    /// `formatting.Encode(args.Encoding, block.Bytes())`; Hex is the default).
    ///
    /// # Errors
    /// Returns [`Error::Service`] / [`Error::Database`] if the block is absent.
    pub fn get_block(&self, args: &GetBlockArgs) -> Result<GetBlockResponse> {
        let bytes = self.state.get_block(args.block_id)?;
        Ok(GetBlockResponse {
            block: hex_encode(&bytes),
            encoding: "hex".to_owned(),
        })
    }

    // -----------------------------------------------------------------------
    // `avm.getBlockByHeight` (Go `service.go GetBlockByHeight`)
    // -----------------------------------------------------------------------

    /// `avm.getBlockByHeight` — the bytes of the accepted block at `height`.
    ///
    /// # Errors
    /// Returns [`Error::Service`] if no block is indexed at the given height.
    pub fn get_block_by_height(&self, args: &GetBlockByHeightArgs) -> Result<GetBlockResponse> {
        let block_id = self
            .state
            .get_block_id_at_height(args.height)
            .ok_or_else(|| Error::Service(format!("no block at height {}", args.height)))?;
        let bytes = self.state.get_block(block_id)?;
        Ok(GetBlockResponse {
            block: hex_encode(&bytes),
            encoding: "hex".to_owned(),
        })
    }

    // -----------------------------------------------------------------------
    // `avm.getTx` (Go `service.go GetTx`)
    // -----------------------------------------------------------------------

    /// `avm.getTx` — the bytes of the accepted tx with `tx_id`.
    ///
    /// Returns the raw signed-tx bytes encoded as `0x<hex>` (Go
    /// `formatting.Encode(args.Encoding, tx.Bytes())`; Hex is the default).
    ///
    /// # Errors
    /// Returns [`Error::Database`] (with `NotFound`) if the tx is absent from
    /// the accepted state.
    pub fn get_tx(&self, args: &GetTxArgs) -> Result<GetTxReply> {
        if args.tx_id == Id::EMPTY {
            return Err(Error::NilTxId);
        }
        let bytes = self.state.get_tx(args.tx_id)?;
        Ok(GetTxReply {
            tx: hex_encode(&bytes),
            encoding: "hex".to_owned(),
        })
    }

    // -----------------------------------------------------------------------
    // `avm.getTxStatus` (Go `service.go GetTxStatus`)
    // -----------------------------------------------------------------------

    /// `avm.getTxStatus` — the status of `tx_id`.
    ///
    /// Returns `Accepted` if the tx is in the accepted state, `Unknown`
    /// otherwise. `Processing` (mempool / preferred chain) is **deferred** —
    /// it requires the VM mempool handle (see module docs). Mirrors Go:
    /// ```go
    /// switch err {
    /// case nil: reply.Status = choices.Accepted
    /// case database.ErrNotFound: reply.Status = choices.Unknown
    /// }
    /// ```
    ///
    /// # Errors
    /// Returns [`Error::Service`] for a nil `tx_id` (mirrors `errNilTxID`).
    pub fn get_tx_status(&self, args: &GetTxStatusArgs) -> Result<GetTxStatusReply> {
        if args.tx_id == Id::EMPTY {
            return Err(Error::NilTxId);
        }
        let status = match self.state.get_tx(args.tx_id) {
            Ok(_) => TxStatus::Accepted,
            // Go: `case database.ErrNotFound: reply.Status = choices.Unknown`
            // Any other error is propagated (mirrors Go's explicit not-found check).
            Err(Error::Database(ava_database::error::Error::NotFound)) => TxStatus::Unknown,
            Err(e) => return Err(e),
        };
        Ok(GetTxStatusReply { status })
    }

    // -----------------------------------------------------------------------
    // `avm.issueTx` (Go `service.go IssueTx`)
    // -----------------------------------------------------------------------

    /// Decodes + parses the `issueTx` wire payload into a [`Tx`] (Go
    /// `formatting.Decode(args.Encoding, args.Tx)` then
    /// `s.vm.parser.ParseTx(txBytes)`). The submit half lives behind the
    /// [`TxIssuer`] seam ([`RpcService::issue_tx`] joins the two).
    ///
    /// # Errors
    /// Returns [`Error::Service`] on a decode failure or [`Error::Codec`] if
    /// the bytes fail to parse.
    pub fn parse_tx(&self, args: &IssueTxArgs) -> Result<Tx> {
        let tx_bytes = decode_formatted_bytes(&args.tx, &args.encoding)?;
        Tx::parse(Codec(), &tx_bytes).map_err(Error::Codec)
    }

    /// `avm.issueTx` — parse tx bytes and return the tx id.
    ///
    /// **Parse-only half**: the wire transport ([`RpcService::issue_tx`])
    /// additionally submits the parsed tx through the [`TxIssuer`] seam (the
    /// VM's mempool admission path); this body alone is sufficient for client
    /// round-trip testing over a bare state snapshot.
    ///
    /// # Errors
    /// Returns [`Error::Service`]/[`Error::Codec`] if the bytes fail to
    /// decode/parse.
    pub fn issue_tx(&self, args: &IssueTxArgs) -> Result<IssueTxReply> {
        Ok(IssueTxReply {
            tx_id: self.parse_tx(args)?.id(),
        })
    }

    // -----------------------------------------------------------------------
    // `avm.getAssetDescription` (Go `service.go GetAssetDescription`)
    // -----------------------------------------------------------------------

    /// `avm.getAssetDescription` — the name, symbol, and denomination of the
    /// asset identified by `args.asset_id` (CB58 only; alias lookup deferred).
    ///
    /// Go: calls `s.vm.state.GetTx(assetID)` then asserts the tx is a
    /// `CreateAssetTx`.
    ///
    /// **Deferred**: alias string lookup (`s.vm.lookupAssetID`) needs the VM's
    /// alias store; only a raw CB58 asset id is accepted here.
    ///
    /// # Errors
    /// Returns [`Error::Codec`] if the asset id string fails to parse as CB58,
    /// [`Error::Database`] if the asset is not found, or
    /// [`Error::TxNotCreateAsset`] if the tx is not a `CreateAssetTx`.
    pub fn get_asset_description(
        &self,
        args: &GetAssetDescriptionArgs,
    ) -> Result<GetAssetDescriptionReply> {
        // Parse the CB58 asset id string.
        let asset_id = args
            .asset_id
            .parse::<Id>()
            .map_err(|e| Error::Service(format!("parse asset id: {e}")))?;
        let tx_bytes = self.state.get_tx(asset_id)?;
        let tx = Tx::parse(Codec(), &tx_bytes).map_err(Error::Codec)?;
        match tx.unsigned {
            crate::txs::UnsignedTx::CreateAsset(cat) => Ok(GetAssetDescriptionReply {
                asset_id,
                name: cat.name,
                symbol: cat.symbol,
                denomination: cat.denomination,
            }),
            _ => Err(Error::TxNotCreateAsset),
        }
    }

    // -----------------------------------------------------------------------
    // `avm.getUTXOs` (Go `service.go GetUTXOs`)
    // -----------------------------------------------------------------------

    /// `avm.getUTXOs` — paginated UTXOs referencing any of `args.addresses`,
    /// from this chain's state (M8.23b address → UTXO index) or, with
    /// `sourceChain`, from cross-chain shared memory (`avax.GetAtomicUTXOs`).
    ///
    /// Error strings mirror Go's `service.go GetUTXOs` byte-for-byte.
    ///
    /// # Errors
    /// [`Error::Api`] on empty/oversized address lists, unresolvable
    /// `sourceChain`, malformed cursor, or a retrieval failure.
    pub fn get_utxos(&self, args: &GetUTXOsArgs) -> Result<GetUTXOsReply> {
        if args.addresses.is_empty() {
            // Go `errNoAddresses`.
            return Err(Error::Api("no addresses provided".to_owned()));
        }
        if args.addresses.len() > MAX_GET_UTXOS_ADDRS {
            return Err(Error::Api(format!(
                "number of addresses given, {}, exceeds maximum, {}",
                args.addresses.len(),
                MAX_GET_UTXOS_ADDRS
            )));
        }

        let source_chain = if args.source_chain.is_empty() {
            self.chain_id
        } else {
            self.lookup_chain(&args.source_chain).map_err(|e| {
                Error::Api(format!(
                    "problem parsing source chainID {:?}: {e}",
                    args.source_chain
                ))
            })?
        };

        let addrs = self.parse_service_addresses(&args.addresses)?;

        let mut start_addr = ShortId::EMPTY;
        let mut start_utxo = Id::EMPTY;
        if !args.start_index.address.is_empty() || !args.start_index.utxo.is_empty() {
            start_addr = self
                .parse_service_address(&args.start_index.address)
                .map_err(|e| {
                    Error::Api(format!(
                        "couldn't parse start index address {:?}: {e}",
                        args.start_index.address
                    ))
                })?;
            start_utxo = args
                .start_index
                .utxo
                .parse::<Id>()
                .map_err(|e| Error::Api(format!("couldn't parse start index utxo: {e}")))?;
        }

        let mut limit = args.limit as usize;
        if limit == 0 || limit > MAX_PAGE_SIZE {
            limit = MAX_PAGE_SIZE;
        }

        let (utxos, end_addr, end_utxo_id) = if source_chain == self.chain_id {
            get_paginated_utxos(self.state.as_ref(), &addrs, start_addr, start_utxo, limit)
        } else {
            self.get_atomic_utxos(source_chain, &addrs, start_addr, start_utxo, limit)
        }
        .map_err(|e| Error::Api(format!("problem retrieving UTXOs: {e}")))?;

        let mut encoded = Vec::with_capacity(utxos.len());
        for utxo in &utxos {
            let bytes = utxo
                .marshal()
                .map_err(|e| Error::Api(format!("problem marshalling UTXO: {e}")))?;
            let s = encode_formatted_bytes(&bytes, &args.encoding).map_err(|e| {
                Error::Api(format!(
                    "couldn't encode UTXO {} as string: {e}",
                    utxo.input_id()
                ))
            })?;
            encoded.push(s);
        }

        let end_address = self
            .format_address(end_addr.as_bytes())
            .map_err(|e| Error::Api(format!("problem formatting address: {e}")))?;

        Ok(GetUTXOsReply {
            num_fetched: encoded.len() as u64,
            utxos: encoded,
            end_index: UtxoIndex {
                address: end_address,
                utxo: end_utxo_id.to_string(),
            },
            encoding: reply_encoding(&args.encoding).to_owned(),
        })
    }

    /// `avax.GetAtomicUTXOs` — paginated exported UTXOs from shared memory
    /// (`SharedMemory.Indexed` keyed by `source_chain`, traits = addresses).
    fn get_atomic_utxos(
        &self,
        source_chain: Id,
        addrs: &BTreeSet<ShortId>,
        start_addr: ShortId,
        start_utxo: Id,
        limit: usize,
    ) -> Result<(Vec<Utxo>, ShortId, Id)> {
        let Some(shared_memory) = &self.shared_memory else {
            return Err(Error::Api(
                "error fetching atomic UTXOs: shared memory unavailable".to_owned(),
            ));
        };
        let traits: Vec<Vec<u8>> = addrs.iter().map(|a| a.as_bytes().to_vec()).collect();
        let (values, last_trait, last_key) = shared_memory
            .indexed(
                source_chain,
                &traits,
                start_addr.as_bytes(),
                start_utxo.as_bytes(),
                limit,
            )
            .map_err(|e| Error::Api(format!("error fetching atomic UTXOs: {e}")))?;

        let last_addr = ShortId::from_slice(&last_trait).unwrap_or(ShortId::EMPTY);
        let last_utxo_id = Id::from_slice(&last_key).unwrap_or(Id::EMPTY);

        let mut utxos = Vec::with_capacity(values.len());
        for bytes in &values {
            let utxo = Utxo::unmarshal(bytes)
                .map_err(|e| Error::Api(format!("error parsing UTXO: {e}")))?;
            utxos.push(utxo);
        }
        Ok((utxos, last_addr, last_utxo_id))
    }

    // -----------------------------------------------------------------------
    // `avm.getBalance` (Go `service.go GetBalance`; deprecated but served)
    // -----------------------------------------------------------------------

    /// `avm.getBalance` — the balance of one asset held by an address.
    ///
    /// If `!args.include_partial`, counts only UTXOs held solely (1-of-1
    /// multisig) by the address with a locktime in the past; otherwise also
    /// partially-held / future-locked UTXOs (Go `GetBalance`). Only
    /// `secp256k1fx.TransferOutput`s count (Go's `TODO` downcast).
    ///
    /// # Errors
    /// [`Error::Api`] on a bad address/asset or a retrieval failure.
    pub fn get_balance(&self, args: &GetBalanceArgs) -> Result<GetBalanceReply> {
        let addr = self
            .parse_service_address(&args.address)
            .map_err(|e| Error::Api(format!("problem parsing address '{}': {e}", args.address)))?;
        let asset_id = self.lookup_asset_id(&args.asset_id)?;

        let addrs = BTreeSet::from([addr]);
        let utxos = get_all_utxos(self.state.as_ref(), &addrs)
            .map_err(|e| Error::Api(format!("problem retrieving UTXOs: {e}")))?;

        let now = self.unix_now();
        let mut balance: u64 = 0;
        let mut utxo_ids = Vec::with_capacity(utxos.len());
        for utxo in &utxos {
            if utxo.asset_id != asset_id {
                continue;
            }
            // Go TODO: not specific to *secp256k1fx.TransferOutput.
            let Output::SecpTransfer(transferable) = &utxo.out else {
                continue;
            };
            let owners = &transferable.owners;
            if !args.include_partial && (owners.addrs.len() != 1 || owners.locktime > now) {
                continue;
            }
            // Go `safemath.Add` — propagate the overflow error.
            balance = balance
                .checked_add(transferable.amt)
                .ok_or_else(|| Error::Api("overflow".to_owned()))?;
            utxo_ids.push(UtxoIdReply {
                tx_id: utxo.tx_id,
                output_index: utxo.output_index,
            });
        }

        Ok(GetBalanceReply { balance, utxo_ids })
    }

    // -----------------------------------------------------------------------
    // `avm.getAllBalances` (Go `service.go GetAllBalances`; deprecated)
    // -----------------------------------------------------------------------

    /// `avm.getAllBalances` — the balance of every asset held by an address
    /// (same `include_partial` semantics as [`get_balance`](Self::get_balance)).
    ///
    /// Determinism note: Go iterates a `set.Set` (random order); the reply here
    /// is sorted by asset id (00 §6.1). The per-entry `asset` string is
    /// `PrimaryAliasOrDefault` — without a VM asset aliaser (recorded deferral)
    /// it is always the CB58 asset id.
    ///
    /// # Errors
    /// [`Error::Api`] on a bad address or a retrieval failure.
    pub fn get_all_balances(&self, args: &GetAllBalancesArgs) -> Result<GetAllBalancesReply> {
        let addr = self
            .parse_service_address(&args.address)
            .map_err(|e| Error::Api(format!("problem parsing address '{}': {e}", args.address)))?;
        let addrs = BTreeSet::from([addr]);

        let utxos = get_all_utxos(self.state.as_ref(), &addrs)
            .map_err(|e| Error::Api(format!("couldn't get address's UTXOs: {e}")))?;

        let now = self.unix_now();
        let mut balances: BTreeMap<Id, u64> = BTreeMap::new();
        for utxo in &utxos {
            let Output::SecpTransfer(transferable) = &utxo.out else {
                continue;
            };
            let owners = &transferable.owners;
            if !args.include_partial && (owners.addrs.len() != 1 || owners.locktime > now) {
                continue;
            }
            let entry = balances.entry(utxo.asset_id).or_insert(0);
            // Go: on overflow the balance saturates at MaxUint64.
            *entry = entry.saturating_add(transferable.amt);
        }

        Ok(GetAllBalancesReply {
            balances: balances
                .into_iter()
                .map(|(asset_id, balance)| AssetBalance {
                    asset_id: asset_id.to_string(),
                    balance,
                })
                .collect(),
        })
    }

    // -----------------------------------------------------------------------
    // `avm.getTxFee` (Go `service.go GetTxFee`)
    // -----------------------------------------------------------------------

    /// `avm.getTxFee` — the static fee schedule (`Config.TxFee` /
    /// `Config.CreateAssetTxFee`), as quoted `json.Uint64` strings.
    #[must_use]
    pub fn get_tx_fee(&self) -> GetTxFeeReply {
        GetTxFeeReply {
            tx_fee: self.tx_fee,
            create_asset_tx_fee: self.create_asset_tx_fee,
        }
    }
}

// ---------------------------------------------------------------------------
// UTXO fetching (`vms/components/avax/utxo_fetching.go`)
// ---------------------------------------------------------------------------

/// `maxGetUTXOsAddrs` — max addresses per `getUTXOs` call.
const MAX_GET_UTXOS_ADDRS: usize = 1024;
/// `maxPageSize` — max UTXOs per page.
const MAX_PAGE_SIZE: usize = 1024;

/// `avax.GetPaginatedUTXOs` — UTXOs referencing any address in `addrs`,
/// resuming after the `(last_addr, last_utxo_id)` cursor, at most `limit`.
///
/// Returns `(utxos, last_address_searched, last_utxo_id_searched)` — the
/// cursor reflects the last *searched* (not necessarily returned) position,
/// exactly like Go.
///
/// # Errors
/// Wraps index/UTXO read failures with Go's message strings.
fn get_paginated_utxos(
    state: &dyn ReadOnlyChain,
    addrs: &BTreeSet<ShortId>,
    mut last_addr: ShortId,
    mut last_utxo_id: Id,
    limit: usize,
) -> Result<(Vec<Utxo>, ShortId, Id)> {
    let mut utxos = Vec::new();
    let mut seen: BTreeSet<Id> = BTreeSet::new();
    let search_size = limit; // the limit diminishes; the search size does not
    let mut remaining = limit;

    // `addrs` iterates sorted (BTreeSet) — Go sorts `addrsList` explicitly.
    for &addr in addrs {
        // Skip addresses before the cursor; resume mid-address at the cursor.
        let start = match addr.cmp(&last_addr) {
            std::cmp::Ordering::Less => continue,
            std::cmp::Ordering::Equal => last_utxo_id,
            std::cmp::Ordering::Greater => Id::EMPTY,
        };

        last_addr = addr; // the last address searched

        let utxo_ids = state
            .utxo_ids(&addr, start, search_size)
            .map_err(|e| Error::Api(format!("couldn't get UTXOs for address {addr}: {e}")))?;
        for utxo_id in utxo_ids {
            last_utxo_id = utxo_id; // the last searched UTXO — not the last found

            if seen.contains(&utxo_id) {
                continue;
            }

            let bytes = state
                .get_utxo(utxo_id)
                .map_err(|e| Error::Api(format!("couldn't get UTXO {utxo_id}: {e}")))?;
            let utxo = Utxo::unmarshal(&bytes)
                .map_err(|e| Error::Api(format!("couldn't get UTXO {utxo_id}: {e}")))?;

            utxos.push(utxo);
            seen.insert(utxo_id);
            remaining = remaining.saturating_sub(1);
            if remaining == 0 {
                return Ok((utxos, last_addr, last_utxo_id));
            }
        }
    }
    Ok((utxos, last_addr, last_utxo_id))
}

/// `avax.GetAllUTXOs` — every UTXO referencing any address in `addrs`.
///
/// # Errors
/// Propagates [`get_paginated_utxos`] failures.
fn get_all_utxos(state: &dyn ReadOnlyChain, addrs: &BTreeSet<ShortId>) -> Result<Vec<Utxo>> {
    get_paginated_utxos(state, addrs, ShortId::EMPTY, Id::EMPTY, usize::MAX)
        .map(|(utxos, _, _)| utxos)
}

// ---------------------------------------------------------------------------
// The JSON-RPC wire layer (M8.22) — gorilla `avm.*` over the local shim
// ---------------------------------------------------------------------------

/// The empty gorilla args object (Go `*struct{}`): `[]` / absent / `[{}]` all
/// accept.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EmptyArgs {}

/// The `issueTx` submission seam (the second half of Go `Service.IssueTx` →
/// `vm.issueTxFromRPC`): admit a parsed [`Tx`] for inclusion. The VM
/// implements it over the shared mempool via the SAME admission path inbound
/// gossip uses (dedupe → verify → add); a no-op/recording impl serves tests.
///
/// Outbound re-gossip of the admitted tx is a recorded deferral (the live
/// `Network::gossip` transport is an M8 handoff; see `tests/PORTING.md`).
pub trait TxIssuer: Send + Sync {
    /// Admits `tx` for inclusion in a future block.
    ///
    /// # Errors
    /// A human-readable rejection (duplicate / verification / mempool bounds),
    /// surfaced to the client as a `-32000` server error (Go returns the
    /// mempool error the same way).
    fn issue_tx(&self, tx: Tx) -> std::result::Result<(), String>;
}

/// Maps an X-Chain domain error onto the gorilla `-32000` server error (the
/// `utils/rpc` handler surfaces Go handler errors the same way, 14 §16.1).
fn server_err(e: Error) -> RpcError {
    RpcError::server(e.to_string())
}

/// The gorilla `avm` service wrapper over [`Service`] (Go `avm.Service`,
/// registered as `"avm"` by `CreateHandlers`, `vm.go:302`). Bridges the typed
/// handler bodies; the Go method set inventory vs the bridged set lives in
/// `tests/PORTING.md` (M8.23 owns full parity).
pub struct RpcService {
    service: Arc<Service>,
    issuer: Arc<dyn TxIssuer>,
}

#[rpc_service("avm")]
impl RpcService {
    /// `avm.getHeight` (Go `Service.GetHeight`, `service.go:156`).
    ///
    /// # Errors
    /// `-32000` if the last-accepted block cannot be read/parsed.
    pub async fn get_height(
        &self,
        _args: EmptyArgs,
    ) -> std::result::Result<GetHeightResponse, RpcError> {
        self.service.get_height().map_err(server_err)
    }

    /// `avm.getBlock` (Go `Service.GetBlock`, `service.go:54`).
    ///
    /// # Errors
    /// `-32000` for an absent block.
    pub async fn get_block(
        &self,
        args: GetBlockArgs,
    ) -> std::result::Result<GetBlockResponse, RpcError> {
        self.service.get_block(&args).map_err(server_err)
    }

    /// `avm.getBlockByHeight` (Go `Service.GetBlockByHeight`, `service.go:101`).
    ///
    /// # Errors
    /// `-32000` for a missing height.
    pub async fn get_block_by_height(
        &self,
        args: GetBlockByHeightArgs,
    ) -> std::result::Result<GetBlockResponse, RpcError> {
        self.service.get_block_by_height(&args).map_err(server_err)
    }

    /// `avm.getTx` (Go `Service.GetTx`, `service.go:244`).
    ///
    /// # Errors
    /// `-32000` for the nil tx id / an absent tx.
    pub async fn get_tx(&self, args: GetTxArgs) -> std::result::Result<GetTxReply, RpcError> {
        self.service.get_tx(&args).map_err(server_err)
    }

    /// `avm.getTxStatus` (Go `Service.GetTxStatus`, `service.go:217`;
    /// deprecated in Go but still served).
    ///
    /// # Errors
    /// `-32000` for the nil tx id (Go `errNilTxID`).
    pub async fn get_tx_status(
        &self,
        args: GetTxStatusArgs,
    ) -> std::result::Result<GetTxStatusReply, RpcError> {
        self.service.get_tx_status(&args).map_err(server_err)
    }

    /// `avm.issueTx` (Go `Service.IssueTx`, `service.go:184`): decode + parse
    /// (the typed body) then submit through the [`TxIssuer`] seam — the same
    /// dedupe → verify → mempool-add path inbound gossip uses. Outbound
    /// re-gossip is a recorded deferral (live `Network::gossip`, M8).
    ///
    /// # Errors
    /// `-32000` on decode/parse failures or a mempool rejection.
    pub async fn issue_tx(&self, args: IssueTxArgs) -> std::result::Result<IssueTxReply, RpcError> {
        let tx = self.service.parse_tx(&args).map_err(server_err)?;
        let tx_id = tx.id();
        self.issuer.issue_tx(tx).map_err(RpcError::server)?;
        Ok(IssueTxReply { tx_id })
    }

    /// `avm.getAssetDescription` (Go `Service.GetAssetDescription`,
    /// `service.go:403`; alias lookup is a recorded deferral).
    ///
    /// # Errors
    /// `-32000` for a bad/unknown asset id or a non-`CreateAssetTx`.
    pub async fn get_asset_description(
        &self,
        args: GetAssetDescriptionArgs,
    ) -> std::result::Result<GetAssetDescriptionReply, RpcError> {
        self.service
            .get_asset_description(&args)
            .map_err(server_err)
    }

    /// `avm.getUTXOs` (Go `Service.GetUTXOs`, `service.go:285`): paginated
    /// address-indexed UTXOs, incl. the cross-chain `sourceChain` atomic path
    /// (M8.23b).
    ///
    /// # Errors
    /// `-32000` carrying the Go handler error strings (bad addresses / cursor /
    /// source chain / retrieval failures).
    #[rpc(name = "GetUTXOs")]
    pub async fn get_utxos(
        &self,
        args: GetUTXOsArgs,
    ) -> std::result::Result<GetUTXOsReply, RpcError> {
        self.service.get_utxos(&args).map_err(server_err)
    }

    /// `avm.getBalance` (Go `Service.GetBalance`, `service.go:453`; deprecated
    /// in Go but still served).
    ///
    /// # Errors
    /// `-32000` for a bad address/asset or a retrieval failure.
    pub async fn get_balance(
        &self,
        args: GetBalanceArgs,
    ) -> std::result::Result<GetBalanceReply, RpcError> {
        self.service.get_balance(&args).map_err(server_err)
    }

    /// `avm.getAllBalances` (Go `Service.GetAllBalances`, `service.go:530`;
    /// deprecated in Go but still served).
    ///
    /// # Errors
    /// `-32000` for a bad address or a retrieval failure.
    pub async fn get_all_balances(
        &self,
        args: GetAllBalancesArgs,
    ) -> std::result::Result<GetAllBalancesReply, RpcError> {
        self.service.get_all_balances(&args).map_err(server_err)
    }

    /// `avm.getTxFee` (Go `Service.GetTxFee`, `service.go:594`): the static
    /// fee schedule.
    ///
    /// # Errors
    /// Infallible (the `Result` is the bridge signature).
    pub async fn get_tx_fee(
        &self,
        _args: EmptyArgs,
    ) -> std::result::Result<GetTxFeeReply, RpcError> {
        Ok(self.service.get_tx_fee())
    }
}

/// Builds the registry serving the bridged `avm.*` methods (the body of Go's
/// `rpcServer.RegisterService(&Service{vm}, "avm")`, `vm.go:302`).
#[must_use]
pub fn registry(service: Arc<Service>, issuer: Arc<dyn TxIssuer>) -> ServiceRegistry {
    let mut registry = ServiceRegistry::new();
    Arc::new(RpcService { service, issuer }).register_rpc(&mut registry);
    registry
}

// ---------------------------------------------------------------------------
// Byte-decoding helper
// ---------------------------------------------------------------------------

/// Decodes a formatted byte string for `issueTx`.
///
/// Mirrors Go's `formatting.Decode(args.Encoding, args.Tx)`. Valid encodings
/// are `"hex"` (default / zero value; checksummed — strips and verifies the
/// trailing 4-byte checksum), `"hexc"` (alias for checksummed hex), and
/// `"hexnc"` (no-checksum hex). Empty/omitted `encoding` defaults to `"hex"`
/// because `formatting.Encoding`'s zero value is `Hex` (NOT CB58; Go's AVM
/// service does not accept CB58 in the encoding field).
///
/// # Errors
/// Returns [`Error::Service`] on decode failure, missing `0x` prefix, bad
/// checksum, or unsupported encoding name.
fn decode_formatted_bytes(s: &str, encoding: &str) -> Result<Vec<u8>> {
    let enc = encoding.to_lowercase();
    match enc.as_str() {
        // `"hex"` and `"hexc"` — checksummed hex (Go `Hex` / `HexC`)
        "hex" | "hexc" | "" => {
            if !s.starts_with("0x") && !s.starts_with("0X") {
                return Err(Error::Service("hex decode: missing 0x prefix".to_owned()));
            }
            let hex_str = s.trim_start_matches("0x").trim_start_matches("0X");
            let decoded =
                hex::decode(hex_str).map_err(|e| Error::Service(format!("hex decode: {e}")))?;
            // Strip and verify the 4-byte checksum (Go `errMissingChecksum` /
            // `errBadChecksum`; `utils/formatting/encoding.go`).
            let split_at = decoded.len().checked_sub(4).ok_or_else(|| {
                Error::Service("hex decode: input is too short to contain a checksum".to_owned())
            })?;
            let (raw, cs) = decoded.split_at(split_at);
            let expected = checksum(raw, 4);
            if cs != expected.as_slice() {
                return Err(Error::Service("hex decode: invalid checksum".to_owned()));
            }
            Ok(raw.to_vec())
        }
        // `"hexnc"` — no checksum (Go `HexNC`)
        "hexnc" => {
            if !s.starts_with("0x") && !s.starts_with("0X") {
                return Err(Error::Service("hexnc decode: missing 0x prefix".to_owned()));
            }
            let hex_str = s.trim_start_matches("0x").trim_start_matches("0X");
            hex::decode(hex_str).map_err(|e| Error::Service(format!("hexnc decode: {e}")))
        }
        other => Err(Error::Service(format!(
            "unsupported encoding: '{other}'; valid values are 'hex', 'hexc', 'hexnc'"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Conformance tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod conformance {
    //! Inline conformance tests asserting the JSON field names, encodings, and
    //! round-trips against the recorded Go `vms/avm/service.go` shapes.
    //!
    //! All tests build a `Service` over a `State<MemDb>` seeded with a block
    //! and/or tx, then assert `serde_json::to_value(reply)` field shapes.

    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use ava_database::MemDb;
    use ava_types::id::Id;

    use super::*;
    use crate::block::{Block, BlockBody, StandardBlock};
    use crate::state::State;
    use crate::state::chain::Chain;
    use crate::txs::codec::Codec;
    use crate::txs::components::AvaxBaseTx;
    use crate::txs::{BaseTx, Tx, UnsignedTx};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Builds a minimal `State<MemDb>` with one accepted block and one accepted
    /// tx, then wraps it in an `Arc` and returns the block id / tx id alongside
    /// the service.
    fn seeded_service() -> (Service, Id, Id) {
        let c = Codec();

        // Build a minimal tx (BaseTx with empty base fields).
        let mut tx = Tx::new(UnsignedTx::Base(BaseTx {
            base: AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![],
                memo: vec![],
            },
        }));
        tx.initialize(c).expect("init tx");
        let tx_id = tx.id();

        // Build a block containing the tx.
        let parent_id = Id::from([0xAB; 32]);
        let mut blk = Block::new(BlockBody::Standard(StandardBlock {
            parent_id,
            height: 1,
            time: 1_600_000_000,
            root: Id::EMPTY,
            transactions: vec![tx.clone()],
        }));
        blk.initialize(c).expect("init block");
        let blk_id = blk.id();

        // Seed the state.
        let mut state = State::new(Arc::new(MemDb::new())).expect("state");
        state.add_block(blk_id, 1, blk.bytes().to_vec());
        state.add_tx(tx_id, tx.bytes().to_vec());
        state.set_last_accepted(blk_id);
        state.set_timestamp(UNIX_EPOCH + Duration::from_secs(1_600_000_000));
        state.commit().expect("commit");

        let service = Service::new(Arc::new(state), 1);
        (service, blk_id, tx_id)
    }

    // -----------------------------------------------------------------------
    // `getHeight`
    // -----------------------------------------------------------------------

    #[test]
    fn service_get_height_shape() {
        let (service, _, _) = seeded_service();
        let reply = service.get_height().expect("get_height");

        // Height must be 1 (the seeded block).
        assert_eq!(reply.height, 1);

        // JSON: { "height": "1" } — Go `avajson.Uint64` quoted string.
        let j = serde_json::to_value(&reply).expect("json");
        assert_eq!(j["height"], serde_json::json!("1"));
    }

    #[test]
    fn service_get_height_empty_chain() {
        // A fresh state (no accepted block) defaults to height 0.
        let state = State::new(Arc::new(MemDb::new())).expect("state");
        let service = Service::new(Arc::new(state), 1);
        let reply = service.get_height().expect("get_height");
        assert_eq!(reply.height, 0);
        let j = serde_json::to_value(&reply).expect("json");
        assert_eq!(j["height"], serde_json::json!("0"));
    }

    // -----------------------------------------------------------------------
    // `getBlock`
    // -----------------------------------------------------------------------

    #[test]
    fn service_get_block_returns_hex() {
        let (service, blk_id, _) = seeded_service();
        let args = GetBlockArgs {
            block_id: blk_id,
            encoding: String::new(),
        };
        let reply = service.get_block(&args).expect("get_block");

        // Block bytes encoded as `0x<hex>`.
        assert!(reply.block.starts_with("0x"), "block should start with 0x");
        assert_eq!(reply.encoding, "hex");

        // JSON field names match Go `api.GetBlockResponse`.
        let j = serde_json::to_value(&reply).expect("json");
        assert!(j["block"].as_str().unwrap().starts_with("0x"));
        assert_eq!(j["encoding"], serde_json::json!("hex"));
    }

    #[test]
    fn service_get_block_not_found() {
        let (service, _, _) = seeded_service();
        let args = GetBlockArgs {
            block_id: Id::from([0xFF; 32]),
            encoding: String::new(),
        };
        // Missing block → Database(NotFound) error.
        assert!(service.get_block(&args).is_err());
    }

    // -----------------------------------------------------------------------
    // `getBlockByHeight`
    // -----------------------------------------------------------------------

    #[test]
    fn service_get_block_by_height_shape() {
        let (service, blk_id, _) = seeded_service();

        let args = GetBlockByHeightArgs {
            height: 1,
            encoding: String::new(),
        };
        let reply = service
            .get_block_by_height(&args)
            .expect("get_block_by_height");

        // The same block bytes we'd get from getBlock.
        let args2 = GetBlockArgs {
            block_id: blk_id,
            encoding: String::new(),
        };
        let reply2 = service.get_block(&args2).expect("get_block");
        assert_eq!(reply.block, reply2.block);
        assert_eq!(reply.encoding, "hex");

        // JSON: { "block": "0x…", "encoding": "hex" }.
        let j = serde_json::to_value(&reply).expect("json");
        assert!(j["block"].as_str().unwrap().starts_with("0x"));
        assert_eq!(j["encoding"], serde_json::json!("hex"));
    }

    #[test]
    fn service_get_block_by_height_missing() {
        let (service, _, _) = seeded_service();
        let args = GetBlockByHeightArgs {
            height: 999,
            encoding: String::new(),
        };
        assert!(service.get_block_by_height(&args).is_err());
    }

    // -----------------------------------------------------------------------
    // `getTx`
    // -----------------------------------------------------------------------

    #[test]
    fn service_get_tx_shape() {
        let (service, _, tx_id) = seeded_service();

        let args = GetTxArgs {
            tx_id,
            encoding: String::new(),
        };
        let reply = service.get_tx(&args).expect("get_tx");

        // Tx bytes as `0x<hex>`.
        assert!(reply.tx.starts_with("0x"), "tx should start with 0x");
        assert_eq!(reply.encoding, "hex");

        // JSON field names match Go `api.GetTxReply`.
        let j = serde_json::to_value(&reply).expect("json");
        assert!(j["tx"].as_str().unwrap().starts_with("0x"));
        assert_eq!(j["encoding"], serde_json::json!("hex"));
    }

    #[test]
    fn service_get_tx_nil_id_error() {
        let (service, _, _) = seeded_service();
        let args = GetTxArgs {
            tx_id: Id::EMPTY,
            encoding: String::new(),
        };
        // nil tx id → `Error::NilTxId`.
        assert!(service.get_tx(&args).is_err());
    }

    #[test]
    fn service_get_tx_not_found() {
        let (service, _, _) = seeded_service();
        let args = GetTxArgs {
            tx_id: Id::from([0xEE; 32]),
            encoding: String::new(),
        };
        assert!(service.get_tx(&args).is_err());
    }

    // -----------------------------------------------------------------------
    // `getTxStatus`
    // -----------------------------------------------------------------------

    #[test]
    fn service_get_tx_status_accepted() {
        let (service, _, tx_id) = seeded_service();
        let args = GetTxStatusArgs { tx_id };
        let reply = service.get_tx_status(&args).expect("get_tx_status");

        assert_eq!(reply.status, TxStatus::Accepted);

        // JSON: { "status": "Accepted" } — Go `choices.Status.String()`.
        let j = serde_json::to_value(&reply).expect("json");
        assert_eq!(j["status"], serde_json::json!("Accepted"));
    }

    #[test]
    fn service_get_tx_status_unknown() {
        let (service, _, _) = seeded_service();
        let args = GetTxStatusArgs {
            tx_id: Id::from([0xDD; 32]),
        };
        let reply = service.get_tx_status(&args).expect("get_tx_status");

        assert_eq!(reply.status, TxStatus::Unknown);

        let j = serde_json::to_value(&reply).expect("json");
        assert_eq!(j["status"], serde_json::json!("Unknown"));
    }

    #[test]
    fn service_get_tx_status_nil_id_error() {
        let (service, _, _) = seeded_service();
        let args = GetTxStatusArgs { tx_id: Id::EMPTY };
        assert!(service.get_tx_status(&args).is_err());
    }

    // -----------------------------------------------------------------------
    // `issueTx`
    // -----------------------------------------------------------------------

    #[test]
    fn service_issue_tx_roundtrip() {
        let (service, _, tx_id) = seeded_service();

        // Recover the tx bytes from state.  `get_tx` returns checksummed hex:
        // `0x<hex(raw ++ sha256(raw)[28..32])>` — matching Go's `formatting.Hex`.
        let args = GetTxArgs {
            tx_id,
            encoding: String::new(),
        };
        let tx_reply = service.get_tx(&args).expect("get_tx");

        // The encoded string must start with "0x" and be at least 2 + (4 * 2) = 10
        // chars (the 4-byte checksum alone would be "0x" + 8 hex digits).
        assert!(tx_reply.tx.starts_with("0x"), "hex must start with 0x");
        // Strip "0x" and verify the hex portion's byte count includes 4 checksum bytes.
        let hex_body = tx_reply.tx.trim_start_matches("0x");
        assert!(
            hex_body.len() >= 8,
            "hex body must have at least 4 checksum bytes (8 hex chars)"
        );
        // The hex body length must be even (valid hex).
        assert_eq!(hex_body.len() % 2, 0, "hex body length must be even");

        // issueTx with the checksummed hex → same tx_id back (closed loop).
        let issue_args = IssueTxArgs {
            tx: tx_reply.tx.clone(),
            encoding: "hex".to_owned(),
        };
        let issue_reply = service.issue_tx(&issue_args).expect("issue_tx");
        assert_eq!(issue_reply.tx_id, tx_id);

        // JSON field name: `txID` (CB58).
        let j = serde_json::to_value(&issue_reply).expect("json");
        assert!(j["txID"].as_str().is_some(), "txID should be a CB58 string");
    }

    #[test]
    fn service_issue_tx_invalid_hex_error() {
        let (service, _, _) = seeded_service();
        let args = IssueTxArgs {
            tx: "0xZZZZNOTHEX".to_owned(),
            encoding: "hex".to_owned(),
        };
        assert!(service.issue_tx(&args).is_err());
    }

    // -----------------------------------------------------------------------
    // `getAssetDescription`
    // -----------------------------------------------------------------------

    #[test]
    fn service_get_asset_description_shape() {
        // Seed a state with a CreateAssetTx.
        let c = Codec();
        let mut tx = Tx::new(UnsignedTx::CreateAsset(crate::txs::CreateAssetTx {
            base: BaseTx {
                base: AvaxBaseTx {
                    network_id: 1,
                    blockchain_id: Id::EMPTY,
                    outs: vec![],
                    ins: vec![],
                    memo: vec![],
                },
            },
            name: "TestAsset".to_owned(),
            symbol: "TEST".to_owned(),
            denomination: 9,
            states: vec![],
        }));
        tx.initialize(c).expect("init tx");
        let asset_id = tx.id();
        let asset_id_str = asset_id.to_string(); // CB58

        let mut state = State::new(Arc::new(MemDb::new())).expect("state");
        state.add_tx(asset_id, tx.bytes().to_vec());
        state.set_last_accepted(Id::from([0xAB; 32]));
        state.commit().expect("commit");

        let service = Service::new(Arc::new(state), 1);
        let args = GetAssetDescriptionArgs {
            asset_id: asset_id_str,
        };
        let reply = service
            .get_asset_description(&args)
            .expect("get_asset_description");

        assert_eq!(reply.asset_id, asset_id);
        assert_eq!(reply.name, "TestAsset");
        assert_eq!(reply.symbol, "TEST");
        assert_eq!(reply.denomination, 9);

        // JSON field names + encodings.
        let j = serde_json::to_value(&reply).expect("json");
        assert!(
            j["assetID"].as_str().is_some(),
            "assetID must be a CB58 string"
        );
        assert_eq!(j["name"], serde_json::json!("TestAsset"));
        assert_eq!(j["symbol"], serde_json::json!("TEST"));
        // denomination serialized as a quoted string (avajson.Uint8).
        assert_eq!(j["denomination"], serde_json::json!("9"));
    }

    #[test]
    fn service_get_asset_description_not_create_asset() {
        // A plain BaseTx is not a CreateAssetTx → TxNotCreateAsset.
        let (service, _, tx_id) = seeded_service();
        let tx_id_str = tx_id.to_string();
        let args = GetAssetDescriptionArgs {
            asset_id: tx_id_str,
        };
        assert!(service.get_asset_description(&args).is_err());
    }

    // -----------------------------------------------------------------------
    // `getUTXOs` / `getBalance` / `getAllBalances` — M8.23b live handlers
    // -----------------------------------------------------------------------

    use ava_secp256k1fx::{OutputOwners, TransferOutput};
    use ava_utils::clock::MockClock;

    /// The fixed "now" for locktime checks (matches the seeded chain time).
    const NOW_SECS: u64 = 1_600_000_000;

    fn short(seed: u8) -> ShortId {
        ShortId::from_slice(&[seed; 20]).expect("short id")
    }

    /// A canonical `avax.UTXO` carrying a secp `TransferOutput`.
    fn make_utxo(
        tx_seed: u8,
        out_index: u32,
        asset_seed: u8,
        amt: u64,
        locktime: u64,
        addr_seeds: &[u8],
    ) -> Utxo {
        Utxo {
            tx_id: Id::from([tx_seed; 32]),
            output_index: out_index,
            asset_id: Id::from([asset_seed; 32]),
            out: Output::SecpTransfer(TransferOutput::new(
                amt,
                OutputOwners::new(locktime, 1, addr_seeds.iter().map(|&a| short(a)).collect()),
            )),
        }
    }

    /// A service over a state seeded with `utxos`, pinned to `NOW_SECS`.
    fn utxo_service(utxos: &[Utxo]) -> Service {
        let mut state = State::new(Arc::new(MemDb::new())).expect("state");
        for utxo in utxos {
            state.add_utxo(utxo.input_id(), utxo.marshal().expect("marshal"));
        }
        state.commit().expect("commit");
        Service::new(Arc::new(state), 1).with_clock(Arc::new(MockClock::at(
            UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        )))
    }

    /// The X-bech32 form of a seeded test address.
    fn bech32(service: &Service, seed: u8) -> String {
        service
            .format_address(short(seed).as_bytes())
            .expect("format")
    }

    #[test]
    fn service_get_utxos_no_addresses_error() {
        let service = utxo_service(&[]);
        let err = service
            .get_utxos(&GetUTXOsArgs::default())
            .expect_err("getUTXOs([])");
        // Go `errNoAddresses` — byte-equal.
        assert_eq!(err.to_string(), "no addresses provided");
    }

    #[test]
    fn service_get_utxos_too_many_addresses_error() {
        let service = utxo_service(&[]);
        let args = GetUTXOsArgs {
            addresses: vec!["X-avax1test".to_owned(); 1025],
            ..Default::default()
        };
        let err = service.get_utxos(&args).expect_err("getUTXOs(1025 addrs)");
        assert_eq!(
            err.to_string(),
            "number of addresses given, 1025, exceeds maximum, 1024"
        );
    }

    #[test]
    fn service_get_utxos_bad_source_chain_error() {
        let service = utxo_service(&[]);
        let args = GetUTXOsArgs {
            addresses: vec!["X-avax1test".to_owned()],
            source_chain: "P".to_owned(),
            ..Default::default()
        };
        let err = service
            .get_utxos(&args)
            .expect_err("getUTXOs(sourceChain=P)");
        // Go: `problem parsing source chainID %q: %w` over the aliaser error.
        assert_eq!(
            err.to_string(),
            "problem parsing source chainID \"P\": there is no ID with alias P"
        );
    }

    #[test]
    fn service_get_utxos_bad_start_index_utxo_error() {
        let utxo = make_utxo(1, 0, 0xAA, 100, 0, &[1]);
        let service = utxo_service(&[utxo]);
        let args = GetUTXOsArgs {
            addresses: vec![bech32(&service, 1)],
            start_index: UtxoIndex {
                address: bech32(&service, 1),
                utxo: "!!!not-cb58!!!".to_owned(),
            },
            ..Default::default()
        };
        let err = service.get_utxos(&args).expect_err("bad cursor utxo");
        assert!(
            err.to_string()
                .starts_with("couldn't parse start index utxo: "),
            "Go wrap prefix; got: {err}"
        );
    }

    #[test]
    fn service_get_utxos_local_shape() {
        // addr 1 owns two UTXOs; addr 2 owns one.
        let u1 = make_utxo(1, 0, 0xAA, 100, 0, &[1]);
        let u2 = make_utxo(2, 0, 0xAA, 200, 0, &[1]);
        let u3 = make_utxo(3, 0, 0xAA, 300, 0, &[2]);
        let service = utxo_service(&[u1.clone(), u2.clone(), u3]);

        let args = GetUTXOsArgs {
            addresses: vec![bech32(&service, 1)],
            ..Default::default()
        };
        let reply = service.get_utxos(&args).expect("get_utxos");

        assert_eq!(reply.num_fetched, 2, "addr 1 has two UTXOs");
        assert_eq!(reply.utxos.len(), 2);
        // Each entry is checksummed hex of the canonical UTXO bytes.
        let mut expected: Vec<String> = vec![
            hex_encode(&u1.marshal().expect("marshal")),
            hex_encode(&u2.marshal().expect("marshal")),
        ];
        expected.sort();
        let mut got = reply.utxos.clone();
        got.sort();
        assert_eq!(got, expected, "canonical UTXO bytes, checksummed hex");

        // endIndex: the last searched address (bech32) + UTXO id (CB58).
        assert_eq!(reply.end_index.address, bech32(&service, 1));
        assert!(reply.end_index.utxo.parse::<Id>().is_ok(), "CB58 end utxo");
        assert_eq!(reply.encoding, "hex");

        // JSON shape: numFetched is a quoted json.Uint64.
        let j = serde_json::to_value(&reply).expect("json");
        assert_eq!(j["numFetched"], serde_json::json!("2"));
        assert!(j["utxos"].as_array().is_some());
        assert!(j["endIndex"]["address"].as_str().is_some());
        assert!(j["endIndex"]["utxo"].as_str().is_some());
        assert_eq!(j["encoding"], serde_json::json!("hex"));
    }

    #[test]
    fn service_get_utxos_pagination_cursor() {
        let u1 = make_utxo(1, 0, 0xAA, 100, 0, &[1]);
        let u2 = make_utxo(2, 0, 0xAA, 200, 0, &[1]);
        let u3 = make_utxo(3, 0, 0xAA, 300, 0, &[1]);
        let service = utxo_service(&[u1, u2, u3]);

        // Page 1: limit 2.
        let args = GetUTXOsArgs {
            addresses: vec![bech32(&service, 1)],
            limit: 2,
            ..Default::default()
        };
        let page1 = service.get_utxos(&args).expect("page 1");
        assert_eq!(page1.num_fetched, 2);

        // Page 2: resume from endIndex.
        let args2 = GetUTXOsArgs {
            addresses: vec![bech32(&service, 1)],
            start_index: page1.end_index.clone(),
            ..Default::default()
        };
        let page2 = service.get_utxos(&args2).expect("page 2");
        assert_eq!(page2.num_fetched, 1, "one UTXO remains");

        // No overlap; union covers all three.
        let mut all: Vec<String> = page1
            .utxos
            .iter()
            .chain(page2.utxos.iter())
            .cloned()
            .collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 3, "pages are disjoint and complete");
    }

    #[test]
    fn service_get_utxos_hexnc_encoding() {
        let u1 = make_utxo(1, 0, 0xAA, 100, 0, &[1]);
        let service = utxo_service(std::slice::from_ref(&u1));
        let args = GetUTXOsArgs {
            addresses: vec![bech32(&service, 1)],
            encoding: "hexnc".to_owned(),
            ..Default::default()
        };
        let reply = service.get_utxos(&args).expect("get_utxos hexnc");
        assert_eq!(
            reply.utxos,
            vec![format!("0x{}", hex::encode(u1.marshal().expect("marshal")))],
            "hexnc carries no checksum"
        );
        assert_eq!(reply.encoding, "hexnc");
    }

    /// A `SharedMemory` stub serving fixed atomic UTXOs for the `sourceChain`
    /// path (the `Indexed` half of `avax.GetAtomicUTXOs`).
    struct FixedSharedMemory {
        values: Vec<Vec<u8>>,
        last_trait: Vec<u8>,
        last_key: Vec<u8>,
    }

    impl ava_vm::components::avax::shared_memory::SharedMemory for FixedSharedMemory {
        fn get(&self, _peer_chain: Id, _keys: &[Vec<u8>]) -> ava_vm::error::Result<Vec<Vec<u8>>> {
            Ok(vec![])
        }
        fn indexed(
            &self,
            _peer_chain: Id,
            _traits: &[Vec<u8>],
            _start_trait: &[u8],
            _start_key: &[u8],
            _limit: usize,
        ) -> ava_vm::error::Result<(Vec<Vec<u8>>, Vec<u8>, Vec<u8>)> {
            Ok((
                self.values.clone(),
                self.last_trait.clone(),
                self.last_key.clone(),
            ))
        }
        fn apply(
            &self,
            _requests: std::collections::BTreeMap<
                Id,
                ava_vm::components::avax::shared_memory::Requests,
            >,
            _batches: &[ava_database::BatchOps],
        ) -> ava_vm::error::Result<()> {
            Ok(())
        }
    }

    /// A fixed alias → chain id lookup (the `ChainLookup` seam).
    struct FixedLookup(Vec<(String, Id)>);

    impl ChainLookup for FixedLookup {
        fn lookup(&self, alias: &str) -> Option<Id> {
            self.0.iter().find(|(a, _)| a == alias).map(|&(_, id)| id)
        }
    }

    #[test]
    fn service_get_utxos_source_chain_atomic() {
        let p_chain = Id::from([0x50; 32]);
        let atomic_utxo = make_utxo(9, 1, 0xAA, 500, 0, &[1]);
        let atomic_bytes = atomic_utxo.marshal().expect("marshal");

        let u_local = make_utxo(1, 0, 0xAA, 100, 0, &[1]);
        let service = utxo_service(&[u_local])
            .with_chain_lookup(Arc::new(FixedLookup(vec![("P".to_owned(), p_chain)])))
            .with_shared_memory(Arc::new(FixedSharedMemory {
                values: vec![atomic_bytes.clone()],
                last_trait: short(1).as_bytes().to_vec(),
                last_key: atomic_utxo.input_id().as_bytes().to_vec(),
            }));

        let args = GetUTXOsArgs {
            addresses: vec![bech32(&service, 1)],
            source_chain: "P".to_owned(),
            ..Default::default()
        };
        let reply = service.get_utxos(&args).expect("atomic get_utxos");

        // The atomic UTXO (not the local one) comes back.
        assert_eq!(reply.num_fetched, 1);
        assert_eq!(reply.utxos, vec![hex_encode(&atomic_bytes)]);
        assert_eq!(reply.end_index.address, bech32(&service, 1));
        assert_eq!(reply.end_index.utxo, atomic_utxo.input_id().to_string());
    }

    #[test]
    fn service_get_balance_strict_and_partial() {
        let asset = 0xAA;
        // Spendable: sole owner, unlocked.
        let u_ok = make_utxo(1, 0, asset, 100, 0, &[1]);
        // Locked in the future: only counted with includePartial.
        let u_locked = make_utxo(2, 0, asset, 50, NOW_SECS + 1000, &[1]);
        // Shared (2 owners): only counted with includePartial.
        let u_shared = make_utxo(3, 0, asset, 25, 0, &[1, 2]);
        // A different asset: never counted.
        let u_other = make_utxo(4, 0, 0xBB, 999, 0, &[1]);
        let service = utxo_service(&[u_ok.clone(), u_locked, u_shared, u_other]);

        let asset_str = Id::from([asset; 32]).to_string();

        // Strict (includePartial=false): only the sole-owner unlocked UTXO.
        let strict = service
            .get_balance(&GetBalanceArgs {
                address: bech32(&service, 1),
                asset_id: asset_str.clone(),
                include_partial: false,
            })
            .expect("strict get_balance");
        assert_eq!(strict.balance, 100, "strict balance counts only u_ok");
        assert_eq!(
            strict.utxo_ids,
            vec![UtxoIdReply {
                tx_id: u_ok.tx_id,
                output_index: 0
            }]
        );

        // Partial: locked + shared also count (100 + 50 + 25).
        let partial = service
            .get_balance(&GetBalanceArgs {
                address: bech32(&service, 1),
                asset_id: asset_str,
                include_partial: true,
            })
            .expect("partial get_balance");
        assert_eq!(partial.balance, 175);
        assert_eq!(partial.utxo_ids.len(), 3);

        // JSON shape: balance quoted; utxoIDs objects {txID, outputIndex}.
        let j = serde_json::to_value(&strict).expect("json");
        assert_eq!(j["balance"], serde_json::json!("100"));
        assert_eq!(j["utxoIDs"][0]["txID"], u_ok.tx_id.to_string());
        assert_eq!(j["utxoIDs"][0]["outputIndex"], serde_json::json!(0));
    }

    #[test]
    fn service_get_balance_bad_address_error() {
        let service = utxo_service(&[]);
        let err = service
            .get_balance(&GetBalanceArgs {
                address: "definitely-not-an-address".to_owned(),
                asset_id: Id::EMPTY.to_string(),
                include_partial: false,
            })
            .expect_err("bad address");
        assert!(
            err.to_string()
                .starts_with("problem parsing address 'definitely-not-an-address': "),
            "Go wrap prefix; got: {err}"
        );
    }

    #[test]
    fn service_get_balance_unknown_asset_error() {
        let service = utxo_service(&[]);
        let err = service
            .get_balance(&GetBalanceArgs {
                address: bech32(&service, 1),
                asset_id: "AVAX".to_owned(), // no asset aliaser (recorded deferral)
                include_partial: false,
            })
            .expect_err("unknown asset");
        // Go `vm.lookupAssetID` failure message.
        assert_eq!(err.to_string(), "asset 'AVAX' not found");
    }

    #[test]
    fn service_get_all_balances_shape() {
        let u_a = make_utxo(1, 0, 0xAA, 100, 0, &[1]);
        let u_a2 = make_utxo(2, 0, 0xAA, 11, 0, &[1]);
        let u_b = make_utxo(3, 0, 0xBB, 7, 0, &[1]);
        // Locked: excluded from the strict reply.
        let u_locked = make_utxo(4, 0, 0xBB, 1000, NOW_SECS + 5, &[1]);
        let service = utxo_service(&[u_a, u_a2, u_b, u_locked]);

        let reply = service
            .get_all_balances(&GetAllBalancesArgs {
                address: bech32(&service, 1),
                include_partial: false,
            })
            .expect("get_all_balances");

        // Sorted by asset id (deterministic; Go's set order is random).
        assert_eq!(
            reply.balances,
            vec![
                AssetBalance {
                    asset_id: Id::from([0xAA; 32]).to_string(),
                    balance: 111
                },
                AssetBalance {
                    asset_id: Id::from([0xBB; 32]).to_string(),
                    balance: 7
                },
            ]
        );

        // JSON: [{"asset": "<cb58>", "balance": "111"}, …].
        let j = serde_json::to_value(&reply).expect("json");
        assert_eq!(j["balances"][0]["asset"], Id::from([0xAA; 32]).to_string());
        assert_eq!(j["balances"][0]["balance"], serde_json::json!("111"));

        // includePartial folds the locked UTXO in.
        let partial = service
            .get_all_balances(&GetAllBalancesArgs {
                address: bech32(&service, 1),
                include_partial: true,
            })
            .expect("partial get_all_balances");
        assert_eq!(partial.balances[1].balance, 1007);
    }

    // -----------------------------------------------------------------------
    // `getTxFee`
    // -----------------------------------------------------------------------

    #[test]
    fn service_get_tx_fee_shape() {
        // Mainnet defaults (config.rs): 1 mAVAX / 10 mAVAX.
        let service = utxo_service(&[]);
        let reply = service.get_tx_fee();
        assert_eq!(reply.tx_fee, 1_000_000);
        assert_eq!(reply.create_asset_tx_fee, 10_000_000);

        // Go shape: quoted json.Uint64 strings under Go's exact JSON tags.
        let j = serde_json::to_value(reply).expect("json");
        assert_eq!(
            j,
            serde_json::json!({
                "txFee": "1000000",
                "createAssetTxFee": "10000000",
            })
        );

        // The fee schedule follows the VM config.
        let configured = utxo_service(&[]).with_fees(7, 9).get_tx_fee();
        assert_eq!(configured.tx_fee, 7);
        assert_eq!(configured.create_asset_tx_fee, 9);
    }

    // -----------------------------------------------------------------------
    // `hex_encode` / `decode_formatted_bytes` — checksum correctness
    // -----------------------------------------------------------------------

    #[test]
    fn hex_encode_includes_checksum() {
        // Verify that `hex_encode` appends exactly 4 bytes of checksum before
        // hex-encoding, matching Go `formatting.Encode(formatting.Hex, bytes)`.
        let raw = b"hello avalanche";
        let encoded = hex_encode(raw);

        // Must start with "0x".
        assert!(encoded.starts_with("0x"), "must start with 0x");
        let hex_body = encoded.trim_start_matches("0x");
        // Body length must be even.
        assert_eq!(hex_body.len() % 2, 0);
        // Decoded length = raw.len() + 4.
        let decoded_all = hex::decode(hex_body).expect("valid hex");
        assert_eq!(decoded_all.len(), raw.len() + 4);
        // Last 4 bytes = sha256(raw)[28..32].
        let expected_cs = ava_crypto::hashing::checksum(raw, 4);
        assert_eq!(&decoded_all[raw.len()..], expected_cs.as_slice());
    }

    #[test]
    fn decode_hex_strips_checksum() {
        // Round-trip: `hex_encode` then `decode_formatted_bytes("hex")` →
        // original bytes.
        let raw = b"round-trip test";
        let encoded = hex_encode(raw);
        let decoded = decode_formatted_bytes(&encoded, "hex").expect("decode");
        assert_eq!(decoded, raw);
    }

    #[test]
    fn decode_hex_bad_checksum_error() {
        // A hex string with a corrupted checksum must fail.
        let raw = b"checksum test";
        let mut encoded = hex_encode(raw);
        // Flip the last two hex characters (corrupt the last checksum byte).
        let n = encoded.len();
        let last_two = &encoded[n - 2..].to_owned();
        let corrupted = if last_two == "ff" { "00" } else { "ff" };
        encoded.truncate(n - 2);
        encoded.push_str(corrupted);
        assert!(
            decode_formatted_bytes(&encoded, "hex").is_err(),
            "bad checksum must error"
        );
    }

    #[test]
    fn decode_hexnc_no_checksum_required() {
        // `hexnc` decodes raw hex without any checksum logic.
        let raw = b"no checksum";
        let encoded = format!("0x{}", hex::encode(raw));
        let decoded = decode_formatted_bytes(&encoded, "hexnc").expect("decode hexnc");
        assert_eq!(decoded, raw);
    }

    #[test]
    fn decode_unsupported_encoding_error() {
        assert!(decode_formatted_bytes("anything", "cb58").is_err());
        assert!(decode_formatted_bytes("anything", "base64").is_err());
    }

    #[test]
    fn decode_missing_0x_prefix_error() {
        // `hex` encoding requires a `0x` prefix (Go `errMissingHexPrefix`).
        assert!(decode_formatted_bytes("aabbccdd", "hex").is_err());
    }

    // -----------------------------------------------------------------------
    // `avajson` encoding invariants
    // -----------------------------------------------------------------------

    #[test]
    fn avajson_u64_roundtrip() {
        let reply = GetHeightResponse { height: 42 };
        let j = serde_json::to_string(&reply).expect("json");
        assert!(j.contains("\"42\""), "u64 must be quoted: {j}");
        let back: GetHeightResponse = serde_json::from_str(&j).expect("deser");
        assert_eq!(back.height, 42);
    }

    #[test]
    fn avajson_u8_roundtrip() {
        let reply = GetAssetDescriptionReply {
            asset_id: Id::EMPTY,
            name: "T".to_owned(),
            symbol: "T".to_owned(),
            denomination: 12,
        };
        let j = serde_json::to_string(&reply).expect("json");
        assert!(j.contains("\"12\""), "u8 denomination must be quoted: {j}");
        let back: GetAssetDescriptionReply = serde_json::from_str(&j).expect("deser");
        assert_eq!(back.denomination, 12);
    }

    // -----------------------------------------------------------------------
    // `format_address` — X-bech32 encoding
    // -----------------------------------------------------------------------

    #[test]
    fn service_format_address_x_prefix() {
        let state = State::new(Arc::new(MemDb::new())).expect("state");
        let service = Service::new(Arc::new(state), 1); // mainnet
        let addr = [0u8; 20];
        let formatted = service.format_address(&addr).expect("format");
        assert!(
            formatted.starts_with("X-"),
            "X-Chain address must start with 'X-': {formatted}"
        );
        // Mainnet HRP is "avax".
        assert!(
            formatted.contains("avax"),
            "mainnet address must contain 'avax': {formatted}"
        );
    }

    // -----------------------------------------------------------------------
    // `TxStatus` JSON encoding
    // -----------------------------------------------------------------------

    #[test]
    fn tx_status_json_shapes() {
        // Must match Go `choices.Status.String()` — PascalCase strings.
        for (status, expected) in [
            (TxStatus::Accepted, "\"Accepted\""),
            (TxStatus::Unknown, "\"Unknown\""),
            (TxStatus::Processing, "\"Processing\""),
        ] {
            let j = serde_json::to_string(&status).expect("json");
            assert_eq!(j, expected, "status {expected} mismatch");
        }
    }

    // -----------------------------------------------------------------------
    // M8.22 wire layer: the bridged `avm.*` method set + gorilla envelope
    // -----------------------------------------------------------------------

    /// A recording [`TxIssuer`] stub.
    struct RecordingIssuer {
        issued: std::sync::Mutex<Vec<Id>>,
    }

    impl RecordingIssuer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                issued: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    impl TxIssuer for RecordingIssuer {
        fn issue_tx(&self, tx: Tx) -> std::result::Result<(), String> {
            self.issued.lock().expect("lock").push(tx.id());
            Ok(())
        }
    }

    /// The bridged method set is EXACTLY the 11 Go wire names (incl. the
    /// `GetUTXOs` acronym override and the M8.23b `GetTxFee`); the `wallet.*`
    /// extension stays unbridged (inventory: `tests/PORTING.md`).
    #[test]
    fn avm_method_set_matches_bridged() {
        let (service, _, _) = seeded_service();
        let reg = registry(Arc::new(service), RecordingIssuer::new());
        const BRIDGED: [&str; 11] = [
            "GetHeight",
            "GetBlock",
            "GetBlockByHeight",
            "GetTx",
            "GetTxStatus",
            "IssueTx",
            "GetAssetDescription",
            "GetUTXOs",
            "GetBalance",
            "GetAllBalances",
            "GetTxFee",
        ];
        assert_eq!(reg.len(), BRIDGED.len(), "exactly the bridged set");
        for m in BRIDGED {
            assert!(reg.lookup("avm", m).is_some(), "avm.{m} registered");
        }
        // Exact-remainder matching: the pascalized (non-Go) casing must miss.
        assert!(
            reg.lookup("avm", "GetUtxos").is_none(),
            "no pascalized GetUtxos"
        );
        // The keystore-backed wallet extension stays out of scope.
        assert!(
            reg.lookup("wallet", "SendMultiple").is_none(),
            "wallet.* unbridged"
        );
    }

    /// avm.getTxStatus + avm.issueTx end-to-end through the gorilla envelope:
    /// the wire issueTx parses AND submits through the `TxIssuer` seam.
    #[tokio::test]
    async fn avm_wire_shapes() {
        use ava_vm::vm::VmRequest;

        let (service, _, tx_id) = seeded_service();
        // Recover wire-encodable tx bytes from the seeded state.
        let tx_hex = service
            .get_tx(&GetTxArgs {
                tx_id,
                encoding: String::new(),
            })
            .expect("get_tx")
            .tx;

        let issuer = RecordingIssuer::new();
        let reg = Arc::new(registry(Arc::new(service), Arc::clone(&issuer) as _));
        let svc = crate::jsonrpc::registry_service(reg);
        let post = |body: serde_json::Value| {
            let svc = Arc::clone(&svc);
            async move {
                let resp = svc
                    .serve_http(VmRequest {
                        method: "POST".to_string(),
                        uri: String::new(),
                        headers: vec![("content-type".to_string(), "application/json".to_string())],
                        body: serde_json::to_vec(&body).expect("serialize"),
                    })
                    .await;
                assert_eq!(resp.status, 200, "JSON-RPC always answers HTTP 200");
                serde_json::from_slice::<serde_json::Value>(&resp.body).expect("json body")
            }
        };

        // getTxStatus over the accepted tx.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "avm.getTxStatus",
            "params": [{ "txID": tx_id.to_string() }],
            "id": 1,
        }))
        .await;
        assert_eq!(
            body,
            serde_json::json!({
                "jsonrpc": "2.0",
                "result": { "status": "Accepted" },
                "id": 1,
            }),
            "avm.getTxStatus envelope"
        );

        // issueTx round-trips the checksummed hex AND submits via the seam.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "avm.issueTx",
            "params": [{ "tx": tx_hex, "encoding": "hex" }],
            "id": 2,
        }))
        .await;
        assert_eq!(
            body["result"]["txID"],
            tx_id.to_string(),
            "avm.issueTx echoes the parsed txID"
        );
        assert_eq!(
            *issuer.issued.lock().expect("lock"),
            vec![tx_id],
            "the wire issueTx submits through the TxIssuer seam"
        );

        // getUTXOs with an unresolvable sourceChain alias surfaces Go's
        // handler error string as a -32000 server error.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "avm.getUTXOs",
            "params": [{ "addresses": ["X-avax1test"], "sourceChain": "P" }],
            "id": 3,
        }))
        .await;
        assert_eq!(body["error"]["code"], -32000, "handler error is -32000");
        assert_eq!(
            body["error"]["message"],
            "problem parsing source chainID \"P\": there is no ID with alias P",
            "Go-byte-equal handler error string"
        );

        // getTxFee (bridged in M8.23b) answers the static fee schedule.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "avm.getTxFee",
            "params": [{}],
            "id": 4,
        }))
        .await;
        assert_eq!(
            body["result"],
            serde_json::json!({
                "txFee": "1000000",
                "createAssetTxFee": "10000000",
            }),
            "avm.getTxFee mainnet defaults"
        );
    }
}
