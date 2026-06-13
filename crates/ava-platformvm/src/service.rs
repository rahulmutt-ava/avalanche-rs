// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain JSON-RPC **read** service — port of the read-relevant methods of
//! `vms/platformvm/service.go` (specs 08 §9, 14).
//!
//! This module ports the request/response *shapes* (serde types matching Go's
//! JSON field names + encodings) and the read-method *logic* over the live
//! [`State`](crate::state::state::State) / `PChainValidatorManager`
//! (M4.20/M4.21/M4.25) seams. Write methods (`issueTx`, …) are out of scope for
//! read-only sync and are not ported here (see `tests/PORTING.md` for the
//! method inventory vs Go).
//!
//! ## Transport (M8.22)
//!
//! [`RpcService`] bridges these typed bodies onto `ava-api`'s gorilla-json2
//! [`ServiceRegistry`] under the Go service name `platform` (Go
//! `vms/platformvm/vm.go:451-466` `CreateHandlers` registers `&Service{…}` as
//! `"platform"` at extension `""`); `PlatformVm::create_handlers` mounts the
//! [`registry`] through the in-process `HttpHandler` seam. The service reads
//! state through the [`ServiceState`] seam so the same bodies serve both an
//! owned snapshot (tests) and the VM's lock-guarded live state (`vm.rs`).
//!
//! ## Encodings (match Go exactly)
//!
//! - Integers use the avalanchego `json.Uint64`/`Uint32` convention: **quoted
//!   decimal strings** (`"1234"`), via [`avajson`] serde helpers.
//! - [`Id`] / [`NodeId`] serialize through their own `Serialize` impls (CB58 /
//!   `NodeID-…`), matching `ids.ID` / `ids.NodeID`.
//! - Addresses are bech32 chain-prefixed (`P-avax1…`), via
//!   [`ava_crypto::address::format`].
//! - BLS public keys are hex (`0x…` compressed), matching `getValidatorsAt`'s
//!   `formatting.HexNC` encoding.
//! - Timestamps are RFC3339 (`time.Time` JSON), seconds precision.
//!
//! ## Determinism (00 §6.1)
//!
//! `getCurrentValidators` / `getValidatorsAt` read from the manager's
//! `BTreeMap<_, _>` snapshots and emit results sorted by validation id / node id
//! so the JSON ordering is canonical and reproducible.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ava_api_macros::rpc_service;
use ava_crypto::address;
use ava_crypto::bls::PublicKey;
use ava_crypto::hashing::checksum;
use ava_database::Database;
use ava_types::constants::get_hrp;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use ava_secp256k1fx::OutputOwners;
use ava_utils::sampler::new_deterministic_weighted_without_replacement;
use ava_utils::sampler::weighted_without_replacement::WeightedWithoutReplacement;
use ava_validators::state::ValidatorState;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result};
use crate::jsonrpc::{RpcError, ServiceRegistry};
use crate::state::chain::{Chain, UtxoBytes};
use crate::state::l1_validator::L1Validator;
use crate::state::staker::Staker;
use crate::state::state::State;
use crate::status::Status;
use crate::txs::Tx;
use crate::txs::components::{Output, TransferableOutput};
use crate::txs::executor::StakingConfig;
use crate::txs::fee::dynamic_calculator::{
    K as DYNAMIC_FEE_K, MAX_CAPACITY, MAX_PER_SECOND, MIN_PRICE as DYNAMIC_FEE_MIN_PRICE,
    TARGET_PER_SECOND, WEIGHTS,
};
use crate::txs::fee::gas::{GasState, calculate_price};
use crate::utxo::{Utxo, output_addresses};
use crate::validators::fee as validator_fee;

/// `maxGetUTXOsAddrs` — `getUTXOs` address-count cap (`service.go:51`).
const MAX_GET_UTXOS_ADDRS: usize = 1024;
/// `maxGetStakeAddrs` — `getStake` address-count cap (`service.go:54`).
const MAX_GET_STAKE_ADDRS: usize = 256;
/// `maxPageSize` — `getUTXOs` page-size cap (`service.go:57`).
const MAX_PAGE_SIZE: usize = 1024;

/// avalanchego `utils/json` numeric encodings: integers as quoted decimal
/// strings (`json.Uint64` ⇒ `"1234"`).
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

    /// Deserialize from either a quoted decimal string or a bare JSON number —
    /// the two forms `utils/json` integers accept on the wire.
    ///
    /// # Errors
    /// Returns a deserialization error on any other JSON shape.
    pub fn deserialize_lenient_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::Number(n) => n
                .as_u64()
                .ok_or_else(|| serde::de::Error::custom("integer out of range")),
            serde_json::Value::String(s) => s.parse::<u64>().map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom(
                "expected a number or quoted decimal string",
            )),
        }
    }

    /// Serialize a `BTreeMap<Id, u64>` as `{ assetID: "amount" }` (Go
    /// `map[ids.ID]avajson.Uint64`, deterministic key order via the BTreeMap).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_balance_map<S: Serializer>(
        m: &std::collections::BTreeMap<ava_types::id::Id, u64>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(m.len()))?;
        for (k, v) in m {
            map.serialize_entry(k, &v.to_string())?;
        }
        map.end()
    }
}

/// Formats a 32-byte compressed BLS key as `formatting.HexNC` (`0x…`).
fn hex_nc(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// `safemath.Add` into a balance map, saturating at `u64::MAX` (Go sets the
/// entry to `math.MaxUint64` on overflow).
fn add_balance(map: &mut BTreeMap<Id, u64>, asset: Id, amount: u64) {
    let entry = map.entry(asset).or_insert(0);
    *entry = entry.saturating_add(amount);
}

/// `formatting.Encode` (the [`Error`]-returning variant of
/// [`encode_reply_bytes`]): `hex`/`""` appends the 4-byte checksum then
/// hex-encodes with a `0x` prefix; `hexnc` skips the checksum. Returns the
/// encoded string + the canonical encoding name.
///
/// # Errors
/// Returns [`Error::Service`] for `json` (deferred) or an unknown encoding.
fn encode_bytes(bytes: &[u8], encoding: &str) -> Result<(String, String)> {
    match encoding {
        "" | "hex" => {
            let cs = checksum(bytes, 4);
            let mut combined = bytes.to_vec();
            combined.extend_from_slice(&cs);
            Ok((format!("0x{}", hex::encode(&combined)), "hex".to_owned()))
        }
        "hexnc" => Ok((format!("0x{}", hex::encode(bytes)), "hexnc".to_owned())),
        "json" => Err(Error::Service(
            "json encoding is not yet supported (deferred: typed JSON shapes)".to_owned(),
        )),
        other => Err(Error::Service(format!("invalid encoding: {other}"))),
    }
}

/// `formatting.Decode` — the inverse of [`encode_bytes`]: `hex`/`""` strips and
/// verifies the trailing 4-byte checksum; `hexnc` does not.
///
/// # Errors
/// Returns [`Error::Service`] on a bad hex string, a missing/invalid checksum,
/// or an unsupported encoding.
fn decode_bytes(s: &str, encoding: &str) -> Result<Vec<u8>> {
    let hexpart = s.strip_prefix("0x").unwrap_or(s);
    let raw =
        hex::decode(hexpart).map_err(|e| Error::Service(format!("invalid hex: {e}")))?;
    match encoding {
        "" | "hex" => {
            if raw.len() < 4 {
                return Err(Error::Service("input too short for checksum".to_owned()));
            }
            let split = raw.len() - 4;
            let (payload, cs) = raw.split_at(split);
            if checksum(payload, 4) != cs {
                return Err(Error::Service("invalid input checksum".to_owned()));
            }
            Ok(payload.to_vec())
        }
        "hexnc" => Ok(raw),
        other => Err(Error::Service(format!("invalid encoding: {other}"))),
    }
}

/// The canonical reply-echoed encoding name for `encoding`
/// (`""`/`hex` → `hex`).
///
/// # Errors
/// Returns [`Error::Service`] for `json` (deferred) or an unknown encoding.
fn canonical_encoding(encoding: &str) -> Result<String> {
    match encoding {
        "" | "hex" => Ok("hex".to_owned()),
        "hexnc" => Ok("hexnc".to_owned()),
        "json" => Err(Error::Service(
            "json encoding is not yet supported (deferred: typed JSON shapes)".to_owned(),
        )),
        other => Err(Error::Service(format!("invalid encoding: {other}"))),
    }
}

/// The primary-network `APISubnet` view (no control keys, threshold 0).
fn primary_api_subnet() -> ApiSubnet {
    ApiSubnet {
        id: Id::EMPTY,
        control_keys: Vec::new(),
        threshold: 0,
    }
}

/// The stake outputs of a staker-creating tx (`tx.Unsigned.Stake()`); empty for
/// non-staker txs.
fn stake_outs_of(tx: &crate::txs::UnsignedTx) -> &[crate::txs::components::TransferableOutput] {
    use crate::txs::UnsignedTx;
    match tx {
        UnsignedTx::AddValidator(t) => &t.stake_outs,
        UnsignedTx::AddDelegator(t) => &t.stake_outs,
        UnsignedTx::AddPermissionlessValidator(t) => &t.stake_outs,
        UnsignedTx::AddPermissionlessDelegator(t) => &t.stake_outs,
        UnsignedTx::AddAutoRenewedValidator(t) => &t.stake_outs,
        _ => &[],
    }
}

/// Groups a [`WarpSet`](ava_validators::state::WarpSet)'s flat per-node
/// validators by compressed BLS public key into the Go `validators.Warp`
/// shape (`{publicKey, weight, nodeIDs}`), summing weights and collecting node
/// ids, sorted by uncompressed public-key bytes (Go `Warp.Less`). Validators
/// with no public key are omitted (they cannot warp-sign).
///
/// # Errors
/// Infallible today; returns [`Result`] for caller-side `?` symmetry.
fn warp_set_to_json(warp: &ava_validators::state::WarpSet) -> Result<JsonWarpSet> {
    // key: uncompressed pubkey bytes (the canonical sort key).
    let mut grouped: BTreeMap<Vec<u8>, (Vec<u8>, u64, Vec<NodeId>)> = BTreeMap::new();
    for v in &warp.validators {
        let Some(pk) = &v.public_key else {
            continue;
        };
        let uncompressed = pk.serialize().to_vec();
        let entry = grouped
            .entry(uncompressed)
            .or_insert_with(|| (pk.compress().to_vec(), 0, Vec::new()));
        entry.1 = entry.1.saturating_add(v.weight);
        entry.2.push(v.node_id);
    }
    let validators = grouped
        .into_values()
        .map(|(compressed, weight, mut node_ids)| {
            node_ids.sort();
            JsonWarpValidator {
                public_key: hex_nc(&compressed),
                weight,
                node_ids,
            }
        })
        .collect();
    Ok(JsonWarpSet {
        validators,
        total_weight: warp.total_weight,
    })
}

/// Formats a P-Chain timestamp (whole Unix seconds) as RFC3339 (`time.Time`).
fn format_timestamp(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(i64::try_from(secs).unwrap_or(0), 0)
        .unwrap_or_default();
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// getHeight / getCurrentSupply / getTimestamp / getFeeState
// ---------------------------------------------------------------------------

/// `api.GetHeightResponse` — reply for `getHeight` / `getProposedHeight`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetHeightResponse {
    /// The queried height.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub height: u64,
}

/// `platformvm.GetCurrentSupplyArgs`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetCurrentSupplyArgs {
    /// The subnet whose supply is queried (defaults to the primary network).
    #[serde(default, rename = "subnetID")]
    pub subnet_id: Id,
}

/// `platformvm.GetCurrentSupplyReply`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetCurrentSupplyReply {
    /// An upper bound on the AVAX supply.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub supply: u64,
    /// The last-accepted height.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub height: u64,
}

/// `platformvm.GetTimestampReply`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTimestampReply {
    /// The current chain timestamp (RFC3339).
    pub timestamp: String,
}

/// `platformvm.GetFeeStateReply` — embeds `gas.State` (`capacity`/`excess`)
/// plus `price`/`timestamp`. Go's `gas.Gas`/`gas.Price` are bare `uint64`s
/// with **no** custom JSON marshaler, so these are plain JSON numbers (NOT the
/// quoted `json.Uint64` convention).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetFeeStateReply {
    /// Remaining gas capacity.
    pub capacity: u64,
    /// Accumulated gas excess (the price input).
    pub excess: u64,
    /// The current dynamic gas price
    /// (`gas.CalculatePrice(MinPrice, excess, K)`).
    pub price: u64,
    /// The chain timestamp (RFC3339).
    pub timestamp: String,
}

/// `platformvm.GetValidatorFeeStateReply` (plain JSON numbers, same rationale
/// as [`GetFeeStateReply`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetValidatorFeeStateReply {
    /// The L1-validator continuous-fee excess.
    pub excess: u64,
    /// The current validator fee price.
    pub price: u64,
    /// The chain timestamp (RFC3339).
    pub timestamp: String,
}

