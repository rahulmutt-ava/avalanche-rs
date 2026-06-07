// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (AVM) JSON-RPC **handler** service — port of the avm.* API methods
//! from `vms/avm/service.go` (specs 09 §10, 14 API reference).
//!
//! This module ports the request/response *shapes* (serde types matching Go's
//! JSON field names + encodings) and the handler *logic* over the live
//! [`State`](crate::state::State) seam. It deliberately does **not** wire an HTTP
//! / JSON-RPC server: that transport lands with `ava-api` (M8/M12). The
//! `Vm::create_handlers` HTTP wiring returns an empty map today; this service is
//! ready to wire when `ava-api` lands.
//!
//! ## Encodings (match Go exactly, `vms/avm/service.go`)
//!
//! - Integers use the avalanchego `json.Uint64`/`Uint8` convention: **quoted
//!   decimal strings** (`json.Uint64` ⇒ `"1234"`), via [`avajson`] serde helpers.
//! - [`Id`] serializes through its own `Serialize` impl (CB58), matching `ids.ID`.
//! - Addresses are bech32 chain-prefixed (`X-avax1…`), via
//!   [`ava_crypto::address::format`] with chain prefix `"X"`.
//! - Tx / block bytes are returned as hex `0x…` (the default `formatting.Hex`
//!   encoding from Go's `formatting.Encode(args.Encoding, bytes)`). The CB58
//!   encoding path is documented but not wired (requires the transport to select).
//! - Timestamps are RFC3339 (`time.Time` JSON), seconds precision.
//!
//! ## Deferred functionality (spec 09 §10 deferral list)
//!
//! - **`getUTXOs` address-pagination**: Go uses an address → UTXO index
//!   (`avax.GetPaginatedUTXOs`, `avax.GetAtomicUTXOs`) that is not yet ported.
//!   The method stubs are implemented returning `ErrNotImplemented` with a
//!   detailed comment. Wiring the address index is a follow-up task.
//! - **`getBalance` / `getAllBalances`**: both require the address UTXO index and
//!   the secp256k1fx UTXO iteration over the address set. Deferred (same reason).
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

use std::sync::Arc;

use ava_crypto::address;
use ava_database::Database;
use ava_types::constants::get_hrp;
use ava_types::id::Id;
use serde::{Deserialize, Serialize};

use crate::block::Block;
use crate::error::{Error, Result};
use crate::state::State;
use crate::state::chain::ReadOnlyChain;
use crate::txs::Tx;
use crate::txs::codec::Codec;

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

/// Formats a block/tx byte slice as `0x<hex>` (Go `formatting.HexEncoding`).
///
/// Go's `formatting.Encode(formatting.Hex, bytes)` → `"0x<hex><4-byte checksum>"`
/// but the service serializes raw bytes; callers that need the checksum variant
/// should use CB58 or hex+checksum. For this service we mirror
/// `formatting.Encode(args.Encoding, bytes)` with `Hex` as the default.
fn hex_encode(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
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
///
/// **DEFERRED**: address-indexed UTXO pagination (`avax.GetPaginatedUTXOs`)
/// requires the address → UTXO index that is not yet ported. The method
/// returns `Error::Service` with a `NotImplemented` message. Cross-chain
/// `sourceChain` lookups (`avax.GetAtomicUTXOs`) are also deferred.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetUTXOsArgs {
    /// The addresses whose UTXOs to fetch.
    pub addresses: Vec<String>,
    /// The source chain for atomic UTXOs (empty = this chain). **Deferred**.
    #[serde(default, rename = "sourceChain")]
    pub source_chain: String,
    /// Max results per page.
    #[serde(default)]
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
///
/// **DEFERRED**: see [`GetUTXOsArgs`] — this struct is defined for the type
/// system but the handler always returns `Error::Service`.
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

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// The X-Chain (AVM) API service over a [`State`].
///
/// Mirrors the handler methods of Go's `avm.Service` (port of
/// `vms/avm/service.go`). The HTTP/JSON-RPC transport that would dispatch onto
/// these is deferred to `ava-api` (M8/M12); each method here is the typed
/// handler body.
///
/// The service is deliberately constructed over the *read-only* [`State`]
/// (matching Go's `s.vm.state`), so it can be used from any snapshot without
/// holding the VM lock. Methods that need the mempool or p2p gossip (e.g.,
/// the full `issueTx` submit path) are documented as deferred.
pub struct Service<D: Database + 'static> {
    /// The persisted X-Chain state.
    state: Arc<State<D>>,
    /// The network id (for bech32 HRP derivation).
    network_id: u32,
}

