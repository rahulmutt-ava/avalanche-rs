// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Args/Reply serde types for the `info` service (specs 12 §3.3, 14 §3).
//!
//! Field names and json tags mirror Go `api/info/service.go` (and the types it
//! embeds: `network/peer/info.go`, `vms/platformvm/signer`, `upgrade/upgrade.go`)
//! **exactly**. Numeric fields typed `json.Uint64` / `json.Uint32` /
//! `json.Float64` in Go serialize as quoted decimal strings (`utils/json`),
//! reproduced here via the [`avajson`] serializers. `set.Set[...]` fields
//! serialize as arrays sorted by their marshaled bytes (Go
//! `set.Set.MarshalJSON` sorts with `bytes.Compare`), reproduced by
//! [`go_sorted_set`].
//!
//! JSON object **key order** differs from Go (serde_json emits keys sorted;
//! encoding/json emits struct fields in declaration order) — this is not
//! semantically observable to JSON clients. Array element order (the sorted
//! sets) IS preserved Go-faithfully.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use chrono::{DateTime, Utc};
use serde::ser::Error as _;
use serde::{Serialize, Serializer};

use ava_types::id::Id;
use ava_types::node_id::NodeId;

// ---------------------------------------------------------------------------
// `avajson` — Go `utils/json` numeric encodings (quoted decimal strings)
// ---------------------------------------------------------------------------

/// avalanchego `utils/json` numeric encodings: `json.Uint64`/`json.Uint32`
/// serialize as quoted decimal strings (`"1234"`), `json.Float64` as a quoted
/// fixed 4-decimal string (`strconv.FormatFloat(f, 'f', 4, 64)`).
pub mod avajson {
    use serde::Serializer;

    /// Serialize a `u64` as a quoted decimal string (Go `json.Uint64`).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_u64<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    /// Serialize a `u32` as a quoted decimal string (Go `json.Uint32`).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_u32<S: Serializer>(v: &u32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    /// Serialize an `f64` as a quoted fixed-4-decimal string (Go
    /// `json.Float64`: `strconv.FormatFloat(f, 'f', 4, 64)`).
    ///
    /// # Errors
    /// Propagates the serializer's error.
    pub fn serialize_f64<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{v:.4}"))
    }
}

/// Serializes a set as a JSON array whose elements are sorted by their
/// **marshaled bytes**, mirroring Go `set.Set.MarshalJSON` (which sorts the
/// per-element JSON encodings with `bytes.Compare`). Note this is a
/// lexicographic sort of the encoded form: a `set.Set[uint32]` emits
/// `[103, 23]` (because `"103" < "23"` byte-wise), and string-like elements
/// (NodeIDs, IDs) sort by their encoded string.
///
/// # Errors
/// Propagates element-serialization errors.
pub fn go_sorted_set<T: Serialize, S: Serializer>(
    set: &BTreeSet<T>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut encoded = Vec::with_capacity(set.len());
    for elem in set {
        encoded.push(serde_json::to_string(elem).map_err(S::Error::custom)?);
    }
    encoded.sort();
    let mut values = Vec::with_capacity(encoded.len());
    for enc in &encoded {
        values.push(serde_json::from_str::<serde_json::Value>(enc).map_err(S::Error::custom)?);
    }
    values.serialize(serializer)
}

/// Serializes an `Option<SocketAddr>` the way Go marshals a zero
/// `netip.AddrPort`: `None` becomes the empty string `""` (Go's
/// `AddrPort.MarshalText` returns `""` for the zero value; `omitempty` does
/// not elide struct-typed fields, so the key is always present).
///
/// # Errors
/// Propagates the serializer's error.
fn serialize_opt_addr<S: Serializer>(v: &Option<SocketAddr>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(addr) => s.collect_str(addr),
        None => s.serialize_str(""),
    }
}

// ---------------------------------------------------------------------------
// Shared args
// ---------------------------------------------------------------------------