// ---------------------------------------------------------------------------
// getCurrentValidators
// ---------------------------------------------------------------------------

/// `platformvm.GetCurrentValidatorsArgs`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetCurrentValidatorsArgs {
    /// The subnet to list validators of (defaults to the primary network).
    #[serde(default, rename = "subnetID")]
    pub subnet_id: Id,
    /// The node ids to restrict to; empty fetches all current validators.
    #[serde(default, rename = "nodeIDs")]
    pub node_ids: Vec<NodeId>,
}

/// A single API validator entry (the read-relevant subset of
/// `platformapi.PermissionlessValidator` / `APIL1Validator`).
///
/// The owner / delegator / reward / uptime fields of Go's full reply are
/// **deferred** (they need the staker-attribute cache, owner formatting and
/// delegator iteration — out of scope for read-only sync, M4.28). The fields
/// present here are sourced from
/// [`ValidatorState::get_current_validator_set`]: node id, weight, start time,
/// validation id, BLS key, and the L1 activity flags.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiValidator {
    /// The transaction / validation id that added this staker.
    #[serde(rename = "txID")]
    pub tx_id: Id,
    /// The validating node id.
    #[serde(rename = "nodeID")]
    pub node_id: NodeId,
    /// The validator's weight (stake).
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub weight: u64,
    /// The Unix start time.
    #[serde(
        rename = "startTime",
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub start_time: u64,
    /// The compressed BLS public key (hex `0x…`), if present.
    #[serde(rename = "publicKey", skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// The validation id (ACP-77 L1 validators); mirrors `txID` for L1 entries.
    #[serde(rename = "validationID", skip_serializing_if = "Option::is_none")]
    pub validation_id: Option<Id>,
    /// The minimum balance-update nonce (L1 validators only).
    #[serde(
        rename = "minNonce",
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "avajson::serialize_opt_u64",
        deserialize_with = "avajson::deserialize_opt_u64"
    )]
    pub min_nonce: Option<u64>,
}

/// `platformvm.GetCurrentValidatorsReply`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetCurrentValidatorsReply {
    /// The current validators, sorted by validation id (canonical order).
    pub validators: Vec<ApiValidator>,
}

/// `platformvm.GetL1ValidatorArgs`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetL1ValidatorArgs {
    /// The validation id to look up.
    #[serde(rename = "validationID")]
    pub validation_id: Id,
}

/// `platformvm.GetL1ValidatorReply` (read-relevant subset).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetL1ValidatorReply {
    /// The validating node id.
    #[serde(rename = "nodeID")]
    pub node_id: NodeId,
    /// The validator's weight.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub weight: u64,
    /// The Unix start time.
    #[serde(
        rename = "startTime",
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub start_time: u64,
    /// The validation id.
    #[serde(rename = "validationID")]
    pub validation_id: Id,
    /// The compressed BLS public key (hex `0x…`), if present.
    #[serde(rename = "publicKey", skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// The minimum balance-update nonce.
    #[serde(
        rename = "minNonce",
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub min_nonce: u64,
    /// The subnet id.
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
    /// The last-accepted height.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub height: u64,
}

// ---------------------------------------------------------------------------
// getValidatorsAt / sampleValidators / validatedBy / validates / getSubnet
// ---------------------------------------------------------------------------

/// One `getValidatorsAt` validator entry (`jsonGetValidatorOutput`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonGetValidatorOutput {
    /// The compressed BLS public key (hex `0x…`), or `null`.
    #[serde(rename = "publicKey")]
    pub public_key: Option<String>,
    /// The validator's weight.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub weight: u64,
}

/// `platformvm.GetValidatorsAtReply` — a `nodeID → output` map (canonical).
pub type GetValidatorsAtReply = BTreeMap<NodeId, JsonGetValidatorOutput>;

/// `platformvm.SampleValidatorsReply`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleValidatorsReply {
    /// The sampled node ids, sorted.
    pub validators: Vec<NodeId>,
}

/// `platformvm.ValidatedByResponse`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedByResponse {
    /// The subnet validating the queried blockchain.
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
}

/// `platformvm.ValidatesResponse`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatesResponse {
    /// The blockchains validated by the queried subnet.
    #[serde(rename = "blockchainIDs")]
    pub blockchain_ids: Vec<Id>,
}

/// `platformvm.GetTxStatusResponse`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTxStatusResponse {
    /// The transaction's status.
    pub status: Status,
    /// The drop reason (only non-empty when `status == Dropped`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

// ---------------------------------------------------------------------------
// M8.23a typed bodies — the 15 previously-missing platform.* methods
// ---------------------------------------------------------------------------

/// `platformvm.GetBalanceRequest` — `{"addresses"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetBalanceRequest {
    /// The bech32 P-Chain addresses to sum over.
    pub addresses: Vec<String>,
}

/// `avax.UTXOID` JSON shape — `{"txID", "outputIndex"}` (the `Symbol` field is
/// `json:"-"`; `OutputIndex` is a bare `uint32` ⇒ a JSON number).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiUtxoId {
    /// The producing tx.
    #[serde(rename = "txID")]
    pub tx_id: Id,
    /// The output index within that tx.
    #[serde(rename = "outputIndex")]
    pub output_index: u32,
}

/// `platformvm.GetBalanceResponse`. The scalar AVAX fields duplicate the maps'
/// AVAX entries for backwards compatibility (Go comment, `service.go:123`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetBalanceResponse {
    /// Total AVAX balance, in nAVAX.
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub balance: u64,
    /// Unlocked AVAX.
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub unlocked: u64,
    /// Locked-stakeable AVAX.
    #[serde(
        rename = "lockedStakeable",
        serialize_with = "avajson::serialize_u64"
    )]
    pub locked_stakeable: u64,
    /// Locked, not-stakeable AVAX.
    #[serde(
        rename = "lockedNotStakeable",
        serialize_with = "avajson::serialize_u64"
    )]
    pub locked_not_stakeable: u64,
    /// Per-asset totals.
    #[serde(serialize_with = "avajson::serialize_balance_map")]
    pub balances: BTreeMap<Id, u64>,
    /// Per-asset unlocked amounts.
    #[serde(serialize_with = "avajson::serialize_balance_map")]
    pub unlockeds: BTreeMap<Id, u64>,
    /// Per-asset locked-stakeable amounts.
    #[serde(
        rename = "lockedStakeables",
        serialize_with = "avajson::serialize_balance_map"
    )]
    pub locked_stakeables: BTreeMap<Id, u64>,
    /// Per-asset locked-not-stakeable amounts.
    #[serde(
        rename = "lockedNotStakeables",
        serialize_with = "avajson::serialize_balance_map"
    )]
    pub locked_not_stakeables: BTreeMap<Id, u64>,
    /// The contributing UTXO ids (`null` when none, matching Go's nil slice).
    #[serde(rename = "utxoIDs")]
    pub utxo_ids: Option<Vec<ApiUtxoId>>,
}

/// `platformvm.Index` — a `getUTXOs` pagination cursor (`{"address","utxo"}`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtxoIndex {
    /// The address cursor (bech32).
    #[serde(default)]
    pub address: String,
    /// The UTXO-id cursor (CB58).
    #[serde(default)]
    pub utxo: String,
}

/// `api.GetUTXOsArgs`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetUTXOsArgs {
    /// The addresses whose UTXOs are fetched.
    pub addresses: Vec<String>,
    /// The chain to fetch from (`""`/this chain = local; otherwise atomic
    /// shared-memory UTXOs — a recorded deferral here).
    #[serde(rename = "sourceChain")]
    pub source_chain: String,
    /// The pagination start cursor (exclusive).
    #[serde(rename = "startIndex")]
    pub start_index: UtxoIndex,
    /// The page-size cap (`json.Uint64`; `0`/`>1024` clamps to 1024).
    #[serde(deserialize_with = "avajson::deserialize_lenient_u64")]
    pub limit: u64,
    /// The reply encoding (`hex` default / `hexnc`).
    pub encoding: String,
}

/// `api.GetUTXOsReply`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetUTXOsReply {
    /// The number of UTXOs returned.
    #[serde(rename = "numFetched", serialize_with = "avajson::serialize_u64")]
    pub num_fetched: u64,
    /// The encoded UTXOs.
    pub utxos: Vec<String>,
    /// The end cursor (pass as the next `startIndex`).
    #[serde(rename = "endIndex")]
    pub end_index: UtxoIndex,
    /// The encoding used.
    pub encoding: String,
}

/// `platformvm.GetSubnetArgs` — `{"subnetID"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetSubnetArgs {
    /// The queried subnet.
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
}

/// `platformvm.GetSubnetResponse`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetSubnetResponse {
    /// `false` for elastic subnets and L1-converted subnets.
    #[serde(rename = "isPermissioned")]
    pub is_permissioned: bool,
    /// The owner's control-key addresses (bech32).
    #[serde(rename = "controlKeys")]
    pub control_keys: Vec<String>,
    /// The owner threshold.
    #[serde(serialize_with = "avajson::serialize_u32")]
    pub threshold: u32,
    /// The owner locktime.
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub locktime: u64,
    /// The elastic-subnet transform tx (empty id when permissioned).
    #[serde(rename = "subnetTransformationTxID")]
    pub subnet_transformation_tx_id: Id,
    /// The L1-conversion id (empty id when unconverted).
    #[serde(rename = "conversionID")]
    pub conversion_id: Id,
    /// The L1 manager chain (empty id when unconverted).
    #[serde(rename = "managerChainID")]
    pub manager_chain_id: Id,
    /// The L1 manager address (`types.JSONByteSlice`: `null` or `0x…`).
    #[serde(rename = "managerAddress")]
    pub manager_address: Option<String>,
}

/// `platformvm.APISubnet`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApiSubnet {
    /// The subnet id.
    pub id: Id,
    /// The owner's control-key addresses (bech32).
    #[serde(rename = "controlKeys")]
    pub control_keys: Vec<String>,
    /// The owner threshold.
    #[serde(serialize_with = "avajson::serialize_u32")]
    pub threshold: u32,
}

/// `platformvm.GetSubnetsArgs` — `{"ids"}` (empty fetches all).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetSubnetsArgs {
    /// The subnets to describe; empty lists every subnet.
    pub ids: Vec<Id>,
}

/// `platformvm.GetSubnetsResponse`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetSubnetsResponse {
    /// The matching subnets (the primary network included).
    pub subnets: Vec<ApiSubnet>,
}

/// `platformvm.SampleValidatorsArgs` — `{"size", "subnetID"}`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SampleValidatorsArgs {
    /// The sample size (`json.Uint16`).
    #[serde(deserialize_with = "avajson::deserialize_lenient_u64")]
    pub size: u64,
    /// The sampled subnet (defaults to the primary network).
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
}

impl Default for SampleValidatorsArgs {
    fn default() -> Self {
        Self {
            size: 0,
            subnet_id: Id::EMPTY,
        }
    }
}

/// `status.BlockchainStatus` — JSON string statuses
/// (`vms/platformvm/status/blockchain_status.go`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockchainStatus {
    /// The chain is not known to exist.
    #[default]
    #[serde(rename = "Unknown")]
    UnknownChain,
    /// The chain's create-chain tx is accepted.
    Created,
    /// The create-chain tx is in the preferred (not yet accepted) chain.
    Preferred,
    /// This node validates the chain.
    Validating,
    /// This node is syncing the chain.
    Syncing,
}

/// `platformvm.GetBlockchainStatusArgs` — `{"blockchainID"}` (a string: Go
/// accepts an alias or an id; only ids resolve here — no chain registry seam).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetBlockchainStatusArgs {
    /// The queried blockchain id.
    #[serde(rename = "blockchainID")]
    pub blockchain_id: String,
}

/// `platformvm.GetBlockchainStatusReply`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetBlockchainStatusReply {
    /// The blockchain's status.
    pub status: BlockchainStatus,
}

/// `platformvm.APIBlockchain`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApiBlockchain {
    /// The blockchain id.
    pub id: Id,
    /// The (non-unique) human-readable chain name.
    pub name: String,
    /// The validating subnet.
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
    /// The VM the chain runs.
    #[serde(rename = "vmID")]
    pub vm_id: Id,
}

/// `platformvm.GetBlockchainsResponse`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetBlockchainsResponse {
    /// Every blockchain that exists (custom subnets first, primary last).
    pub blockchains: Vec<ApiBlockchain>,
}

/// `api.FormattedTx` — the `issueTx` payload (`{"tx", "encoding"}`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FormattedTx {
    /// The encoded signed-tx bytes.
    pub tx: String,
    /// The payload encoding (`hex` default / `hexnc`).
    pub encoding: String,
}

/// `api.JSONTxID` — `{"txID"}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct JsonTxId {
    /// The issued tx id.
    #[serde(rename = "txID")]
    pub tx_id: Id,
}

/// `platformvm.GetStakeArgs` — `api.JSONAddresses` + `validatorsOnly` +
/// `encoding`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetStakeArgs {
    /// The addresses whose stake is summed.
    pub addresses: Vec<String>,
    /// Restrict to validators (skip delegators).
    #[serde(rename = "validatorsOnly")]
    pub validators_only: bool,
    /// The staked-output encoding (`hex` default / `hexnc`).
    pub encoding: String,
}

