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

use std::collections::BTreeMap;
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
use ava_validators::state::ValidatorState;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result};
use crate::jsonrpc::{RpcError, ServiceRegistry};
use crate::state::chain::Chain;
use crate::state::l1_validator::L1Validator;
use crate::state::state::State;
use crate::status::Status;
use crate::txs::fee::gas::GasState;

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
}

/// Formats a 32-byte compressed BLS key as `formatting.HexNC` (`0x…`).
fn hex_nc(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
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

/// `platformvm.GetFeeStateReply`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetFeeStateReply {
    /// Remaining gas capacity.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub capacity: u64,
    /// Accumulated gas excess (the price input).
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub excess: u64,
    /// The current dynamic gas price.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub price: u64,
    /// The chain timestamp (RFC3339).
    pub timestamp: String,
}

/// `platformvm.GetValidatorFeeStateReply`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetValidatorFeeStateReply {
    /// The L1-validator continuous-fee excess.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
    pub excess: u64,
    /// The current validator fee price.
    #[serde(
        serialize_with = "avajson::serialize_u64",
        deserialize_with = "avajson::deserialize_u64"
    )]
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
}

impl Service {
    /// Builds a service over a shared state view + validator manager.
    #[must_use]
    pub fn new(
        state: Arc<dyn ServiceState>,
        validators: Arc<dyn ValidatorState>,
        network_id: u32,
    ) -> Self {
        Self {
            state,
            validators,
            network_id,
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

    /// `getFeeState` — the dynamic gas fee state.
    pub fn get_fee_state(&self) -> GetFeeStateReply {
        let s = self.state.fee_state();
        GetFeeStateReply {
            capacity: s.capacity,
            excess: s.excess,
            // Price computation needs the chain's dynamic-fee config; the
            // read-only seam exposes the excess input. Reported as the excess
            // sentinel until the fee-config seam lands (deferred, M4.28).
            price: 0,
            timestamp: format_timestamp(self.state.timestamp()),
        }
    }

    /// `getValidatorFeeState` — the L1-validator continuous-fee state.
    pub fn get_validator_fee_state(&self) -> GetValidatorFeeStateReply {
        GetValidatorFeeStateReply {
            excess: self.state.l1_validator_excess(),
            price: 0,
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

    /// `validates` — the blockchains validated by `subnet`.
    pub fn validates(&self, subnet: Id) -> ValidatesResponse {
        ValidatesResponse {
            blockchain_ids: self.state.chains(subnet),
        }
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
        Ok(self.service.validates(args.subnet_id))
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
        let service = Service::new(state, vmgr, 1);
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
        const BRIDGED: [&str; 15] = [
            "GetHeight",
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