/// The empty args object for Go's `*struct{}` (parameterless) methods. The
/// dispatch shim maps absent / empty `params` to `{}`, which this accepts;
/// unknown fields are ignored (matching `encoding/json`).
#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
pub struct EmptyArgs {}

// ---------------------------------------------------------------------------
// getNodeVersion
// ---------------------------------------------------------------------------

/// Reply of `info.getNodeVersion` (Go `GetNodeVersionReply`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GetNodeVersionReply {
    /// `version` — `version.Application.String()` (`"avalanchego/x.y.z"`).
    pub version: String,
    /// `databaseVersion` — `version.CurrentDatabase`.
    #[serde(rename = "databaseVersion")]
    pub database_version: String,
    /// `rpcProtocolVersion` — `version.RPCChainVMProtocol` (`json.Uint32` ⇒
    /// quoted string).
    #[serde(rename = "rpcProtocolVersion", serialize_with = "avajson::serialize_u32")]
    pub rpc_protocol_version: u32,
    /// `gitCommit` — the build-time `version.GitCommit`.
    #[serde(rename = "gitCommit")]
    pub git_commit: String,
    /// `vmVersions` — per-VM version strings from `vms.Manager.Versions()`.
    #[serde(rename = "vmVersions")]
    pub vm_versions: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// getNodeID
// ---------------------------------------------------------------------------

/// The node's BLS proof of possession (Go `signer.ProofOfPossession`).
///
/// Serializes as `{publicKey, proofOfPossession}` with both fields encoded as
/// `formatting.HexNC` (`0x`-prefixed hex, **no checksum**), matching Go's
/// `ProofOfPossession.MarshalJSON`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofOfPossession {
    /// The compressed (G1) BLS public key — 48 bytes.
    pub public_key: [u8; 48],
    /// The BLS proof-of-possession signature over the public key — 96 bytes.
    pub proof_of_possession: [u8; 96],
}

impl ProofOfPossession {
    /// Builds a [`ProofOfPossession`] from a compressed public key and
    /// signature.
    #[must_use]
    pub fn new(public_key: [u8; 48], proof_of_possession: [u8; 96]) -> Self {
        Self {
            public_key,
            proof_of_possession,
        }
    }
}

impl Serialize for ProofOfPossession {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire {
            #[serde(rename = "publicKey")]
            public_key: String,
            #[serde(rename = "proofOfPossession")]
            proof_of_possession: String,
        }
        Wire {
            public_key: format!("0x{}", hex::encode(self.public_key)),
            proof_of_possession: format!("0x{}", hex::encode(self.proof_of_possession)),
        }
        .serialize(serializer)
    }
}

/// Reply of `info.getNodeID` (Go `GetNodeIDReply`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GetNodeIdReply {
    /// `nodeID` — the node's ID (`"NodeID-..."`).
    #[serde(rename = "nodeID")]
    pub node_id: NodeId,
    /// `nodePOP` — the node's BLS proof of possession.
    #[serde(rename = "nodePOP")]
    pub node_pop: ProofOfPossession,
}

// ---------------------------------------------------------------------------
// getNodeIP / getNetworkID / getNetworkName
// ---------------------------------------------------------------------------

/// Reply of `info.getNodeIP` (Go `GetNodeIPReply`): `ip` as `"host:port"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct GetNodeIpReply {
    /// `ip` — this node's external `host:port` (Go `netip.AddrPort` text).
    pub ip: SocketAddr,
}

/// Reply of `info.getNetworkID` (Go `GetNetworkIDReply`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct GetNetworkIdReply {
    /// `networkID` — `json.Uint32` ⇒ quoted string.
    #[serde(rename = "networkID", serialize_with = "avajson::serialize_u32")]
    pub network_id: u32,
}

/// Reply of `info.getNetworkName` (Go `GetNetworkNameReply`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GetNetworkNameReply {
    /// `networkName` — `constants.NetworkName(networkID)`.
    #[serde(rename = "networkName")]
    pub network_name: String,
}

// ---------------------------------------------------------------------------
// getBlockchainID
// ---------------------------------------------------------------------------