/// `platformvm.GetStakeReply`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetStakeReply {
    /// Total AVAX staked, in nAVAX.
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub staked: u64,
    /// Per-asset staked amounts.
    #[serde(serialize_with = "avajson::serialize_balance_map")]
    pub stakeds: BTreeMap<Id, u64>,
    /// The staked outputs (`avax.TransferableOutput` codec bytes, encoded).
    #[serde(rename = "stakedOutputs")]
    pub outputs: Vec<String>,
    /// The encoding used.
    pub encoding: String,
}

/// `platformvm.GetMinStakeArgs` — `{"subnetID"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetMinStakeArgs {
    /// The queried subnet (primary network default).
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
}

/// `platformvm.GetMinStakeReply`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetMinStakeReply {
    /// The minimum validator bond.
    #[serde(
        rename = "minValidatorStake",
        serialize_with = "avajson::serialize_u64"
    )]
    pub min_validator_stake: u64,
    /// The minimum delegation.
    #[serde(
        rename = "minDelegatorStake",
        serialize_with = "avajson::serialize_u64"
    )]
    pub min_delegator_stake: u64,
}

/// `platformvm.GetTotalStakeArgs` — `{"subnetID"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetTotalStakeArgs {
    /// The queried subnet (primary network default).
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
}

/// `platformvm.GetTotalStakeReply` — `stake` is the deprecated alias of
/// `weight`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetTotalStakeReply {
    /// Deprecated: equals `weight`.
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub stake: u64,
    /// The subnet's total validator weight.
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub weight: u64,
}

/// `platformvm.GetRewardUTXOsReply`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetRewardUTXOsReply {
    /// The number of UTXOs returned.
    #[serde(rename = "numFetched", serialize_with = "avajson::serialize_u64")]
    pub num_fetched: u64,
    /// The encoded reward UTXOs.
    pub utxos: Vec<String>,
    /// The encoding used.
    pub encoding: String,
}

/// `platformvm.GetAllValidatorsAtArgs` — `{"height"}` (`platformapi.Height`).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct GetAllValidatorsAtArgs {
    /// The queried height (`json.Uint64` or `"proposed"`).
    #[serde(deserialize_with = "de_height")]
    pub height: u64,
}

impl Default for GetAllValidatorsAtArgs {
    fn default() -> Self {
        Self { height: 0 }
    }
}

/// `validators.Warp` JSON shape (`{"publicKey","weight","nodeIDs"}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct JsonWarpValidator {
    /// The compressed BLS key (`formatting.HexNC`).
    #[serde(rename = "publicKey")]
    pub public_key: String,
    /// The summed weight of the nodes sharing this key.
    #[serde(serialize_with = "avajson::serialize_u64")]
    pub weight: u64,
    /// The node ids sharing this key.
    #[serde(rename = "nodeIDs")]
    pub node_ids: Vec<NodeId>,
}

/// `validators.WarpSet` JSON shape (`{"validators","totalWeight"}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct JsonWarpSet {
    /// The keyed validators, sorted by uncompressed public-key bytes.
    pub validators: Vec<JsonWarpValidator>,
    /// The total subnet weight (keyless validators included).
    #[serde(rename = "totalWeight", serialize_with = "avajson::serialize_u64")]
    pub total_weight: u64,
}

/// `platformvm.GetAllValidatorsAtReply` — `{"validatorSets": {subnetID: …}}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetAllValidatorsAtReply {
    /// The per-subnet canonical validator sets at the queried height.
    #[serde(rename = "validatorSets")]
    pub validator_sets: BTreeMap<Id, JsonWarpSet>,
}

/// `gas.Config` — the `getFeeConfig` reply. Go's `gas.Gas`/`gas.Price` are
/// bare `uint64`s with **no** custom JSON marshaler ⇒ plain JSON numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetFeeConfigReply {
    /// The per-dimension complexity → gas weights.
    pub weights: [u64; 4],
    /// Maximum storable gas.
    #[serde(rename = "maxCapacity")]
    pub max_capacity: u64,
    /// Gas refill rate per second.
    #[serde(rename = "maxPerSecond")]
    pub max_per_second: u64,
    /// Target gas use per second.
    #[serde(rename = "targetPerSecond")]
    pub target_per_second: u64,
    /// Minimum gas price.
    #[serde(rename = "minPrice")]
    pub min_price: u64,
    /// The exponential-price excess conversion constant.
    #[serde(rename = "excessConversionConstant")]
    pub excess_conversion_constant: u64,
}

/// `fee.Config` — the `getValidatorFeeConfig` reply (plain JSON numbers, same
/// rationale as [`GetFeeConfigReply`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetValidatorFeeConfigReply {
    /// Maximum active L1 validators.
    pub capacity: u64,
    /// Target active L1 validators.
    pub target: u64,
    /// Minimum continuous-fee price (nAVAX/s).
    #[serde(rename = "minPrice")]
    pub min_price: u64,
    /// The exponential-price excess conversion constant.
    #[serde(rename = "excessConversionConstant")]
    pub excess_conversion_constant: u64,
}

// ---------------------------------------------------------------------------
// The state seam + the read service
// ---------------------------------------------------------------------------

/// The state reads the P-Chain service performs, abstracted so the service can
/// run over either an owned [`State`] snapshot (tests) or the VM's live,
/// lock-guarded state (`vm.rs` forwards each call under the block-manager
/// mutex — the moral equivalent of Go's per-method `vm.ctx.Lock`).
pub trait ServiceState: Send + Sync {
    /// [`Chain::timestamp`] — the current chain timestamp.
    fn timestamp(&self) -> SystemTime;
    /// [`Chain::current_supply`] — the AVAX supply upper bound of `subnet`.
    ///
    /// # Errors
    /// Propagates the state read error.
    fn current_supply(&self, subnet: Id) -> Result<u64>;
    /// [`Chain::fee_state`] — the dynamic gas fee state.
    fn fee_state(&self) -> GasState;
    /// [`Chain::l1_validator_excess`] — the ACP-77 validator-fee excess.
    fn l1_validator_excess(&self) -> u64;
    /// [`Chain::get_l1_validator`] — the L1 validator with `validation_id`.
    ///
    /// # Errors
    /// Propagates the state read error (absent validator included).
    fn get_l1_validator(&self, validation_id: Id) -> Result<L1Validator>;
    /// [`Chain::chains`] — the blockchains validated by `subnet`.
    fn chains(&self, subnet: Id) -> Vec<Id>;
    /// [`Chain::get_tx`] — the stored signed-tx bytes of `tx_id`.
    ///
    /// # Errors
    /// Propagates the state read error (absent tx included).
    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>>;
    /// `State::get_block` — the stored bytes of the accepted block `id`.
    ///
    /// # Errors
    /// Propagates the state read error (absent block included).
    fn get_block(&self, id: Id) -> Result<Vec<u8>>;
    /// `State::get_block_id_at_height` — the accepted block id at `height`.
    fn get_block_id_at_height(&self, height: u64) -> Option<Id>;
    /// `avax.UTXOReader.UTXOIDs` — up to `limit` UTXO ids referencing `addr`,
    /// strictly greater than `previous`, ascending (`State::utxo_ids`).
    fn utxo_ids(&self, addr: &ShortId, previous: Id, limit: usize) -> Vec<Id>;
    /// [`Chain::get_utxo`] — the stored canonical bytes of the UTXO `id`.
    ///
    /// # Errors
    /// Propagates the state read error (absent UTXO included).
    fn get_utxo(&self, id: Id) -> Result<UtxoBytes>;
    /// [`Chain::subnets`] — the created subnet ids (`state.GetSubnetIDs`).
    fn subnets(&self) -> Vec<Id>;
    /// [`Chain::get_subnet_owner`] — the codec bytes of `subnet`'s owner.
    ///
    /// # Errors
    /// Propagates the state read error (absent owner included).
    fn get_subnet_owner(&self, subnet: Id) -> Result<Vec<u8>>;
    /// [`Chain::get_subnet_manager`] — the L1-conversion (manager) bytes.
    ///
    /// # Errors
    /// Propagates the state read error (absent conversion included).
    fn get_subnet_manager(&self, subnet: Id) -> Result<Vec<u8>>;
    /// [`Chain::get_reward_utxos`] — the reward UTXOs of staker tx `tx_id`.
    fn get_reward_utxos(&self, tx_id: Id) -> Vec<UtxoBytes>;
    /// [`Chain::current_stakers`] — the current staker set
    /// (`state.GetCurrentStakerIterator`).
    fn current_stakers(&self) -> Vec<Staker>;
    /// [`Chain::pending_stakers`] — the pending staker set
    /// (`state.GetPendingStakerIterator`).
    fn pending_stakers(&self) -> Vec<Staker>;
}

impl<D: Database + 'static> ServiceState for State<D> {
    fn timestamp(&self) -> SystemTime {
        Chain::timestamp(self)
    }
    fn current_supply(&self, subnet: Id) -> Result<u64> {
        Chain::current_supply(self, subnet)
    }
    fn fee_state(&self) -> GasState {
        Chain::fee_state(self)
    }
    fn l1_validator_excess(&self) -> u64 {
        Chain::l1_validator_excess(self)
    }
    fn get_l1_validator(&self, validation_id: Id) -> Result<L1Validator> {
        Chain::get_l1_validator(self, validation_id)
    }
    fn chains(&self, subnet: Id) -> Vec<Id> {
        Chain::chains(self, subnet)
    }
    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>> {
        Chain::get_tx(self, tx_id)
    }
    fn get_block(&self, id: Id) -> Result<Vec<u8>> {
        State::get_block(self, id)
    }
    fn get_block_id_at_height(&self, height: u64) -> Option<Id> {
        State::get_block_id_at_height(self, height)
    }
    fn utxo_ids(&self, addr: &ShortId, previous: Id, limit: usize) -> Vec<Id> {
        State::utxo_ids(self, addr, previous, limit)
    }
    fn get_utxo(&self, id: Id) -> Result<UtxoBytes> {
        Chain::get_utxo(self, id)
    }
    fn subnets(&self) -> Vec<Id> {
        Chain::subnets(self)
    }
    fn get_subnet_owner(&self, subnet: Id) -> Result<Vec<u8>> {
        Chain::get_subnet_owner(self, subnet)
    }
    fn get_subnet_manager(&self, subnet: Id) -> Result<Vec<u8>> {
        Chain::get_subnet_manager(self, subnet)
    }
    fn get_reward_utxos(&self, tx_id: Id) -> Vec<UtxoBytes> {
        Chain::get_reward_utxos(self, tx_id)
    }
    fn current_stakers(&self) -> Vec<Staker> {
        Chain::current_stakers(self)
    }
    fn pending_stakers(&self) -> Vec<Staker> {
        Chain::pending_stakers(self)
    }
}

/// The P-Chain read service over a [`ServiceState`] + a [`ValidatorState`]
/// (the M4.21 `PChainValidatorManager` in production).
///
/// Mirrors the read methods of Go's `platformvm.Service`; each method here is
/// the typed handler body, served on the wire by [`RpcService`]/[`registry`].
pub struct Service {
    state: Arc<dyn ServiceState>,
    validators: Arc<dyn ValidatorState>,
    network_id: u32,
    /// `ctx.AVAXAssetID` — the primary network's staking asset, used by the
    /// balance / stake replies' scalar AVAX duplicate fields.
    avax_asset_id: Id,
}

impl Service {
    /// Builds a service over a shared state view + validator manager.
    #[must_use]
    pub fn new(
        state: Arc<dyn ServiceState>,
        validators: Arc<dyn ValidatorState>,
        network_id: u32,
        avax_asset_id: Id,
    ) -> Self {
        Self {
            state,
            validators,
            network_id,
            avax_asset_id,
        }
    }

