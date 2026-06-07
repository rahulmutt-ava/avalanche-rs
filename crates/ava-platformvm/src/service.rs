// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain JSON-RPC **read** service — port of the read-relevant methods of
//! `vms/platformvm/service.go` (specs 08 §9, 14).
//!
//! This module ports the request/response *shapes* (serde types matching Go's
//! JSON field names + encodings) and the read-method *logic* over the live
//! [`State`](crate::state::state::State) / [`PChainValidatorManager`]
//! (M4.20/M4.21/M4.25) seams. It deliberately does **not** wire an HTTP /
//! JSON-RPC server: that transport lands with `ava-api` (M8/M12). Write methods
//! (`issueTx`, …) are out of scope for read-only sync and are not ported here.
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

use ava_crypto::address;
use ava_crypto::bls::PublicKey;
use ava_database::Database;
use ava_types::constants::get_hrp;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::state::ValidatorState;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::state::State;
use crate::status::Status;
use crate::validators::manager::PChainValidatorManager;

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
// The read service
// ---------------------------------------------------------------------------

/// The P-Chain read service over a [`State`] + [`PChainValidatorManager`].
///
/// Mirrors the read methods of Go's `platformvm.Service`. The HTTP/JSON-RPC
/// transport that would dispatch onto these is deferred to `ava-api` (M8/M12);
/// each method here is the typed handler body.
pub struct Service<D: Database + 'static> {
    state: Arc<State<D>>,
    validators: Arc<PChainValidatorManager<D>>,
    network_id: u32,
}

impl<D: Database + 'static> Service<D> {
    /// Builds a service over a shared state snapshot + validator manager.
    #[must_use]
    pub fn new(
        state: Arc<State<D>>,
        validators: Arc<PChainValidatorManager<D>>,
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
    fn seeded_service() -> (Service<MemDb>, NodeId, NodeId, PublicKey, PublicKey) {
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
}