/// Args of `info.getBlockchainID` (Go `GetBlockchainIDArgs`).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct GetBlockchainIdArgs {
    /// `alias` — the chain alias to resolve.
    #[serde(default)]
    pub alias: String,
}

/// Reply of `info.getBlockchainID` (Go `GetBlockchainIDReply`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct GetBlockchainIdReply {
    /// `blockchainID` — the resolved chain ID (CB58).
    #[serde(rename = "blockchainID")]
    pub blockchain_id: Id,
}

// ---------------------------------------------------------------------------
// peers
// ---------------------------------------------------------------------------

/// Args of `info.peers` (Go `PeersArgs`). An empty `nodeIDs` returns all
/// connected peers.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct PeersArgs {
    /// `nodeIDs` — filter to these peers (empty ⇒ all).
    #[serde(rename = "nodeIDs", default)]
    pub node_ids: Vec<NodeId>,
}

/// A connected peer as reported by the networking layer — the Go
/// `network/peer.Info` mirror the [`InfoNetwork`](super::InfoNetwork) seam
/// returns. Combined with the benched aliases into [`Peer`] by the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInfo {
    /// The remote `host:port` of the connection.
    pub ip: SocketAddr,
    /// The peer's advertised public `host:port`; `None` serializes as `""`
    /// (Go's zero `netip.AddrPort`).
    pub public_ip: Option<SocketAddr>,
    /// The peer's NodeID.
    pub node_id: NodeId,
    /// The peer's reported client version string.
    pub version: String,
    /// The peer's advertised upgrade time (unix seconds; plain JSON number).
    pub upgrade_time: u64,
    /// When we last sent a message to the peer.
    pub last_sent: DateTime<Utc>,
    /// When we last received a message from the peer.
    pub last_received: DateTime<Utc>,
    /// The peer's observed uptime of *this* node, percent (`json.Uint32`).
    pub observed_uptime: u32,
    /// The subnets the peer tracks.
    pub tracked_subnets: BTreeSet<Id>,
    /// The ACPs the peer signals support for.
    pub supported_acps: BTreeSet<u32>,
    /// The ACPs the peer signals objection to.
    pub objected_acps: BTreeSet<u32>,
}

/// One entry of `info.peers` — Go's `Peer` (embedded `peer.Info` + `benched`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Peer {
    /// `ip` — remote `host:port`.
    pub ip: SocketAddr,
    /// `publicIP` — advertised public `host:port` (`""` when unknown).
    #[serde(rename = "publicIP", serialize_with = "serialize_opt_addr")]
    pub public_ip: Option<SocketAddr>,
    /// `nodeID` — the peer's NodeID.
    #[serde(rename = "nodeID")]
    pub node_id: NodeId,
    /// `version` — the peer's client version string.
    pub version: String,
    /// `upgradeTime` — plain JSON number (Go `uint64`).
    #[serde(rename = "upgradeTime")]
    pub upgrade_time: u64,
    /// `lastSent` — RFC 3339 timestamp.
    #[serde(rename = "lastSent")]
    pub last_sent: DateTime<Utc>,
    /// `lastReceived` — RFC 3339 timestamp.
    #[serde(rename = "lastReceived")]
    pub last_received: DateTime<Utc>,
    /// `observedUptime` — `json.Uint32` ⇒ quoted string.
    #[serde(rename = "observedUptime", serialize_with = "avajson::serialize_u32")]
    pub observed_uptime: u32,
    /// `trackedSubnets` — `set.Set[ids.ID]` ⇒ sorted array of CB58 strings.
    #[serde(rename = "trackedSubnets", serialize_with = "go_sorted_set")]
    pub tracked_subnets: BTreeSet<Id>,
    /// `supportedACPs` — `set.Set[uint32]` ⇒ array sorted by marshaled bytes.
    #[serde(rename = "supportedACPs", serialize_with = "go_sorted_set")]
    pub supported_acps: BTreeSet<u32>,
    /// `objectedACPs` — `set.Set[uint32]` ⇒ array sorted by marshaled bytes.
    #[serde(rename = "objectedACPs", serialize_with = "go_sorted_set")]
    pub objected_acps: BTreeSet<u32>,
    /// `benched` — primary aliases of the chains this peer is benched on.
    pub benched: Vec<String>,
}