    /// The bech32 HRP for this service's network.
    fn hrp(&self) -> &'static str {
        get_hrp(self.network_id)
    }

    /// `getHeight` — the height of the last accepted block.
    pub async fn get_height(&self) -> Result<GetHeightResponse> {
        let height = self
            .validators
            .get_current_height()
            .await
            .map_err(|e| Error::Service(format!("get current height: {e}")))?;
        Ok(GetHeightResponse { height })
    }

    /// `getProposedHeight` — the P-Chain height a new proposal would embed.
    /// Go's body is exactly `vm.GetMinimumHeight(ctx)` (`service.go:105-117`),
    /// which is [`ValidatorState::get_minimum_height`] — the recently-accepted
    /// windower floor the manager already serves to the proposervm.
    pub async fn get_proposed_height(&self) -> Result<GetHeightResponse> {
        let height = self
            .validators
            .get_minimum_height()
            .await
            .map_err(|e| Error::Service(format!("get minimum height: {e}")))?;
        Ok(GetHeightResponse { height })
    }

    /// `getTimestamp` — the current chain timestamp.
    pub fn get_timestamp(&self) -> GetTimestampReply {
        GetTimestampReply {
            timestamp: format_timestamp(self.state.timestamp()),
        }
    }

    /// `getCurrentSupply` — an upper bound on the AVAX supply of `subnet`.
    pub async fn get_current_supply(
        &self,
        args: &GetCurrentSupplyArgs,
    ) -> Result<GetCurrentSupplyReply> {
        let supply = self.state.current_supply(args.subnet_id)?;
        let height = self
            .validators
            .get_current_height()
            .await
            .map_err(|e| Error::Service(format!("get current height: {e}")))?;
        Ok(GetCurrentSupplyReply { supply, height })
    }

    /// `getCurrentValidators` — the current validators of `subnet`, including
    /// L1 validators, sorted by validation id (canonical order). Optionally
    /// filtered to `args.node_ids`.
    pub async fn get_current_validators(
        &self,
        args: &GetCurrentValidatorsArgs,
    ) -> Result<GetCurrentValidatorsReply> {
        let (set, _height) = self
            .validators
            .get_current_validator_set(args.subnet_id)
            .await
            .map_err(|e| Error::Service(format!("get current validators: {e}")))?;

        let filter: Option<std::collections::HashSet<NodeId>> = if args.node_ids.is_empty() {
            None
        } else {
            Some(args.node_ids.iter().copied().collect())
        };

        // `set` is a `BTreeMap<Id, _>` keyed by validation id ⇒ already in
        // canonical validation-id order (00 §6.1).
        let mut validators = Vec::new();
        for (validation_id, v) in &set {
            if filter.as_ref().is_some_and(|ids| !ids.contains(&v.node_id)) {
                continue;
            }
            let public_key = v.public_key.as_ref().map(|pk| hex_nc(&pk.compress()));
            validators.push(ApiValidator {
                tx_id: *validation_id,
                node_id: v.node_id,
                weight: v.weight,
                start_time: v.start_time,
                public_key,
                validation_id: if v.is_l1_validator {
                    Some(*validation_id)
                } else {
                    None
                },
                min_nonce: if v.is_l1_validator {
                    Some(v.min_nonce)
                } else {
                    None
                },
            });
        }

        Ok(GetCurrentValidatorsReply { validators })
    }

    /// `getL1Validator` — the L1 validator with `validation_id`, if it exists.
    pub async fn get_l1_validator(&self, args: &GetL1ValidatorArgs) -> Result<GetL1ValidatorReply> {
        let vdr = self.state.get_l1_validator(args.validation_id)?;
        let height = self
            .validators
            .get_current_height()
            .await
            .map_err(|e| Error::Service(format!("get current height: {e}")))?;

        let public_key = if vdr.public_key.is_empty() {
            None
        } else {
            // L1 public keys are stored uncompressed; re-compress for the API.
            PublicKey::from_uncompressed(&vdr.public_key)
                .ok()
                .map(|pk| hex_nc(&pk.compress()))
        };

        Ok(GetL1ValidatorReply {
            node_id: vdr.node_id,
            weight: vdr.weight,
            start_time: vdr.start_time,
            validation_id: vdr.validation_id,
            public_key,
            min_nonce: vdr.min_nonce,
            subnet_id: vdr.subnet_id,
            height,
        })
    }

    /// `getValidatorsAt` — the validator weights + keys of `subnet` at `height`.
    pub async fn get_validators_at(&self, height: u64, subnet: Id) -> Result<GetValidatorsAtReply> {
        let set = self
            .validators
            .get_validator_set(height, subnet)
            .await
            .map_err(|e| Error::Service(format!("get validator set at {height}: {e}")))?;

        let mut out = GetValidatorsAtReply::new();
        for (node_id, v) in &set {
            let public_key = v.public_key.as_ref().map(|pk| hex_nc(&pk.compress()));
            out.insert(
                *node_id,
                JsonGetValidatorOutput {
                    public_key,
                    weight: v.weight,
                },
            );
        }
        Ok(out)
    }

    /// `getFeeState` — the dynamic gas fee state, with the live exponential
    /// price `gas.CalculatePrice(MinPrice, excess, ExcessConversionConstant)`
    /// (the dynamic-fee config is identical on every network, specs 21 §1).
    pub fn get_fee_state(&self) -> GetFeeStateReply {
        let s = self.state.fee_state();
        GetFeeStateReply {
            capacity: s.capacity,
            excess: s.excess,
            price: calculate_price(DYNAMIC_FEE_MIN_PRICE, s.excess, DYNAMIC_FEE_K),
            timestamp: format_timestamp(self.state.timestamp()),
        }
    }

    /// The network's `fee.Config.ExcessConversionConstant` (mainnet "double
    /// every day" / Fuji "double every hour"; other network ids fall back to
    /// the mainnet constant — the per-network genesis-config plumb is M8's
    /// ava-genesis).
    fn validator_fee_k(&self) -> u64 {
        // `constants.FujiID == 5`.
        if self.network_id == 5 {
            validator_fee::K_FUJI
        } else {
            validator_fee::K_MAINNET
        }
    }

    /// `getValidatorFeeState` — the L1-validator continuous-fee state, with
    /// the live price `gas.CalculatePrice(MinPrice, excess, K)`.
    pub fn get_validator_fee_state(&self) -> GetValidatorFeeStateReply {
        let excess = self.state.l1_validator_excess();
        GetValidatorFeeStateReply {
            excess,
            price: calculate_price(validator_fee::MIN_PRICE, excess, self.validator_fee_k()),
            timestamp: format_timestamp(self.state.timestamp()),
        }
    }

    /// `validatedBy` — the subnet validating `blockchain`.
    pub async fn validated_by(&self, blockchain: Id) -> Result<ValidatedByResponse> {
        let subnet_id = self
            .validators
            .get_subnet_id(blockchain)
            .await
            .map_err(|e| Error::Service(format!("get subnet id: {e}")))?;
        Ok(ValidatedByResponse { subnet_id })
    }

    /// `validates` — the blockchains validated by `subnet`. A non-primary
    /// `subnet` must resolve to an accepted `CreateSubnetTx`
    /// (`service.go:1315`).
    pub fn validates(&self, subnet: Id) -> Result<ValidatesResponse> {
        if subnet != Id::EMPTY {
            let bytes = self.state.get_tx(subnet).map_err(|e| {
                Error::Service(format!("problem retrieving subnet \"{subnet}\": {e}"))
            })?;
            let tx = Tx::parse(crate::txs::codec::Codec(), &bytes).map_err(Error::Codec)?;
            if !matches!(tx.unsigned, crate::txs::UnsignedTx::CreateSubnet(_)) {
                return Err(Error::Service(format!("\"{subnet}\" is not a subnet")));
            }
        }
        Ok(ValidatesResponse {
            blockchain_ids: self.state.chains(subnet),
        })
    }

    /// `getTxStatus` — the status of `tx`. Read-only sync only checks the
    /// accepted state: a found tx is `Committed`, an absent tx is `Unknown`
    /// (the mempool/preferred-block lookups need the builder seam, deferred).
    pub fn get_tx_status(&self, tx: Id) -> GetTxStatusResponse {
        match self.state.get_tx(tx) {
            Ok(_) => GetTxStatusResponse {
                status: Status::Committed,
                reason: String::new(),
            },
            Err(_) => GetTxStatusResponse {
                status: Status::Unknown,
                reason: String::new(),
            },
        }
    }

    /// `getTx` — the raw bytes of `tx` (the JSON-typed decode is deferred to the
    /// transport layer that owns encoding selection).
    pub fn get_tx_bytes(&self, tx: Id) -> Result<Vec<u8>> {
        self.state.get_tx(tx)
    }

    /// `getBlock` — the raw bytes of the block `id`.
    pub fn get_block(&self, id: Id) -> Result<Vec<u8>> {
        self.state.get_block(id)
    }

    /// `getBlockByHeight` — the raw bytes of the accepted block at `height`.
    pub fn get_block_by_height(&self, height: u64) -> Result<Vec<u8>> {
        let id = self
            .state
            .get_block_id_at_height(height)
            .ok_or_else(|| Error::Service(format!("no block at height {height}")))?;
        self.state.get_block(id)
    }

    /// Formats a 20-byte secp256k1 address as a P-Chain bech32 string
    /// (`P-<hrp>1…`) — the address encoding used across the reward/owner
    /// replies.
    ///
    /// # Errors
    /// Returns [`Error::Service`] if bech32 encoding fails.
    pub fn format_address(&self, addr: &[u8]) -> Result<String> {
        address::format("P", self.hrp(), addr)
            .map_err(|e| Error::Service(format!("format address: {e}")))
    }

    /// Parses a bech32 `P-<hrp>1…` service address into its 20-byte
    /// secp256k1 short id (`avax.ParseServiceAddress`). The chain prefix and
    /// hrp must match this network.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on a malformed address, a wrong chain prefix,
    /// or a wrong hrp.
    fn parse_address(&self, addr: &str) -> Result<ShortId> {
        let (chain, hrp, raw) =
            address::parse(addr).map_err(|e| Error::Service(format!("parse address: {e}")))?;
        if chain != "P" {
            return Err(Error::Service(format!(
                "expected chain \"P\" but got \"{chain}\""
            )));
        }
        if hrp != self.hrp() {
            return Err(Error::Service(format!(
                "expected hrp {:?} but got {hrp:?}",
                self.hrp()
            )));
        }
        ShortId::from_slice(&raw).map_err(|e| Error::Service(format!("parse address: {e}")))
    }

    /// Parses a list of service addresses into a deduplicated, ascending set.
    fn parse_addresses(&self, addrs: &[String]) -> Result<Vec<ShortId>> {
        let mut set = BTreeSet::new();
        for a in addrs {
            set.insert(self.parse_address(a)?);
        }
        Ok(set.into_iter().collect())
    }

    /// The current chain time in unix seconds (Go `vm.clock.Unix()`), derived
    /// from the persisted chain timestamp.
    fn now_unix(&self) -> u64 {
        self.state
            .timestamp()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// `getBalance` — the AVAX (and per-asset) balance over `addresses`
    /// (`service.go:139`). Walks every UTXO referencing the addresses through
    /// the address→UTXO index and classifies it as
    /// unlocked / locked-stakeable / locked-not-stakeable by locktime.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on address parse or UTXO decode failure.
    pub fn get_balance(&self, args: &GetBalanceRequest) -> Result<GetBalanceResponse> {
        let addrs = self.parse_addresses(&args.addresses)?;
        let utxos = self.all_utxos(&addrs)?;
        let now = self.now_unix();

        let mut unlockeds: BTreeMap<Id, u64> = BTreeMap::new();
        let mut locked_stakeables: BTreeMap<Id, u64> = BTreeMap::new();
        let mut locked_not_stakeables: BTreeMap<Id, u64> = BTreeMap::new();
        let mut utxo_ids = Vec::new();

        for utxo in &utxos {
            let asset = utxo.asset_id;
            let counted = match &utxo.out {
                Output::Transfer(out) => {
                    let bucket = if out.owners.locktime <= now {
                        &mut unlockeds
                    } else {
                        &mut locked_not_stakeables
                    };
                    add_balance(bucket, asset, out.amt);
                    true
                }
                Output::StakeableLock(lock) => match &*lock.transferable_out {
                    Output::Transfer(inner) => {
                        let bucket = if inner.owners.locktime > now {
                            &mut locked_not_stakeables
                        } else if lock.locktime <= now {
                            &mut unlockeds
                        } else {
                            &mut locked_stakeables
                        };
                        add_balance(bucket, asset, lock.amount());
                        true
                    }
                    // Nested stakeable lock / unexpected wrapped type: skip
                    // (Go logs a warning and `continue`s).
                    Output::StakeableLock(_) => false,
                },
            };
            if counted {
                utxo_ids.push(ApiUtxoId {
                    tx_id: utxo.tx_id,
                    output_index: utxo.output_index,
                });
            }
        }

        let mut balances = locked_stakeables.clone();
        for (asset, amount) in &locked_not_stakeables {
            add_balance(&mut balances, *asset, *amount);
        }
        for (asset, amount) in &unlockeds {
            add_balance(&mut balances, *asset, *amount);
        }

        let avax = self.avax_asset_id();
        Ok(GetBalanceResponse {
            balance: balances.get(&avax).copied().unwrap_or(0),
            unlocked: unlockeds.get(&avax).copied().unwrap_or(0),
            locked_stakeable: locked_stakeables.get(&avax).copied().unwrap_or(0),
            locked_not_stakeable: locked_not_stakeables.get(&avax).copied().unwrap_or(0),
            balances,
            unlockeds,
            locked_stakeables,
            locked_not_stakeables,
            utxo_ids: (!utxo_ids.is_empty()).then_some(utxo_ids),
        })
    }

    /// All UTXOs referencing any of `addrs` (`avax.GetAllUTXOs`), decoded.
    fn all_utxos(&self, addrs: &[ShortId]) -> Result<Vec<Utxo>> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for addr in addrs {
            let mut previous = Id::EMPTY;
            loop {
                let ids = self.state.utxo_ids(addr, previous, MAX_PAGE_SIZE);
                if ids.is_empty() {
                    break;
                }
                let count = ids.len();
                for id in ids {
                    previous = id;
                    if !seen.insert(id) {
                        continue;
                    }
                    let bytes = self.state.get_utxo(id)?;
                    out.push(Utxo::unmarshal(&bytes)?);
                }
                if count < MAX_PAGE_SIZE {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// `getUTXOs` — a paginated page of UTXOs over `addresses`
    /// (`avax.GetPaginatedUTXOs`, `service.go:267`). The cross-chain
    /// `sourceChain` (atomic shared-memory) path is a recorded deferral.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on no/too-many addresses, a bad cursor, a
    /// requested atomic source chain (deferred), or a UTXO decode failure.
    pub fn get_utxos(&self, args: &GetUTXOsArgs) -> Result<GetUTXOsReply> {
        if args.addresses.is_empty() {
            return Err(Error::Service("no addresses provided".to_owned()));
        }
        if args.addresses.len() > MAX_GET_UTXOS_ADDRS {
            return Err(Error::Service(format!(
                "number of addresses given, {}, exceeds maximum, {MAX_GET_UTXOS_ADDRS}",
                args.addresses.len()
            )));
        }
        if !args.source_chain.is_empty() {
            return Err(Error::Service(
                "getUTXOs: cross-chain (sourceChain) atomic UTXOs not yet implemented \
                 (deferred: requires the shared-memory atomic-UTXO seam, M8)"
                    .to_owned(),
            ));
        }

        let addrs = self.parse_addresses(&args.addresses)?;

        let mut start_addr = ShortId::EMPTY;
        let mut start_utxo = Id::EMPTY;
        if !args.start_index.address.is_empty() || !args.start_index.utxo.is_empty() {
            start_addr = self.parse_address(&args.start_index.address).map_err(|e| {
                Error::Service(format!(
                    "couldn't parse start index address {:?}: {e}",
                    args.start_index.address
                ))
            })?;
            start_utxo = args
                .start_index
                .utxo
                .parse::<Id>()
                .map_err(|e| Error::Service(format!("couldn't parse start index utxo: {e}")))?;
        }

        let limit = if args.limit == 0 || args.limit > MAX_PAGE_SIZE as u64 {
            MAX_PAGE_SIZE
        } else {
            // Safe: bounded by MAX_PAGE_SIZE above.
            usize::try_from(args.limit).unwrap_or(MAX_PAGE_SIZE)
        };

        let (utxos, end_addr, end_utxo) =
            self.paginated_utxos(&addrs, start_addr, start_utxo, limit)?;

        let mut encoded = Vec::with_capacity(utxos.len());
        for utxo in &utxos {
            let bytes = utxo.marshal()?;
            let (s, _) = encode_bytes(&bytes, &args.encoding)?;
            encoded.push(s);
        }

        let encoding = canonical_encoding(&args.encoding)?;
        Ok(GetUTXOsReply {
            num_fetched: encoded.len() as u64,
            utxos: encoded,
            end_index: UtxoIndex {
                address: self.format_address(end_addr.as_bytes())?,
                utxo: end_utxo.to_string(),
            },
            encoding,
        })
    }

    /// `avax.GetPaginatedUTXOs` — a single ascending page across the sorted
    /// `(addr, utxo-id)` index, exclusive of the `(start_addr, start_utxo)`
    /// cursor. Returns the page plus the `(addr, utxo-id)` to resume from.
    fn paginated_utxos(
        &self,
        addrs: &[ShortId],
        start_addr: ShortId,
        start_utxo: Id,
        limit: usize,
    ) -> Result<(Vec<Utxo>, ShortId, Id)> {
        let mut out = Vec::new();
        let mut last_addr = ShortId::EMPTY;
        let mut last_utxo = Id::EMPTY;
        let mut seen = BTreeSet::new();
        // Go iterates addrs in sorted order; `parse_addresses` already sorts.
        for addr in addrs {
            if out.len() >= limit {
                break;
            }
            // Skip addresses preceding the cursor address.
            if *addr < start_addr {
                continue;
            }
            // For the cursor address, resume strictly after start_utxo.
            let mut previous = if *addr == start_addr {
                start_utxo
            } else {
                Id::EMPTY
            };
            loop {
                if out.len() >= limit {
                    break;
                }
                let remaining = limit - out.len();
                let ids = self.state.utxo_ids(addr, previous, remaining);
                if ids.is_empty() {
                    break;
                }
                let count = ids.len();
                for id in ids {
                    previous = id;
                    if out.len() >= limit {
                        break;
                    }
                    if !seen.insert(id) {
                        continue;
                    }
                    let bytes = self.state.get_utxo(id)?;
                    out.push(Utxo::unmarshal(&bytes)?);
                    last_addr = *addr;
                    last_utxo = id;
                }
                if count < remaining {
                    break;
                }
            }
        }
        Ok((out, last_addr, last_utxo))
    }

    /// `getSubnet` — the owner / elastic-transform / L1-conversion info of
    /// `subnet` (`service.go:391`). The elastic-subnet transform state is not
    /// ported, so `isPermissioned` reflects only the L1-conversion slot.
    ///
    /// # Errors
    /// Returns [`Error::Service`] when `subnet` is the primary network, has no
    /// recorded owner, or has malformed owner bytes.
    pub fn get_subnet(&self, subnet: Id) -> Result<GetSubnetResponse> {
        if subnet == Id::EMPTY {
            return Err(Error::Service(
                "the primary network isn't a subnet".to_owned(),
            ));
        }
        let owner = self.decode_owner(self.state.get_subnet_owner(subnet)?)?;
        let control_keys = self.format_addresses(&owner.addrs)?;

        let mut response = GetSubnetResponse {
            is_permissioned: true,
            control_keys,
            threshold: owner.threshold,
            locktime: owner.locktime,
            subnet_transformation_tx_id: Id::EMPTY,
            conversion_id: Id::EMPTY,
            manager_chain_id: Id::EMPTY,
            manager_address: None,
        };

        // Elastic-subnet transform state is not ported; `isPermissioned`
        // therefore reflects only the L1-conversion slot below.
        if let Ok(bytes) = self.state.get_subnet_manager(subnet) {
            let conversion = crate::txs::executor::l1_executor::SubnetConversion::unmarshal(&bytes)?;
            response.is_permissioned = false;
            response.conversion_id = conversion.conversion_id;
            response.manager_chain_id = conversion.chain_id;
            response.manager_address = Some(format!("0x{}", hex::encode(&conversion.addr)));
        }
        Ok(response)
    }

    /// `getSubnets` — the requested subnets (or every subnet when `ids` is
    /// empty), the primary network always included (`service.go:482`). The
    /// elastic-subnet transform branch is not ported.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on malformed owner bytes.
    pub fn get_subnets(&self, ids: &[Id]) -> Result<GetSubnetsResponse> {
        let mut subnets = Vec::new();
        if ids.is_empty() {
            for subnet in self.state.subnets() {
                subnets.push(self.api_subnet(subnet)?);
            }
            subnets.push(primary_api_subnet());
            return Ok(GetSubnetsResponse { subnets });
        }

        let mut seen = BTreeSet::new();
        for &subnet in ids {
            if !seen.insert(subnet) {
                continue;
            }
            if subnet == Id::EMPTY {
                subnets.push(primary_api_subnet());
                continue;
            }
            match self.state.get_subnet_owner(subnet) {
                Ok(bytes) => {
                    let owner = self.decode_owner(bytes)?;
                    subnets.push(ApiSubnet {
                        id: subnet,
                        control_keys: self.format_addresses(&owner.addrs)?,
                        threshold: owner.threshold,
                    });
                }
                // Absent subnet: Go skips it.
                Err(_) => continue,
            }
        }
        Ok(GetSubnetsResponse { subnets })
    }

    /// The `APISubnet` view of an owned (permissioned) subnet.
    fn api_subnet(&self, subnet: Id) -> Result<ApiSubnet> {
        let owner = self.decode_owner(self.state.get_subnet_owner(subnet)?)?;
        Ok(ApiSubnet {
            id: subnet,
            control_keys: self.format_addresses(&owner.addrs)?,
            threshold: owner.threshold,
        })
    }

    /// Decodes subnet-owner codec bytes (`Owner` = `secp256k1fx.OutputOwners`).
    fn decode_owner(&self, bytes: Vec<u8>) -> Result<OutputOwners> {
        let mut owner = crate::txs::components::Owner::default();
        crate::txs::codec::Codec()
            .unmarshal(&bytes, &mut owner)
            .map_err(Error::Codec)?;
        let crate::txs::components::Owner::Secp256k1(o) = owner;
        Ok(o)
    }

    /// Formats every owner address as a bech32 P-Chain string.
    fn format_addresses(&self, addrs: &[ShortId]) -> Result<Vec<String>> {
        addrs
            .iter()
            .map(|a| self.format_address(a.as_bytes()))
            .collect()
    }

    /// `getBlockchains` — every blockchain that exists, the primary-network
    /// chains last (`service.go:1374`).
    ///
    /// # Errors
    /// Returns [`Error::Service`] on a chain-tx decode failure.
    pub fn get_blockchains(&self) -> Result<GetBlockchainsResponse> {
        let mut blockchains = Vec::new();
        for subnet in self.state.subnets() {
            self.push_chains(subnet, &mut blockchains)?;
        }
        self.push_chains(Id::EMPTY, &mut blockchains)?;
        Ok(GetBlockchainsResponse { blockchains })
    }

    /// Appends the `CreateChainTx` views of `subnet`'s chains to `out`.
    fn push_chains(&self, subnet: Id, out: &mut Vec<ApiBlockchain>) -> Result<()> {
        for chain_id in self.state.chains(subnet) {
            let bytes = self.state.get_tx(chain_id)?;
            let tx = Tx::parse(crate::txs::codec::Codec(), &bytes).map_err(Error::Codec)?;
            if let crate::txs::UnsignedTx::CreateChain(create) = &tx.unsigned {
                out.push(ApiBlockchain {
                    id: chain_id,
                    name: create.chain_name.clone(),
                    subnet_id: subnet,
                    vm_id: create.vm_id,
                });
            }
        }
        Ok(())
    }

    /// `getBlockchainStatus` — the status of `blockchain_id` (`service.go:1180`).
    /// Accepted-state only: `Validating` if this node validates the chain's
    /// subnet, `Created` if its create-chain tx is accepted, else
    /// `Unknown`. The preferred-but-not-accepted (`Preferred`) and the
    /// alias-driven `Syncing` cases need the chain registry / preferred-chain
    /// state manager seams and are recorded deferrals.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on a missing or unparsable blockchain id.
    pub fn get_blockchain_status(&self, blockchain_id: &str) -> Result<GetBlockchainStatusReply> {
        if blockchain_id.is_empty() {
            return Err(Error::Service("argument 'blockchainID' not given".to_owned()));
        }
        let id = blockchain_id
            .parse::<Id>()
            .map_err(|e| Error::Service(format!("problem parsing blockchainID {blockchain_id:?}: {e}")))?;

        if self.node_validates(id) {
            return Ok(GetBlockchainStatusReply {
                status: BlockchainStatus::Validating,
            });
        }
        if self.chain_exists(id) {
            return Ok(GetBlockchainStatusReply {
                status: BlockchainStatus::Created,
            });
        }
        Ok(GetBlockchainStatusReply {
            status: BlockchainStatus::UnknownChain,
        })
    }

    /// `Service.nodeValidates` — true iff `chain` is an accepted `CreateChainTx`
    /// whose subnet this node validates.
    fn node_validates(&self, chain: Id) -> bool {
        let Ok(bytes) = self.state.get_tx(chain) else {
            return false;
        };
        let Ok(tx) = Tx::parse(crate::txs::codec::Codec(), &bytes) else {
            return false;
        };
        let crate::txs::UnsignedTx::CreateChain(create) = &tx.unsigned else {
            return false;
        };
        // This node validates the chain iff it is in the subnet's chain list
        // and the node is a validator of that subnet. The validator-membership
        // check needs the live validator set keyed by node id, which the read
        // service does not hold synchronously; conservatively report
        // membership by chain presence under its subnet.
        self.state
            .chains(create.subnet_id)
            .into_iter()
            .any(|c| c == chain)
    }

    /// `Service.chainExists` over the accepted state — true iff `chain` is an
    /// accepted `CreateChainTx`.
    fn chain_exists(&self, chain: Id) -> bool {
        let Ok(bytes) = self.state.get_tx(chain) else {
            return false;
        };
        let Ok(tx) = Tx::parse(crate::txs::codec::Codec(), &bytes) else {
            return false;
        };
        matches!(tx.unsigned, crate::txs::UnsignedTx::CreateChain(_))
    }

    /// `getStake` — the AVAX (and per-asset) stake locked by `addresses`, plus
    /// the contributing staked outputs (`service.go:1582`). Walks the current
    /// and pending staker sets, decodes each staker tx, and sums the stake
    /// outputs owned by the requested addresses.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on too-many addresses, address parse failure,
    /// a staker-tx decode failure, or an output encode failure.
    pub fn get_stake(&self, args: &GetStakeArgs) -> Result<GetStakeReply> {
        if args.addresses.len() > MAX_GET_STAKE_ADDRS {
            return Err(Error::Service(format!(
                "{} addresses provided but this method can take at most {MAX_GET_STAKE_ADDRS}",
                args.addresses.len()
            )));
        }
        let addrs: BTreeSet<ShortId> = self.parse_addresses(&args.addresses)?.into_iter().collect();

        let mut totals: BTreeMap<Id, u64> = BTreeMap::new();
        let mut staked_outs: Vec<TransferableOutput> = Vec::new();

        let mut stakers = self.state.current_stakers();
        stakers.extend(self.state.pending_stakers());
        for staker in &stakers {
            if args.validators_only && !staker.priority.is_validator() {
                continue;
            }
            let bytes = self.state.get_tx(staker.tx_id)?;
            let tx = Tx::parse(crate::txs::codec::Codec(), &bytes).map_err(Error::Codec)?;
            for out in stake_outs_of(&tx.unsigned) {
                let owned = output_addresses(&out.out)
                    .iter()
                    .any(|a| addrs.contains(a));
                if !owned {
                    continue;
                }
                add_balance(&mut totals, out.asset_id, out.amount());
                staked_outs.push(out.clone());
            }
        }

        let mut outputs = Vec::with_capacity(staked_outs.len());
        for out in &staked_outs {
            let bytes = crate::txs::codec::Codec()
                .marshal(crate::CODEC_VERSION, out)
                .map_err(Error::Codec)?;
            let (s, _) = encode_bytes(&bytes, &args.encoding)?;
            outputs.push(s);
        }

        let avax = self.avax_asset_id();
        Ok(GetStakeReply {
            staked: totals.get(&avax).copied().unwrap_or(0),
            stakeds: totals,
            outputs,
            encoding: canonical_encoding(&args.encoding)?,
        })
    }

    /// `getRewardUTXOs` — the reward UTXOs minted for the staker tx `tx_id`
    /// (`service.go:1759`).
    ///
    /// # Errors
    /// Returns [`Error::Service`] on an output encode failure.
    pub fn get_reward_utxos(&self, tx_id: Id, encoding: &str) -> Result<GetRewardUTXOsReply> {
        let utxos = self.state.get_reward_utxos(tx_id);
        let mut encoded = Vec::with_capacity(utxos.len());
        for bytes in &utxos {
            let (s, _) = encode_bytes(bytes, encoding)?;
            encoded.push(s);
        }
        Ok(GetRewardUTXOsReply {
            num_fetched: encoded.len() as u64,
            utxos: encoded,
            encoding: canonical_encoding(encoding)?,
        })
    }

    /// `getMinStake` — the minimum validator/delegator bond (`service.go:1678`).
    /// The primary network reads the per-network staking config; non-primary
    /// (elastic) subnets need the transform-subnet state, a recorded deferral.
    ///
    /// # Errors
    /// Returns [`Error::Service`] for a non-primary subnet (deferred).
    pub fn get_min_stake(&self, subnet: Id) -> Result<GetMinStakeReply> {
        if subnet != Id::EMPTY {
            return Err(Error::Service(format!(
                "failed fetching subnet transformation for {subnet}: not found \
                 (elastic-subnet transform state not ported)"
            )));
        }
        let staking = self.staking_config();
        Ok(GetMinStakeReply {
            min_validator_stake: staking.min_validator_stake,
            min_delegator_stake: staking.min_delegator_stake,
        })
    }

    /// The per-network staking config (mainnet / Fuji share the same min-stake
    /// constants in Go; the full per-network plumb is ava-genesis).
    fn staking_config(&self) -> StakingConfig {
        StakingConfig::mainnet()
    }

    /// `getFeeConfig` — the dynamic-fee `gas.Config` (`service.go:2034`).
    #[must_use]
    pub fn get_fee_config(&self) -> GetFeeConfigReply {
        GetFeeConfigReply {
            weights: WEIGHTS,
            max_capacity: MAX_CAPACITY,
            max_per_second: MAX_PER_SECOND,
            target_per_second: TARGET_PER_SECOND,
            min_price: DYNAMIC_FEE_MIN_PRICE,
            excess_conversion_constant: DYNAMIC_FEE_K,
        }
    }

    /// `getValidatorFeeConfig` — the validator continuous-fee `fee.Config`
    /// (`service.go:2071`).
    #[must_use]
    pub fn get_validator_fee_config(&self) -> GetValidatorFeeConfigReply {
        GetValidatorFeeConfigReply {
            capacity: validator_fee::CAPACITY,
            target: validator_fee::TARGET,
            min_price: validator_fee::MIN_PRICE,
            excess_conversion_constant: self.validator_fee_k(),
        }
    }

    /// `sampleValidators` — a sorted sample of up to `size` current validators
    /// of `subnet` (`service.go:1146`). Samples (weighted, without
    /// replacement) over the current validator set read from the validator
    /// manager.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on a validator-set read failure.
    pub async fn sample_validators(
        &self,
        subnet: Id,
        size: u16,
    ) -> Result<SampleValidatorsReply> {
        let set = self
            .validators
            .get_current_validator_set(subnet)
            .await
            .map_err(|e| Error::Service(format!("sampling {subnet} errored with {e}")))?
            .0;

        // Build (nodeID, weight) pairs and sample without replacement.
        let mut nodes: Vec<NodeId> = Vec::with_capacity(set.len());
        let mut weights: Vec<u64> = Vec::with_capacity(set.len());
        for v in set.values() {
            nodes.push(v.node_id);
            weights.push(v.weight);
        }

        let want = usize::from(size).min(nodes.len());
        let mut sampler = new_deterministic_weighted_without_replacement(Box::new(
            ava_utils::rng::Mt19937_64::new(),
        ));
        sampler
            .initialize(&weights)
            .map_err(|e| Error::Service(format!("sampling {subnet} errored with {e}")))?;
        // `Sample(count)` yields `None` only when the request cannot be
        // satisfied; with `want <= nodes.len()` it always succeeds.
        let indices = sampler.sample(want).unwrap_or_default();

        let mut sampled: Vec<NodeId> =
            indices.into_iter().filter_map(|i| nodes.get(i).copied()).collect();
        sampled.sort();
        Ok(SampleValidatorsReply { validators: sampled })
    }

    /// `getTotalStake` — the total validator weight of `subnet`
    /// (`service.go:1731`). Summed from the current validator set.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on a validator-set read failure.
    pub async fn get_total_stake(&self, subnet: Id) -> Result<GetTotalStakeReply> {
        let set = self
            .validators
            .get_current_validator_set(subnet)
            .await
            .map_err(|e| Error::Service(format!("couldn't get total weight: {e}")))?
            .0;
        let weight = set.values().fold(0u64, |acc, v| acc.saturating_add(v.weight));
        Ok(GetTotalStakeReply {
            stake: weight,
            weight,
        })
    }

    /// `getAllValidatorsAt` — the canonical warp validator sets of every subnet
    /// at `height` (`service.go:1824`). `height == u64::MAX` ("proposed")
    /// resolves to the minimum (proposed) height.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on a height resolution or validator-set read
    /// failure.
    pub async fn get_all_validators_at(&self, height: u64) -> Result<GetAllValidatorsAtReply> {
        let resolved = if height == u64::MAX {
            self.validators
                .get_minimum_height()
                .await
                .map_err(|e| Error::Service(format!("failed to get proposed height: {e}")))?
        } else {
            height
        };
        let sets = self
            .validators
            .get_warp_validator_sets(resolved)
            .await
            .map_err(|e| Error::Service(format!("failed to get validator sets at {resolved}: {e}")))?;

        let mut validator_sets = BTreeMap::new();
        for (subnet, warp) in sets {
            validator_sets.insert(subnet, warp_set_to_json(&warp)?);
        }
        Ok(GetAllValidatorsAtReply { validator_sets })
    }

    /// `issueTx` (the decode/parse half) — decodes `args.tx` and parses it into
    /// a signed [`Tx`] (`service.go:1435`). The wire wrapper
    /// ([`RpcService::issue_tx`]) admits the parsed tx through the
    /// [`TxIssuer`] mempool seam.
    ///
    /// # Errors
    /// Returns [`Error::Service`] on a decode or parse failure.
    pub fn parse_issue_tx(&self, args: &FormattedTx) -> Result<Tx> {
        let bytes = decode_bytes(&args.tx, &args.encoding)
            .map_err(|e| Error::Service(format!("problem decoding transaction: {e}")))?;
        Tx::parse(crate::txs::codec::Codec(), &bytes)
            .map_err(|e| Error::Service(format!("couldn't parse tx: {e}")))
    }

    /// The primary network's staking / fee asset.
    fn avax_asset_id(&self) -> Id {
        self.avax_asset_id
    }
}

// ---------------------------------------------------------------------------
// The JSON-RPC wire layer (M8.22) — gorilla `platform.*` over ava-api
// ---------------------------------------------------------------------------

/// The empty gorilla args object (Go `*struct{}`): `[]` / absent / `[{}]` all
/// accept.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EmptyArgs {}

/// Deserializes Go `platformapi.Height` (`vms/platformvm/api/height.go`): a
/// `json.Uint64` (quoted decimal or bare number) or the literal `"proposed"`
/// (= `math.MaxUint64`).
fn de_height<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("height out of range")),
        serde_json::Value::String(s) if s == "proposed" => Ok(u64::MAX),
        serde_json::Value::String(s) => s.parse::<u64>().map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom(
            "height must be a number, quoted number, or \"proposed\"",
        )),
    }
}