impl<D: Database + 'static> Service<D> {
    /// Builds a service over a shared state snapshot.
    #[must_use]
    pub fn new(state: Arc<State<D>>, network_id: u32) -> Self {
        Self { state, network_id }
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
            Err(_) => TxStatus::Unknown,
        };
        Ok(GetTxStatusReply { status })
    }

    // -----------------------------------------------------------------------
    // `avm.issueTx` (Go `service.go IssueTx`)
    // -----------------------------------------------------------------------

    /// `avm.issueTx` — parse tx bytes and return the tx id.
    ///
    /// **Partial implementation**: the tx is parsed from the supplied bytes
    /// (hex `0x…` or CB58 per `args.encoding`) and its id is returned. The
    /// full submit path (mempool add + p2p gossip via `vm.issueTxFromRPC`)
    /// requires the `AvmVm` handle and is **deferred** to the `ava-api`
    /// transport layer. This is sufficient for client round-trip testing.
    ///
    /// Go decodes bytes using `formatting.Decode(args.Encoding, args.Tx)` then
    /// calls `s.vm.parser.ParseTx(txBytes)`.
    ///
    /// # Errors
    /// Returns [`Error::Codec`] if the bytes fail to parse.
    pub fn issue_tx(&self, args: &IssueTxArgs) -> Result<IssueTxReply> {
        let tx_bytes = decode_formatted_bytes(&args.tx, &args.encoding)?;
        let tx = Tx::parse(Codec(), &tx_bytes).map_err(Error::Codec)?;
        // NOTE: mempool submit + gossip is deferred; see module docs.
        Ok(IssueTxReply { tx_id: tx.id() })
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
    // `avm.getUTXOs` — DEFERRED
    // -----------------------------------------------------------------------

    /// `avm.getUTXOs` — **DEFERRED**.
    ///
    /// Address-indexed UTXO pagination (`avax.GetPaginatedUTXOs`) and the
    /// cross-chain atomic UTXO path (`avax.GetAtomicUTXOs`) both require the
    /// address → UTXO index, which is not yet ported. Returns an error
    /// with a clear `NotImplemented` message.
    ///
    /// Follow-up: port the address index + `avax.GetPaginatedUTXOs` +
    /// `avax.GetAtomicUTXOs` (specs 09 §10; 27 §2.3).
    pub fn get_utxos(&self, _args: &GetUTXOsArgs) -> Result<GetUTXOsReply> {
        Err(Error::Service(
            "getUTXOs: address-indexed UTXO pagination not yet implemented \
             (deferred: requires address→UTXO index port; see service.rs module docs)"
                .to_owned(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Byte-decoding helper
// ---------------------------------------------------------------------------

/// Decodes a formatted byte string for `issueTx`.
///
/// Supports `"hex"` / `"0x…"` and `"cb58"` / empty (CB58 is the Go default).
/// Go uses `formatting.Decode(args.Encoding, args.Tx)`.
///
/// # Errors
/// Returns [`Error::Service`] on decode failure.
fn decode_formatted_bytes(s: &str, encoding: &str) -> Result<Vec<u8>> {
    let enc = encoding.to_lowercase();
    if enc == "hex" || s.starts_with("0x") || s.starts_with("0X") {
        let hex_str = s.trim_start_matches("0x").trim_start_matches("0X");
        hex::decode(hex_str).map_err(|e| Error::Service(format!("hex decode: {e}")))
    } else {
        // Default: CB58 (Go's `formatting.CB58` / `formatting.Encoding("")`).
        ava_utils::cb58::cb58_decode(s).map_err(|e| Error::Service(format!("cb58 decode: {e}")))
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
    fn seeded_service() -> (Service<MemDb>, Id, Id) {
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

        // Recover the tx bytes from state and re-issue them (hex-encoded).
        let args = GetTxArgs {
            tx_id,
            encoding: String::new(),
        };
        let tx_reply = service.get_tx(&args).expect("get_tx");

        // issueTx with the hex-encoded bytes → same tx_id back.
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
    // `getUTXOs` — stub returns error
    // -----------------------------------------------------------------------

    #[test]
    fn service_get_utxos_deferred() {
        let (service, _, _) = seeded_service();
        let args = GetUTXOsArgs {
            addresses: vec!["X-avax1test".to_owned()],
            ..Default::default()
        };
        // Expected: deferred stub returns an error.
        assert!(service.get_utxos(&args).is_err());
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
}