impl Peer {
    /// Combines the networking-layer [`PeerInfo`] with the benchlist aliases
    /// (mirror the loop in Go `Info.Peers`).
    #[must_use]
    pub fn new(info: PeerInfo, benched: Vec<String>) -> Self {
        Self {
            ip: info.ip,
            public_ip: info.public_ip,
            node_id: info.node_id,
            version: info.version,
            upgrade_time: info.upgrade_time,
            last_sent: info.last_sent,
            last_received: info.last_received,
            observed_uptime: info.observed_uptime,
            tracked_subnets: info.tracked_subnets,
            supported_acps: info.supported_acps,
            objected_acps: info.objected_acps,
            benched,
        }
    }
}

/// Reply of `info.peers` (Go `PeersReply`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PeersReply {
    /// `numPeers` — `json.Uint64` ⇒ quoted string; `len(peers)`.
    #[serde(rename = "numPeers", serialize_with = "avajson::serialize_u64")]
    pub num_peers: u64,
    /// `peers` — one entry per connected (matching) peer.
    pub peers: Vec<Peer>,
}

// ---------------------------------------------------------------------------
// isBootstrapped
// ---------------------------------------------------------------------------

/// Args of `info.isBootstrapped` (Go `IsBootstrappedArgs`).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct IsBootstrappedArgs {
    /// `chain` — alias or string ID of the chain.
    #[serde(default)]
    pub chain: String,
}

/// Reply of `info.isBootstrapped` (Go `IsBootstrappedResponse`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct IsBootstrappedResponse {
    /// `isBootstrapped` — true iff the chain exists and finished bootstrapping.
    #[serde(rename = "isBootstrapped")]
    pub is_bootstrapped: bool,
}

// ---------------------------------------------------------------------------
// upgrades
// ---------------------------------------------------------------------------

/// Reply of `info.upgrades` — the whole upgrade schedule, mirroring Go
/// `upgrade.Config`'s json tags (`upgrade/upgrade.go`). Times marshal as
/// RFC 3339; `apricotPhase4MinPChainHeight` is a plain number;
/// `cortinaXChainStopVertexID` is a CB58 ID; `graniteEpochDuration` is a plain
/// number of **nanoseconds** (Go `time.Duration`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpgradesReply {
    /// `apricotPhase1Time`.
    #[serde(rename = "apricotPhase1Time")]
    pub apricot_phase_1_time: DateTime<Utc>,
    /// `apricotPhase2Time`.
    #[serde(rename = "apricotPhase2Time")]
    pub apricot_phase_2_time: DateTime<Utc>,
    /// `apricotPhase3Time`.
    #[serde(rename = "apricotPhase3Time")]
    pub apricot_phase_3_time: DateTime<Utc>,
    /// `apricotPhase4Time`.
    #[serde(rename = "apricotPhase4Time")]
    pub apricot_phase_4_time: DateTime<Utc>,
    /// `apricotPhase4MinPChainHeight` — plain JSON number (Go `uint64`).
    #[serde(rename = "apricotPhase4MinPChainHeight")]
    pub apricot_phase_4_min_p_chain_height: u64,
    /// `apricotPhase5Time`.
    #[serde(rename = "apricotPhase5Time")]
    pub apricot_phase_5_time: DateTime<Utc>,
    /// `apricotPhasePre6Time`.
    #[serde(rename = "apricotPhasePre6Time")]
    pub apricot_phase_pre_6_time: DateTime<Utc>,
    /// `apricotPhase6Time`.
    #[serde(rename = "apricotPhase6Time")]
    pub apricot_phase_6_time: DateTime<Utc>,
    /// `apricotPhasePost6Time`.
    #[serde(rename = "apricotPhasePost6Time")]
    pub apricot_phase_post_6_time: DateTime<Utc>,
    /// `banffTime`.
    #[serde(rename = "banffTime")]
    pub banff_time: DateTime<Utc>,
    /// `cortinaTime`.
    #[serde(rename = "cortinaTime")]
    pub cortina_time: DateTime<Utc>,
    /// `cortinaXChainStopVertexID` — CB58 ID string.
    #[serde(rename = "cortinaXChainStopVertexID")]
    pub cortina_x_chain_stop_vertex_id: Id,
    /// `durangoTime`.
    #[serde(rename = "durangoTime")]
    pub durango_time: DateTime<Utc>,
    /// `etnaTime`.
    #[serde(rename = "etnaTime")]
    pub etna_time: DateTime<Utc>,
    /// `fortunaTime`.
    #[serde(rename = "fortunaTime")]
    pub fortuna_time: DateTime<Utc>,
    /// `graniteTime`.
    #[serde(rename = "graniteTime")]
    pub granite_time: DateTime<Utc>,
    /// `graniteEpochDuration` — plain JSON number of nanoseconds (Go
    /// `time.Duration`, an `int64`).
    #[serde(rename = "graniteEpochDuration")]
    pub granite_epoch_duration: i64,
    /// `heliconTime`.
    #[serde(rename = "heliconTime")]
    pub helicon_time: DateTime<Utc>,
}