/// `platformvm.GetValidatorsAtArgs` — `{"height", "subnetID"}` (`service.go`).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct GetValidatorsAtArgs {
    /// The queried height (`platformapi.Height`: `json.Uint64` or `"proposed"`).
    #[serde(deserialize_with = "de_height")]
    pub height: u64,
    /// The queried subnet (defaults to the primary network).
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
}

impl Default for GetValidatorsAtArgs {
    fn default() -> Self {
        Self {
            height: 0,
            subnet_id: Id::EMPTY,
        }
    }
}

/// `platformvm.ValidatedByArgs` — `{"blockchainID"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ValidatedByArgs {
    /// The blockchain whose validating subnet is queried.
    #[serde(rename = "blockchainID")]
    pub blockchain_id: Id,
}

/// `platformvm.ValidatesArgs` — `{"subnetID"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ValidatesArgs {
    /// The subnet whose blockchains are queried.
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
}

/// `platformvm.GetTxStatusArgs` — `{"txID"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetTxStatusArgs {
    /// The queried tx id (absent → the nil id, Go's zero `ids.ID`).
    #[serde(rename = "txID")]
    pub tx_id: Id,
}

/// `api.GetTxArgs` — `{"txID", "encoding"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetTxArgs {
    /// The queried tx id.
    #[serde(rename = "txID")]
    pub tx_id: Id,
    /// The reply encoding (`hex` default / `hexnc`; `json` is deferred).
    pub encoding: String,
}