impl From<&ava_version::upgrade::UpgradeConfig> for UpgradesReply {
    fn from(c: &ava_version::upgrade::UpgradeConfig) -> Self {
        Self {
            apricot_phase_1_time: c.apricot_phase_1_time,
            apricot_phase_2_time: c.apricot_phase_2_time,
            apricot_phase_3_time: c.apricot_phase_3_time,
            apricot_phase_4_time: c.apricot_phase_4_time,
            apricot_phase_4_min_p_chain_height: c.apricot_phase_4_min_p_chain_height,
            apricot_phase_5_time: c.apricot_phase_5_time,
            apricot_phase_pre_6_time: c.apricot_phase_pre_6_time,
            apricot_phase_6_time: c.apricot_phase_6_time,
            apricot_phase_post_6_time: c.apricot_phase_post_6_time,
            banff_time: c.banff_time,
            cortina_time: c.cortina_time,
            cortina_x_chain_stop_vertex_id: c.cortina_x_chain_stop_vertex_id,
            durango_time: c.durango_time,
            etna_time: c.etna_time,
            fortuna_time: c.fortuna_time,
            granite_time: c.granite_time,
            // Go time.Duration is an int64 of nanoseconds; epoch durations are
            // seconds-scale, so saturate rather than fail on a (nonsensical)
            // >292-year duration.
            granite_epoch_duration: i64::try_from(c.granite_epoch_duration.as_nanos())
                .unwrap_or(i64::MAX),
            helicon_time: c.helicon_time,
        }
    }
}

// ---------------------------------------------------------------------------
// uptime
// ---------------------------------------------------------------------------

/// A node-uptime query result as returned by the networking layer (Go
/// `network.UptimeResult`) — the [`InfoNetwork`](super::InfoNetwork) seam type.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UptimeResult {
    /// Percent of network stake that thinks we are above the uptime
    /// requirement.
    pub rewarding_stake_percentage: f64,
    /// Average perceived uptime of this node, weighted by stake.
    pub weighted_average_percentage: f64,
}

/// Reply of `info.uptime` (Go `UptimeResponse`); both fields are
/// `json.Float64` ⇒ quoted fixed-4-decimal strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct UptimeReply {
    /// `rewardingStakePercentage`.
    #[serde(
        rename = "rewardingStakePercentage",
        serialize_with = "avajson::serialize_f64"
    )]
    pub rewarding_stake_percentage: f64,
    /// `weightedAveragePercentage`.
    #[serde(
        rename = "weightedAveragePercentage",
        serialize_with = "avajson::serialize_f64"
    )]
    pub weighted_average_percentage: f64,
}