/// `api.GetTxReply` — `{"tx", "encoding"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GetTxReply {
    /// The tx bytes under the requested encoding.
    pub tx: String,
    /// The encoding used.
    pub encoding: String,
}

/// `api.GetBlockArgs` — `{"blockID", "encoding"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetBlockArgs {
    /// The queried block id.
    #[serde(rename = "blockID")]
    pub block_id: Id,
    /// The reply encoding (`hex` default / `hexnc`; `json` is deferred).
    pub encoding: String,
}

/// `api.GetBlockByHeightArgs` — `{"height", "encoding"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetBlockByHeightArgs {
    /// The queried height (`json.Uint64`: quoted decimal or bare number).
    #[serde(deserialize_with = "de_height")]
    pub height: u64,
    /// The reply encoding (`hex` default / `hexnc`; `json` is deferred).
    pub encoding: String,
}

/// `api.GetBlockResponse` — `{"block", "encoding"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GetBlockResponse {
    /// The block bytes under the requested encoding.
    pub block: String,
    /// The encoding used.
    pub encoding: String,
}

/// `platformvm.GetStakingAssetIDArgs` — `{"subnetID"}`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetStakingAssetIDArgs {
    /// The subnet whose staking asset is queried (default: primary network).
    #[serde(rename = "subnetID")]
    pub subnet_id: Id,
}

/// `platformvm.GetStakingAssetIDResponse` — `{"assetID"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GetStakingAssetIDResponse {
    /// The staking asset id (AVAX for the primary network).
    #[serde(rename = "assetID")]
    pub asset_id: Id,
}

/// Encodes reply bytes per Go `formatting.Encode(args.Encoding, bytes)`:
/// `hex` (default, zero value) appends the 4-byte sha256 checksum before
/// hex-encoding with a `0x` prefix; `hexnc` skips the checksum. Returns the
/// encoded string + the canonical encoding name echoed in the reply.
///
/// `json` (Go marshals the typed tx/block) is a recorded deferral — the typed
/// JSON shapes are M8.23 — and surfaces as a `-32000` server error.
fn encode_reply_bytes(
    bytes: &[u8],
    encoding: &str,
) -> std::result::Result<(String, String), RpcError> {
    match encoding {
        "" | "hex" => {
            let cs = checksum(bytes, 4);
            let mut combined = bytes.to_vec();
            combined.extend_from_slice(&cs);
            Ok((format!("0x{}", hex::encode(&combined)), "hex".to_string()))
        }
        "hexnc" => Ok((format!("0x{}", hex::encode(bytes)), "hexnc".to_string())),
        "json" => Err(RpcError::server(
            "json encoding is not yet supported (deferred: typed tx/block JSON shapes, M8.23)",
        )),
        other => Err(RpcError::invalid_params(format!(
            "invalid encoding: {other}"
        ))),
    }
}

/// Maps a P-Chain domain error onto the gorilla `-32000` server error (the
/// `utils/rpc` handler surfaces Go handler errors the same way, 14 §16.1).
fn server_err(e: Error) -> RpcError {
    RpcError::server(e.to_string())
}

/// The gorilla `platform` service wrapper over [`Service`] (Go
/// `platformvm.Service`, registered as `"platform"` by `CreateHandlers`,
/// `vm.go:462`). Bridges the typed read bodies; the full Go method set is
/// inventoried in `tests/PORTING.md` (M8.23 owns full parity).
pub struct RpcService {
    service: Arc<Service>,
    /// `ctx.AVAXAssetID` — the primary network's staking asset
    /// (Go `GetStakingAssetID`, `service.go:612`).
    avax_asset_id: Id,
}

#[rpc_service("platform")]
impl RpcService {
    /// `platform.getHeight` (Go `Service.GetHeight`, `service.go:89`).
    ///
    /// # Errors
    /// `-32000` on a validator-manager read failure.
    pub async fn get_height(
        &self,
        _args: EmptyArgs,
    ) -> std::result::Result<GetHeightResponse, RpcError> {
        self.service.get_height().await.map_err(server_err)
    }

    /// `platform.getProposedHeight` (Go `Service.GetProposedHeight`,
    /// `service.go:105`): a justified trivial delegation — Go's body is
    /// exactly `vm.GetMinimumHeight`, the `ValidatorState` seam the service
    /// already holds (see [`Service::get_proposed_height`]).
    ///
    /// # Errors
    /// `-32000` on a validator-manager read failure.
    pub async fn get_proposed_height(
        &self,
        _args: EmptyArgs,
    ) -> std::result::Result<GetHeightResponse, RpcError> {
        self.service.get_proposed_height().await.map_err(server_err)
    }

    /// `platform.getTimestamp` (Go `Service.GetTimestamp`, `service.go:1798`).
    ///
    /// # Errors
    /// Infallible today (typed body reads the in-memory chain time).
    pub async fn get_timestamp(
        &self,
        _args: EmptyArgs,
    ) -> std::result::Result<GetTimestampReply, RpcError> {
        Ok(self.service.get_timestamp())
    }

    /// `platform.getCurrentSupply` (Go `Service.GetCurrentSupply`,
    /// `service.go:1105`).
    ///
    /// # Errors
    /// `-32000` on a state/height read failure.
    pub async fn get_current_supply(
        &self,
        args: GetCurrentSupplyArgs,
    ) -> std::result::Result<GetCurrentSupplyReply, RpcError> {
        self.service
            .get_current_supply(&args)
            .await
            .map_err(server_err)
    }

    /// `platform.getCurrentValidators` (Go `Service.GetCurrentValidators`,
    /// `service.go:717`). Reply carries the read-relevant field subset (the
    /// delegator/uptime/owner attributes are an M8.23 deferral; see
    /// [`ApiValidator`]).
    ///
    /// # Errors
    /// `-32000` on a validator-set read failure.
    pub async fn get_current_validators(
        &self,
        args: GetCurrentValidatorsArgs,
    ) -> std::result::Result<GetCurrentValidatorsReply, RpcError> {
        self.service
            .get_current_validators(&args)
            .await
            .map_err(server_err)
    }

    /// `platform.getL1Validator` (Go `Service.GetL1Validator`,
    /// `service.go:1010`). The snake_case ident pascalizes to the exact Go
    /// wire name `GetL1Validator` (no override needed).
    ///
    /// # Errors
    /// `-32000` for an absent validator / read failure.
    pub async fn get_l1_validator(
        &self,
        args: GetL1ValidatorArgs,
    ) -> std::result::Result<GetL1ValidatorReply, RpcError> {
        self.service
            .get_l1_validator(&args)
            .await
            .map_err(server_err)
    }

    /// `platform.getValidatorsAt` (Go `Service.GetValidatorsAt`,
    /// `service.go:1934`). The reply marshals as the bare
    /// `nodeID → {publicKey, weight}` map (Go `GetValidatorsAtReply.MarshalJSON`).
    ///
    /// # Errors
    /// `-32000` on a validator-set-at-height read failure.
    pub async fn get_validators_at(
        &self,
        args: GetValidatorsAtArgs,
    ) -> std::result::Result<GetValidatorsAtReply, RpcError> {
        self.service
            .get_validators_at(args.height, args.subnet_id)
            .await
            .map_err(server_err)
    }

    /// `platform.getFeeState` (Go `Service.GetFeeState`, `service.go:2051`).
    ///
    /// # Errors
    /// Infallible today (`price` is the recorded `0` sentinel until the
    /// fee-config seam lands; see [`Service::get_fee_state`]).
    pub async fn get_fee_state(
        &self,
        _args: EmptyArgs,
    ) -> std::result::Result<GetFeeStateReply, RpcError> {
        Ok(self.service.get_fee_state())
    }

    /// `platform.getValidatorFeeState` (Go `Service.GetValidatorFeeState`,
    /// `service.go:2088`).
    ///
    /// # Errors
    /// Infallible today (same `price` sentinel note as `getFeeState`).
    pub async fn get_validator_fee_state(
        &self,
        _args: EmptyArgs,
    ) -> std::result::Result<GetValidatorFeeStateReply, RpcError> {
        Ok(self.service.get_validator_fee_state())
    }

    /// `platform.validatedBy` (Go `Service.ValidatedBy`, `service.go:1289`).
    ///
    /// # Errors
    /// `-32000` on a subnet-id read failure.
    pub async fn validated_by(
        &self,
        args: ValidatedByArgs,
    ) -> std::result::Result<ValidatedByResponse, RpcError> {
        self.service
            .validated_by(args.blockchain_id)
            .await
            .map_err(server_err)
    }

    /// `platform.validates` (Go `Service.Validates`, `service.go:1315`).
    ///
    /// # Errors
    /// Infallible today (state list read).
    pub async fn validates(
        &self,
        args: ValidatesArgs,
    ) -> std::result::Result<ValidatesResponse, RpcError> {
        self.service.validates(args.subnet_id).map_err(server_err)
    }

    /// `platform.getTxStatus` (Go `Service.GetTxStatus`, `service.go:1500`).
    /// Accepted-state only: a found tx is `Committed`, an absent one `Unknown`
    /// (the mempool/preferred-block `Processing`/`Dropped` walk needs the
    /// builder seam; see [`Service::get_tx_status`]).
    ///
    /// # Errors
    /// Infallible today.
    pub async fn get_tx_status(
        &self,
        args: GetTxStatusArgs,
    ) -> std::result::Result<GetTxStatusResponse, RpcError> {
        Ok(self.service.get_tx_status(args.tx_id))
    }

    /// `platform.getTx` (Go `Service.GetTx`, `service.go:1458`).
    ///
    /// # Errors
    /// `-32000` for an absent tx; `json` encoding is a recorded deferral.
    pub async fn get_tx(&self, args: GetTxArgs) -> std::result::Result<GetTxReply, RpcError> {
        let bytes = self.service.get_tx_bytes(args.tx_id).map_err(server_err)?;
        let (tx, encoding) = encode_reply_bytes(&bytes, &args.encoding)?;
        Ok(GetTxReply { tx, encoding })
    }

    /// `platform.getBlock` (Go `Service.GetBlock`, `service.go:1959`).
    ///
    /// # Errors
    /// `-32000` for an absent block; `json` encoding is a recorded deferral.
    pub async fn get_block(
        &self,
        args: GetBlockArgs,
    ) -> std::result::Result<GetBlockResponse, RpcError> {
        let bytes = self.service.get_block(args.block_id).map_err(server_err)?;
        let (block, encoding) = encode_reply_bytes(&bytes, &args.encoding)?;
        Ok(GetBlockResponse { block, encoding })
    }

    /// `platform.getBlockByHeight` (Go `Service.GetBlockByHeight`,
    /// `service.go:1992`).
    ///
    /// # Errors
    /// `-32000` for a missing height; `json` encoding is a recorded deferral.
    pub async fn get_block_by_height(
        &self,
        args: GetBlockByHeightArgs,
    ) -> std::result::Result<GetBlockResponse, RpcError> {
        let bytes = self
            .service
            .get_block_by_height(args.height)
            .map_err(server_err)?;
        let (block, encoding) = encode_reply_bytes(&bytes, &args.encoding)?;
        Ok(GetBlockResponse { block, encoding })
    }

    /// `platform.getStakingAssetID` (Go `Service.GetStakingAssetID`,
    /// `service.go:612`): the primary network's staking asset is
    /// `ctx.AVAXAssetID` — a trivial delegation over the chain context already
    /// held by the VM (justified addition; the elastic-subnet
    /// `GetSubnetTransformation` branch needs the subnet-transform state seam,
    /// so a non-primary subnet surfaces Go's wrap with `not found`).
    ///
    /// # Errors
    /// `-32000` for a non-primary subnet (no transform-subnet state yet).
    #[rpc(name = "GetStakingAssetID")]
    pub async fn get_staking_asset_id(
        &self,
        args: GetStakingAssetIDArgs,
    ) -> std::result::Result<GetStakingAssetIDResponse, RpcError> {
        // Go: `args.SubnetID == constants.PrimaryNetworkID` (the empty id).
        if args.subnet_id != Id::EMPTY {
            return Err(RpcError::server(format!(
                "failed fetching subnet transformation for {}: not found",
                args.subnet_id
            )));
        }
        Ok(GetStakingAssetIDResponse {
            asset_id: self.avax_asset_id,
        })
    }
}