// ---------------------------------------------------------------------------
// acps
// ---------------------------------------------------------------------------

/// The set of ACPs that are, at the time of release, marked implementable but
/// not yet activated — every entry receives an `abstainWeight` tally in
/// `info.acps`. Mirrors Go `constants.CurrentACPs`, which is **empty** at the
/// pinned upstream (all listed ACPs are activated; see
/// `utils/constants/acps.go`).
pub const CURRENT_ACPS: &[u32] = &[];

/// One ACP tally of `info.acps` (Go `ACP`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Acp {
    /// `supportWeight` — `json.Uint64` ⇒ quoted string.
    #[serde(rename = "supportWeight", serialize_with = "avajson::serialize_u64")]
    pub support_weight: u64,
    /// `supporters` — `set.Set[ids.NodeID]` ⇒ sorted array of NodeID strings.
    #[serde(serialize_with = "go_sorted_set")]
    pub supporters: BTreeSet<NodeId>,
    /// `objectWeight` — `json.Uint64` ⇒ quoted string.
    #[serde(rename = "objectWeight", serialize_with = "avajson::serialize_u64")]
    pub object_weight: u64,
    /// `objectors` — `set.Set[ids.NodeID]` ⇒ sorted array of NodeID strings.
    #[serde(serialize_with = "go_sorted_set")]
    pub objectors: BTreeSet<NodeId>,
    /// `abstainWeight` — `json.Uint64` ⇒ quoted string.
    #[serde(rename = "abstainWeight", serialize_with = "avajson::serialize_u64")]
    pub abstain_weight: u64,
}

/// Reply of `info.acps` (Go `ACPsReply`): `acps` maps ACP number → tally.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AcpsReply {
    /// `acps` — keyed by ACP number (JSON object keys are decimal strings).
    pub acps: BTreeMap<u32, Acp>,
}

// ---------------------------------------------------------------------------
// getTxFee
// ---------------------------------------------------------------------------

/// 1 nAVAX in nAVAX (Go `units.NanoAvax`).
const NANO_AVAX: u64 = 1;
/// 1 mAVAX in nAVAX (Go `units.MilliAvax`).
const MILLI_AVAX: u64 = 1_000 * 1_000 * NANO_AVAX;
/// 1 AVAX in nAVAX (Go `units.Avax`).
const AVAX: u64 = 1_000 * MILLI_AVAX;

/// Reply of `info.getTxFee` (Go `GetTxFeeResponse`); **deprecated** in Go —
/// all nine fee fields are `json.Uint64` ⇒ quoted strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetTxFeeResponse {
    /// `txFee`.
    #[serde(rename = "txFee", serialize_with = "avajson::serialize_u64")]
    pub tx_fee: u64,
    /// `createAssetTxFee`.
    #[serde(rename = "createAssetTxFee", serialize_with = "avajson::serialize_u64")]
    pub create_asset_tx_fee: u64,
    /// `createSubnetTxFee`.
    #[serde(rename = "createSubnetTxFee", serialize_with = "avajson::serialize_u64")]
    pub create_subnet_tx_fee: u64,
    /// `transformSubnetTxFee`.
    #[serde(
        rename = "transformSubnetTxFee",
        serialize_with = "avajson::serialize_u64"
    )]
    pub transform_subnet_tx_fee: u64,
    /// `createBlockchainTxFee`.
    #[serde(
        rename = "createBlockchainTxFee",
        serialize_with = "avajson::serialize_u64"
    )]
    pub create_blockchain_tx_fee: u64,
    /// `addPrimaryNetworkValidatorFee`.
    #[serde(
        rename = "addPrimaryNetworkValidatorFee",
        serialize_with = "avajson::serialize_u64"
    )]
    pub add_primary_network_validator_fee: u64,
    /// `addPrimaryNetworkDelegatorFee`.
    #[serde(
        rename = "addPrimaryNetworkDelegatorFee",
        serialize_with = "avajson::serialize_u64"
    )]
    pub add_primary_network_delegator_fee: u64,
    /// `addSubnetValidatorFee`.
    #[serde(
        rename = "addSubnetValidatorFee",
        serialize_with = "avajson::serialize_u64"
    )]
    pub add_subnet_validator_fee: u64,
    /// `addSubnetDelegatorFee`.
    #[serde(
        rename = "addSubnetDelegatorFee",
        serialize_with = "avajson::serialize_u64"
    )]
    pub add_subnet_delegator_fee: u64,
}

/// The static mainnet fee table (Go `mainnetGetTxFeeResponse`).
#[must_use]
pub fn mainnet_get_tx_fee_response() -> GetTxFeeResponse {
    GetTxFeeResponse {
        create_subnet_tx_fee: AVAX,
        transform_subnet_tx_fee: 10 * AVAX,
        create_blockchain_tx_fee: AVAX,
        add_primary_network_validator_fee: 0,
        add_primary_network_delegator_fee: 0,
        add_subnet_validator_fee: MILLI_AVAX,
        add_subnet_delegator_fee: MILLI_AVAX,
        ..GetTxFeeResponse::default()
    }
}

/// The static fuji fee table (Go `fujiGetTxFeeResponse`).
#[must_use]
pub fn fuji_get_tx_fee_response() -> GetTxFeeResponse {
    GetTxFeeResponse {
        create_subnet_tx_fee: 100 * MILLI_AVAX,
        transform_subnet_tx_fee: AVAX,
        create_blockchain_tx_fee: 100 * MILLI_AVAX,
        add_primary_network_validator_fee: 0,
        add_primary_network_delegator_fee: 0,
        add_subnet_validator_fee: MILLI_AVAX,
        add_subnet_delegator_fee: MILLI_AVAX,
        ..GetTxFeeResponse::default()
    }
}

/// The static default (non-mainnet, non-fuji) fee table (Go
/// `defaultGetTxFeeResponse`).
#[must_use]
pub fn default_get_tx_fee_response() -> GetTxFeeResponse {
    GetTxFeeResponse {
        create_subnet_tx_fee: 100 * MILLI_AVAX,
        transform_subnet_tx_fee: 100 * MILLI_AVAX,
        create_blockchain_tx_fee: 100 * MILLI_AVAX,
        add_primary_network_validator_fee: 0,
        add_primary_network_delegator_fee: 0,
        add_subnet_validator_fee: MILLI_AVAX,
        add_subnet_delegator_fee: MILLI_AVAX,
        ..GetTxFeeResponse::default()
    }
}

// ---------------------------------------------------------------------------
// getVMs
// ---------------------------------------------------------------------------

/// Reply of `info.getVMs` (Go `GetVMsReply`). Map keys are the CB58 forms of
/// the `ids.ID` keys (how `encoding/json` marshals `map[ids.ID]...`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetVmsReply {
    /// `vms` — VM ID → its non-redundant aliases (`ids.GetRelevantAliases`).
    pub vms: BTreeMap<String, Vec<String>>,
    /// `fxs` — fx ID → fx name (`secp256k1fx`, `nftfx`, `propertyfx`).
    pub fxs: BTreeMap<String, String>,
}

/// Builds the zero-right-padded 32-byte ASCII ID Go uses for hardcoded fx IDs
/// (`ids.ID{'s','e','c','p',...}`).
fn ascii_id(name: &str) -> Id {
    let mut bytes = [0u8; 32];
    for (dst, src) in bytes.iter_mut().zip(name.as_bytes()) {
        *dst = *src;
    }
    Id::from(bytes)
}

/// `(fx ID, fx name)` of the three built-in fxs reported by `info.getVMs`
/// (Go `secp256k1fx.ID/Name`, `nftfx.ID/Name`, `propertyfx.ID/Name`).
#[must_use]
pub fn builtin_fxs() -> [(Id, &'static str); 3] {
    [
        (ascii_id("secp256k1fx"), "secp256k1fx"),
        (ascii_id("nftfx"), "nftfx"),
        (ascii_id("propertyfx"), "propertyfx"),
    ]
}