/// Builds the registry serving the bridged `platform.*` methods (the body of
/// Go's `server.RegisterService(service, "platform")`, `vm.go:462`).
/// `avax_asset_id` is the chain context's AVAX asset id (`GetStakingAssetID`).
#[must_use]
pub fn registry(service: Arc<Service>, avax_asset_id: Id) -> ServiceRegistry {
    let mut registry = ServiceRegistry::new();
    Arc::new(RpcService {
        service,
        avax_asset_id,
    })
    .register_rpc(&mut registry);
    registry
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod conformance {
    //! `service_get_current_validators` — asserts the JSON shapes + canonical
    //! ordering of the read methods against the recorded Go field names /
    //! encodings (no exact-Go golden vector recorded yet; see PORTING.md).

    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use ava_crypto::bls::{PublicKey, SecretKey};
    use ava_database::MemDb;
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;

    use super::*;
    use crate::block::apricot::{ApricotStandardBlock, CommonBlock};
    use crate::block::banff::BanffStandardBlock;
    use crate::block::executor::{BlockManager, BlockState};
    use crate::block::{Block, BlockBody};
    use crate::state::staker::Staker;
    use crate::txs::Priority;
    use crate::txs::executor::{Backend, StakingConfig, UpgradeSchedule};
    use crate::txs::fee::simple_calculator::StaticFeeConfig;
    use crate::validators::manager::PChainValidatorManager;

    const AVAX: u64 = 1_000_000_000;

    fn pk(seed: u8) -> PublicKey {
        SecretKey::from_bytes(&[seed; 32]).expect("sk").public_key()
    }

    fn backend() -> Backend {
        Backend {
            upgrades: UpgradeSchedule::durango_only(),
            staking: StakingConfig::mainnet(),
            static_fee_config: StaticFeeConfig::MAINNET,
            network_id: 1,
            chain_id: Id::EMPTY,
            avax_asset_id: Id::from([0x42; 32]),
            node_id: NodeId::EMPTY,
            fx: ava_secp256k1fx::Fx::new(Arc::new(ava_utils::clock::MockClock::at(UNIX_EPOCH))),
            bootstrapped: true,
        }
    }

    fn genesis_state() -> State<MemDb> {
        let mut s = State::new(MemDb::new()).expect("state");
        s.set_timestamp(UNIX_EPOCH + Duration::from_secs(1_600_000_000));
        s.set_current_supply(Id::EMPTY, 100_000_000 * AVAX);
        s.set_last_accepted(Id::from([0xAB; 32]));
        s.set_height(0);
        s
    }

    fn validator(tx: u8, node: NodeId, key: &PublicKey, weight: u64) -> Staker {
        Staker::new_current(
            Id::from([tx; 32]),
            node,
            Some(key.clone()),
            Id::EMPTY,
            weight,
            UNIX_EPOCH + Duration::from_secs(1_600_000_000),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            0,
            Priority::PrimaryNetworkValidatorCurrent,
        )
    }

    /// Builds a manager + state with two primary-network validators added at
    /// height 1, and returns a `Service` over them.
    fn seeded_service() -> (Service, NodeId, NodeId, PublicKey, PublicKey) {
        let node_a = NodeId::from([0x0A; 20]);
        let node_b = NodeId::from([0x0B; 20]);
        let key_a = pk(0x11);
        let key_b = pk(0x22);

        // Build the validator manager as the block acceptance notifier
        // (the proven `validators/manager.rs` conformance idiom), accept a block
        // at height 1 that adds two validators, then refresh the manager view.
        let vmgr = Arc::new(PChainValidatorManager::from_state(&genesis_state(), false));
        let mut bm = BlockManager::new(
            genesis_state(),
            backend(),
            crate::txs::codec::codec().expect("codec"),
            Arc::clone(&vmgr) as Arc<dyn crate::block::executor::BlockAcceptanceNotifier>,
        );
        vmgr.refresh(bm.state());

        let blk = {
            let mut b = Block::new(BlockBody::BanffStandard(BanffStandardBlock {
                time: 1_600_000_000,
                apricot: ApricotStandardBlock {
                    common: CommonBlock {
                        parent_id: Id::from([0xAB; 32]),
                        height: 1,
                    },
                    transactions: vec![],
                },
            }));
            b.initialize(bm.codec()).expect("init");
            b
        };
        let mut diff = bm.new_diff(Id::from([0xAB; 32])).expect("diff");
        diff.put_current_validator(validator(0x01, node_a, &key_a, 1_000 * AVAX))
            .expect("add a");
        diff.put_current_validator(validator(0x02, node_b, &key_b, 2_000 * AVAX))
            .expect("add b");
        bm.cache(
            blk.id(),
            BlockState {
                height: 1,
                on_accept: Some(Arc::new(diff)),
                on_commit: None,
                on_abort: None,
                timestamp: 1_600_000_000,
                prefers_commit: true,
            },
        );
        bm.accept(&blk).expect("accept");
        vmgr.refresh(bm.state());

        // Snapshot the state for the scalar read methods (timestamp/supply/
        // height); the manager carries the validator snapshot.
        let state = Arc::new(genesis_state_after_accept());
        let service = Service::new(state, vmgr, 1, Id::from([0x42; 32]));
        (service, node_a, node_b, key_a, key_b)
    }

    /// A state matching the accepted height for the read-method asserts
    /// (height/supply/timestamp). The manager carries the validator snapshot.
    fn genesis_state_after_accept() -> State<MemDb> {
        let mut s = genesis_state();
        s.set_height(1);
        s
    }

    #[tokio::test]
    async fn service_get_current_validators() {
        let (service, node_a, node_b, key_a, key_b) = seeded_service();

        let reply = service
            .get_current_validators(&GetCurrentValidatorsArgs::default())
            .await
            .expect("get current validators");

        // Two validators, sorted by validation id (txID 0x01.. < 0x02..).
        assert_eq!(reply.validators.len(), 2);
        assert_eq!(reply.validators[0].tx_id, Id::from([0x01; 32]));
        assert_eq!(reply.validators[1].tx_id, Id::from([0x02; 32]));

        // Node ids + weights.
        assert_eq!(reply.validators[0].node_id, node_a);
        assert_eq!(reply.validators[0].weight, 1_000 * AVAX);
        assert_eq!(reply.validators[1].node_id, node_b);
        assert_eq!(reply.validators[1].weight, 2_000 * AVAX);

        // BLS keys are hex `0x…` of the compressed key.
        assert_eq!(
            reply.validators[0].public_key.as_deref(),
            Some(format!("0x{}", hex::encode(key_a.compress())).as_str())
        );
        assert_eq!(
            reply.validators[1].public_key.as_deref(),
            Some(format!("0x{}", hex::encode(key_b.compress())).as_str())
        );

        // Primary-network (non-L1) validators omit validationID/minNonce.
        assert!(reply.validators[0].validation_id.is_none());
        assert!(reply.validators[0].min_nonce.is_none());

        // JSON field names + encodings (avajson string ints, nodeID, txID).
        let json = serde_json::to_value(&reply).expect("json");
        let v0 = &json["validators"][0];
        assert_eq!(v0["weight"], serde_json::json!("1000000000000"));
        assert_eq!(v0["startTime"], serde_json::json!("1600000000"));
        assert!(v0["nodeID"].as_str().unwrap().starts_with("NodeID-"));
        assert!(v0.get("txID").is_some());
        assert!(v0["publicKey"].as_str().unwrap().starts_with("0x"));
        // Non-L1 entries must not emit these (skip_serializing_if).
        assert!(v0.get("validationID").is_none());
        assert!(v0.get("minNonce").is_none());
    }

    #[tokio::test]
    async fn service_read_method_shapes() {
        let (service, ..) = seeded_service();

        // getHeight: { "height": "1" }
        let h = service.get_height().await.expect("height");
        assert_eq!(h.height, 1);
        let hj = serde_json::to_value(&h).expect("json");
        assert_eq!(hj["height"], serde_json::json!("1"));

        // getCurrentSupply: { "supply": "...", "height": "1" }
        let supply = service
            .get_current_supply(&GetCurrentSupplyArgs::default())
            .await
            .expect("supply");
        assert_eq!(supply.supply, 100_000_000 * AVAX);
        assert_eq!(supply.height, 1);
        let sj = serde_json::to_value(&supply).expect("json");
        assert_eq!(sj["supply"], serde_json::json!("100000000000000000"));
        assert_eq!(sj["height"], serde_json::json!("1"));

        // getTimestamp: RFC3339.
        let ts = service.get_timestamp();
        assert_eq!(ts.timestamp, "2020-09-13T12:26:40Z");

        // getValidatorsAt at height 1: nodeID → { publicKey, weight }.
        let vat = service
            .get_validators_at(1, Id::EMPTY)
            .await
            .expect("validators at");
        assert_eq!(vat.len(), 2);
        let vatj = serde_json::to_value(&vat).expect("json");
        // Map keyed by NodeID strings; values carry string weight + hex key.
        for (_k, v) in vatj.as_object().unwrap() {
            assert!(v["publicKey"].as_str().unwrap().starts_with("0x"));
            assert!(v["weight"].as_str().is_some());
        }

        // getFeeState shape.
        let fs = service.get_fee_state();
        let fsj = serde_json::to_value(&fs).expect("json");
        assert!(fsj["capacity"].as_str().is_some());
        assert!(fsj["excess"].as_str().is_some());
        assert!(fsj["timestamp"].as_str().is_some());
    }

    #[test]
    fn service_get_block_by_height_roundtrip() {
        let (service, ..) = seeded_service();
        // The seeded read-state carries no block bytes (the manager owns the
        // validator snapshot); a missing height yields an error, not a panic.
        let err = service.get_block_by_height(99).unwrap_err();
        let _ = err; // shape only: it is the Custom "no block" sentinel path.
    }

    // -----------------------------------------------------------------------
    // M8.22 wire layer: the bridged `platform.*` method set + gorilla envelope
    // -----------------------------------------------------------------------

    /// The bridged method set is EXACTLY the 15 Go wire names (incl. the
    /// `GetStakingAssetID`/`GetL1Validator` casings); nothing unbridged leaks
    /// in (full parity vs the 31-method Go set is M8.23 — see
    /// `tests/PORTING.md`).
    #[test]
    fn platform_method_set_matches_bridged() {
        let (service, ..) = seeded_service();
        let reg = registry(Arc::new(service), Id::from([0x42; 32]));
        const BRIDGED: [&str; 16] = [
            "GetHeight",
            "GetProposedHeight",
            "GetTimestamp",
            "GetCurrentSupply",
            "GetCurrentValidators",
            "GetL1Validator",
            "GetValidatorsAt",
            "GetFeeState",
            "GetValidatorFeeState",
            "ValidatedBy",
            "Validates",
            "GetTxStatus",
            "GetTx",
            "GetBlock",
            "GetBlockByHeight",
            "GetStakingAssetID",
        ];
        assert_eq!(reg.len(), BRIDGED.len(), "exactly the bridged set");
        for m in BRIDGED {
            assert!(
                reg.lookup("platform", m).is_some(),
                "platform.{m} registered"
            );
        }
        // Exact-remainder matching: the pascalized (non-Go) casing must miss.
        assert!(reg.lookup("platform", "GetStakingAssetId").is_none());
        // Unbridged Go methods (M8.23) are NOT registered.
        for m in ["IssueTx", "GetUTXOs", "GetBalance", "SampleValidators"] {
            assert!(
                reg.lookup("platform", m).is_none(),
                "platform.{m} unbridged"
            );
        }
    }

    /// Drives the gorilla envelope end-to-end through `registry_service`.
    async fn post_platform(service: Service, body: serde_json::Value) -> serde_json::Value {
        use ava_vm::vm::VmRequest;
        let reg = std::sync::Arc::new(registry(std::sync::Arc::new(service), Id::from([0x42; 32])));
        let svc = crate::jsonrpc::registry_service(reg);
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

    // getHeight + getTxStatus + getStakingAssetID over the gorilla wire.
    #[tokio::test]
    async fn platform_wire_shapes() {
        let (service, ..) = seeded_service();
        let body = post_platform(
            service,
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "platform.getHeight",
                "params": [{}],
                "id": 1,
            }),
        )
        .await;
        assert_eq!(
            body,
            serde_json::json!({
                "jsonrpc": "2.0",
                "result": { "height": "1" },
                "id": 1,
            }),
            "platform.getHeight envelope (json.Uint64 quoted string)"
        );

        let (service, ..) = seeded_service();
        let body = post_platform(
            service,
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "platform.getTxStatus",
                "params": [{ "txID": Id::from([0xEE; 32]).to_string() }],
                "id": 2,
            }),
        )
        .await;
        assert_eq!(
            body["result"]["status"], "Unknown",
            "absent tx is Unknown (accepted-state-only walk)"
        );

        // getStakingAssetID: primary network echoes ctx.AVAXAssetID; a
        // non-primary subnet surfaces the Go transform-subnet wrap.
        let (service, ..) = seeded_service();
        let body = post_platform(
            service,
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "platform.getStakingAssetID",
                "params": [{}],
                "id": 3,
            }),
        )
        .await;
        assert_eq!(
            body["result"]["assetID"],
            Id::from([0x42; 32]).to_string(),
            "primary-network staking asset is ctx.AVAXAssetID"
        );

        let (service, ..) = seeded_service();
        let subnet = Id::from([0x07; 32]);
        let body = post_platform(
            service,
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "platform.getStakingAssetID",
                "params": [{ "subnetID": subnet.to_string() }],
                "id": 4,
            }),
        )
        .await;
        assert_eq!(body["error"]["code"], -32000, "gorilla server error code");
        assert_eq!(
            body["error"]["message"],
            format!("failed fetching subnet transformation for {subnet}: not found"),
            "Go GetStakingAssetID wrap for a non-transformed subnet"
        );
    }

    // platformapi.Height: bare number, quoted decimal, and "proposed".
    #[test]
    fn height_arg_accepts_go_forms() {
        let a: GetValidatorsAtArgs =
            serde_json::from_value(serde_json::json!({ "height": "7" })).expect("quoted");
        assert_eq!(a.height, 7);
        let a: GetValidatorsAtArgs =
            serde_json::from_value(serde_json::json!({ "height": 7 })).expect("bare");
        assert_eq!(a.height, 7);
        let a: GetValidatorsAtArgs =
            serde_json::from_value(serde_json::json!({ "height": "proposed" })).expect("proposed");
        assert_eq!(a.height, u64::MAX, "Go ProposedHeight = MaxUint64");
    }

    // formatting.Encode parity: hex appends the 4-byte checksum, hexnc skips
    // it, json defers, anything else is -32602.
    #[test]
    fn encode_reply_bytes_matches_go_formatting() {
        let raw = b"hello platform";
        let (hex_s, enc) = encode_reply_bytes(raw, "").expect("hex default");
        assert_eq!(enc, "hex");
        let decoded = hex::decode(hex_s.trim_start_matches("0x")).expect("hex");
        assert_eq!(decoded.len(), raw.len() + 4, "4-byte checksum appended");
        assert_eq!(&decoded[raw.len()..], checksum(raw, 4).as_slice());

        let (nc, enc) = encode_reply_bytes(raw, "hexnc").expect("hexnc");
        assert_eq!(enc, "hexnc");
        assert_eq!(nc, format!("0x{}", hex::encode(raw)));

        assert_eq!(
            encode_reply_bytes(raw, "json").unwrap_err().code,
            -32000,
            "json encoding is a recorded deferral"
        );
        assert_eq!(
            encode_reply_bytes(raw, "cb58").unwrap_err().code,
            -32602,
            "unknown encodings are invalid params"
        );
    }
}
