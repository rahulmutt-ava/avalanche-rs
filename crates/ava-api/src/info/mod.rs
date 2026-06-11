// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `info` API service — `/ext/info`, prefix `info.` (specs 12 §3.3, 14 §3;
//! mirror Go `api/info/service.go`).
//!
//! [`Info`] carries the static [`Parameters`] plus the node-runtime handles.
//! The handles are **narrow local trait seams** ([`ChainManager`],
//! [`InfoNetwork`], [`Benchlist`], [`ValidatorSet`], [`VmManager`]) — the node
//! assembly (M8.29) adapts the real `ava-chains` / `ava-network` /
//! `ava-validators` / VM-registry objects onto them (the deferred-live-handle
//! pattern of `ava-wallet::client`). Each trait exposes ONLY what the Go
//! service calls on the corresponding handle.
//!
//! The 13 methods register through `#[rpc_service("info")]` under the exact Go
//! wire names (acronym-bearing names — `GetNodeID`, `GetNodeIP`,
//! `GetNetworkID`, `GetBlockchainID`, `GetVMs` — via `#[rpc(name = ...)]`,
//! since dispatch matches the remainder after the first letter EXACTLY; 14
//! §16.1). Handler error strings are byte-stable Go messages (they surface as
//! `-32000` json2 errors).

pub mod types;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use parking_lot::RwLock;

use ava_types::constants::{PRIMARY_NETWORK_ID, network_name};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::{Application, CURRENT_DATABASE, RPC_CHAIN_VM_PROTOCOL};

use crate::jsonrpc::{RpcError, ServiceRegistry};
use crate::rpc_service;
use types::{
    Acp, AcpsReply, CURRENT_ACPS, EmptyArgs, GetBlockchainIdArgs, GetBlockchainIdReply,
    GetNetworkIdReply, GetNetworkNameReply, GetNodeIdReply, GetNodeIpReply, GetNodeVersionReply,
    GetTxFeeResponse, GetVmsReply, IsBootstrappedArgs, IsBootstrappedResponse, Peer, PeerInfo,
    PeersArgs, PeersReply, ProofOfPossession, UpgradesReply, UptimeReply, UptimeResult,
    builtin_fxs, default_get_tx_fee_response, fuji_get_tx_fee_response,
    mainnet_get_tx_fee_response,
};

// ---------------------------------------------------------------------------
// Handle seams (Go: chains.Manager / network.Network / benchlist.Manager /
// validators.Manager / vms.Manager)
// ---------------------------------------------------------------------------

/// The slice of Go `chains.Manager` the info service uses: alias resolution
/// (`ids.Aliaser`) plus the bootstrapped check.
///
/// `Err` values are the Go error **messages** (e.g. the aliaser's
/// `"there is no ID with alias: <alias>"`) so handler messages stay
/// byte-stable.
pub trait ChainManager: Send + Sync {
    /// Resolves a chain alias (or stringified ID) to the chain ID
    /// (Go `Lookup`).
    ///
    /// # Errors
    /// The Go aliaser error message when the alias is unknown.
    fn lookup(&self, alias: &str) -> Result<Id, String>;

    /// The primary (first) alias of `chain_id` (Go `PrimaryAlias`).
    ///
    /// # Errors
    /// The Go aliaser error message when the chain has no alias.
    fn primary_alias(&self, chain_id: Id) -> Result<String, String>;

    /// Whether the chain exists and is done bootstrapping
    /// (Go `IsBootstrapped`).
    fn is_bootstrapped(&self, chain_id: Id) -> bool;
}

/// The slice of Go `network.Network` the info service uses.
pub trait InfoNetwork: Send + Sync {
    /// Info for the given peers; **all** connected peers when `node_ids` is
    /// empty (Go `PeerInfo`).
    fn peer_info(&self, node_ids: &[NodeId]) -> Vec<PeerInfo>;

    /// This node's uptime as observed by its peers (Go `NodeUptime`).
    ///
    /// # Errors
    /// The underlying error message (wrapped by the handler into Go's
    /// `"couldn't get node uptime: ..."`).
    fn node_uptime(&self) -> Result<UptimeResult, String>;
}

/// The slice of Go `benchlist.Manager` the info service uses.
pub trait Benchlist: Send + Sync {
    /// The chain IDs `node_id` is currently benched on (Go `GetBenched`).
    fn get_benched(&self, node_id: NodeId) -> Vec<Id>;
}

/// The slice of Go `validators.Manager` the info service uses.
pub trait ValidatorSet: Send + Sync {
    /// The validation weight of `node_id` on `subnet_id` (0 when not a
    /// validator; Go `GetWeight`).
    fn get_weight(&self, subnet_id: Id, node_id: NodeId) -> u64;

    /// The total validation weight of `subnet_id` (Go `TotalWeight`).
    ///
    /// # Errors
    /// The underlying error message (surfaced verbatim, as in Go).
    fn total_weight(&self, subnet_id: Id) -> Result<u64, String>;
}

/// The slice of Go `vms.Manager` (a `Factory` registry + `ids.Aliaser`) the
/// info service uses.
pub trait VmManager: Send + Sync {
    /// Version string per VM, keyed by VM alias (Go `Versions`).
    ///
    /// # Errors
    /// The underlying error message (surfaced verbatim, as in Go).
    fn versions(&self) -> Result<BTreeMap<String, String>, String>;

    /// The IDs of all registered VM factories (Go `ListFactories`).
    ///
    /// # Errors
    /// The underlying error message (surfaced verbatim, as in Go).
    fn list_factories(&self) -> Result<Vec<Id>, String>;

    /// All aliases of `vm_id`, in registration order (Go `Aliases`; the Go
    /// primary aliaser never errors).
    fn aliases(&self, vm_id: Id) -> Vec<String>;
}

// ---------------------------------------------------------------------------
// Parameters + service
// ---------------------------------------------------------------------------

/// The static service parameters (Go `info.Parameters`).
#[derive(Clone, Debug)]
pub struct Parameters {
    /// The node's application version (Go `Version *version.Application`).
    pub version: Application,
    /// The build-time git commit reported by `getNodeVersion` (Go reads the
    /// `version.GitCommit` package global; Rust has no build-info global yet,
    /// so the node assembly injects it here).
    pub git_commit: String,
    /// This node's ID (Go `NodeID`).
    pub node_id: NodeId,
    /// This node's BLS proof of possession (Go `NodePOP`).
    pub node_pop: ProofOfPossession,
    /// The network this node runs on (Go `NetworkID`).
    pub network_id: u32,
    /// The network-upgrade schedule (Go `Upgrades upgrade.Config`).
    pub upgrades: ava_version::upgrade::UpgradeConfig,
    /// The (deprecated) base tx fee, nAVAX (Go `TxFee`).
    pub tx_fee: u64,
    /// The (deprecated) create-asset tx fee, nAVAX (Go `CreateAssetTxFee`).
    pub create_asset_tx_fee: u64,
}

/// The API service for unprivileged info on a node (Go `info.Info`), exposed
/// at `/ext/info` under the `info.` prefix.
pub struct Info {
    parameters: Parameters,
    validators: Arc<dyn ValidatorSet>,
    chain_manager: Arc<dyn ChainManager>,
    vm_manager: Arc<dyn VmManager>,
    /// This node's externally reachable address (Go
    /// `myIP *utils.Atomic[netip.AddrPort]`).
    my_ip: Arc<RwLock<SocketAddr>>,
    network: Arc<dyn InfoNetwork>,
    benchlist: Arc<dyn Benchlist>,
}

impl Info {
    /// Constructs the service from its parameters and node handles (mirror Go
    /// `info.NewService`, minus the HTTP plumbing — registration happens via
    /// [`Info::register_rpc`] into a [`ServiceRegistry`]).
    #[must_use]
    pub fn new(
        parameters: Parameters,
        validators: Arc<dyn ValidatorSet>,
        chain_manager: Arc<dyn ChainManager>,
        vm_manager: Arc<dyn VmManager>,
        my_ip: Arc<RwLock<SocketAddr>>,
        network: Arc<dyn InfoNetwork>,
        benchlist: Arc<dyn Benchlist>,
    ) -> Self {
        Self {
            parameters,
            validators,
            chain_manager,
            vm_manager,
            my_ip,
            network,
            benchlist,
        }
    }
}

#[rpc_service("info")]
impl Info {
    /// `info.getNodeVersion` — the version this node is running (Go
    /// `GetNodeVersion`).
    ///
    /// # Errors
    /// Propagates a `vms.Manager.Versions()` failure as `-32000`.
    pub async fn get_node_version(
        &self,
        _args: EmptyArgs,
    ) -> Result<GetNodeVersionReply, RpcError> {
        tracing::debug!(service = "info", method = "getNodeVersion", "API called");

        let vm_versions = self.vm_manager.versions().map_err(RpcError::server)?;
        Ok(GetNodeVersionReply {
            version: self.parameters.version.display(),
            database_version: CURRENT_DATABASE.to_string(),
            rpc_protocol_version: RPC_CHAIN_VM_PROTOCOL,
            git_commit: self.parameters.git_commit.clone(),
            vm_versions,
        })
    }

    /// `info.getNodeID` — this node's ID and BLS proof of possession (Go
    /// `GetNodeID`).
    ///
    /// # Errors
    /// Infallible (`Result` for the RPC signature).
    #[rpc(name = "GetNodeID")]
    pub async fn get_node_id(&self, _args: EmptyArgs) -> Result<GetNodeIdReply, RpcError> {
        tracing::debug!(service = "info", method = "getNodeID", "API called");

        Ok(GetNodeIdReply {
            node_id: self.parameters.node_id,
            node_pop: self.parameters.node_pop.clone(),
        })
    }

    /// `info.getNodeIP` — this node's external `host:port` (Go `GetNodeIP`).
    ///
    /// # Errors
    /// Infallible (`Result` for the RPC signature).
    #[rpc(name = "GetNodeIP")]
    pub async fn get_node_ip(&self, _args: EmptyArgs) -> Result<GetNodeIpReply, RpcError> {
        tracing::debug!(service = "info", method = "getNodeIP", "API called");

        Ok(GetNodeIpReply {
            ip: *self.my_ip.read(),
        })
    }

    /// `info.getNetworkID` — the network ID this node runs on (Go
    /// `GetNetworkID`).
    ///
    /// # Errors
    /// Infallible (`Result` for the RPC signature).
    #[rpc(name = "GetNetworkID")]
    pub async fn get_network_id(&self, _args: EmptyArgs) -> Result<GetNetworkIdReply, RpcError> {
        tracing::debug!(service = "info", method = "getNetworkID", "API called");

        Ok(GetNetworkIdReply {
            network_id: self.parameters.network_id,
        })
    }

    /// `info.getNetworkName` — the network name this node runs on (Go
    /// `GetNetworkName`).
    ///
    /// # Errors
    /// Infallible (`Result` for the RPC signature).
    pub async fn get_network_name(
        &self,
        _args: EmptyArgs,
    ) -> Result<GetNetworkNameReply, RpcError> {
        tracing::debug!(service = "info", method = "getNetworkName", "API called");

        Ok(GetNetworkNameReply {
            network_name: network_name(self.parameters.network_id),
        })
    }

    /// `info.getBlockchainID` — resolves a chain alias to its blockchain ID
    /// (Go `GetBlockchainID`).
    ///
    /// # Errors
    /// The chain manager's lookup error (Go aliaser message) as `-32000`.
    #[rpc(name = "GetBlockchainID")]
    pub async fn get_blockchain_id(
        &self,
        args: GetBlockchainIdArgs,
    ) -> Result<GetBlockchainIdReply, RpcError> {
        tracing::debug!(service = "info", method = "getBlockchainID", "API called");

        let blockchain_id = self
            .chain_manager
            .lookup(&args.alias)
            .map_err(RpcError::server)?;
        Ok(GetBlockchainIdReply { blockchain_id })
    }

    /// `info.peers` — the currently connected peers, optionally filtered to
    /// `nodeIDs` (Go `Peers`).
    ///
    /// # Errors
    /// Go's `"failed to get primary alias for chain ID <id>: <err>"` when a
    /// benched chain has no primary alias.
    pub async fn peers(&self, args: PeersArgs) -> Result<PeersReply, RpcError> {
        tracing::debug!(service = "info", method = "peers", "API called");

        let infos = self.network.peer_info(&args.node_ids);
        let mut peers = Vec::with_capacity(infos.len());
        for info in infos {
            let benched_ids = self.benchlist.get_benched(info.node_id);
            let mut benched = Vec::with_capacity(benched_ids.len());
            for chain_id in benched_ids {
                let alias = self.chain_manager.primary_alias(chain_id).map_err(|e| {
                    RpcError::server(format!(
                        "failed to get primary alias for chain ID {chain_id}: {e}"
                    ))
                })?;
                benched.push(alias);
            }
            peers.push(Peer::new(info, benched));
        }
        Ok(PeersReply {
            num_peers: peers.len() as u64,
            peers,
        })
    }

    /// `info.isBootstrapped` — whether the chain exists and is done
    /// bootstrapping (Go `IsBootstrapped`).
    ///
    /// # Errors
    /// Go's `"argument 'chain' not given"` when `chain` is empty, and
    /// `"there is no chain with alias/ID '<chain>'"` when unknown.
    pub async fn is_bootstrapped(
        &self,
        args: IsBootstrappedArgs,
    ) -> Result<IsBootstrappedResponse, RpcError> {
        tracing::debug!(
            service = "info",
            method = "isBootstrapped",
            chain = %args.chain,
            "API called"
        );

        if args.chain.is_empty() {
            return Err(RpcError::server("argument 'chain' not given"));
        }
        let chain_id = self.chain_manager.lookup(&args.chain).map_err(|_| {
            RpcError::server(format!("there is no chain with alias/ID '{}'", args.chain))
        })?;
        Ok(IsBootstrappedResponse {
            is_bootstrapped: self.chain_manager.is_bootstrapped(chain_id),
        })
    }

    /// `info.upgrades` — the upgrade schedule this node is running (Go
    /// `Upgrades`).
    ///
    /// # Errors
    /// Infallible (`Result` for the RPC signature).
    pub async fn upgrades(&self, _args: EmptyArgs) -> Result<UpgradesReply, RpcError> {
        tracing::debug!(service = "info", method = "upgrades", "API called");

        Ok(UpgradesReply::from(&self.parameters.upgrades))
    }

    /// `info.uptime` — this node's uptime as perceived by the network (Go
    /// `Uptime`).
    ///
    /// # Errors
    /// Go's `"couldn't get node uptime: <err>"` on a networking failure.
    pub async fn uptime(&self, _args: EmptyArgs) -> Result<UptimeReply, RpcError> {
        tracing::debug!(service = "info", method = "uptime", "API called");

        let result = self
            .network
            .node_uptime()
            .map_err(|e| RpcError::server(format!("couldn't get node uptime: {e}")))?;
        Ok(UptimeReply {
            rewarding_stake_percentage: result.rewarding_stake_percentage,
            weighted_average_percentage: result.weighted_average_percentage,
        })
    }

    /// `info.acps` — the stake-weighted ACP support tally over connected
    /// Primary Network validators (Go `Acps`).
    ///
    /// # Errors
    /// Propagates a `validators.TotalWeight` failure as `-32000`.
    pub async fn acps(&self, _args: EmptyArgs) -> Result<AcpsReply, RpcError> {
        tracing::debug!(service = "info", method = "acps", "API called");

        let mut acps: BTreeMap<u32, Acp> = BTreeMap::new();
        for peer in self.network.peer_info(&[]) {
            let weight = self.validators.get_weight(PRIMARY_NETWORK_ID, peer.node_id);
            if weight == 0 {
                continue;
            }
            for acp_num in &peer.supported_acps {
                let acp = acps.entry(*acp_num).or_default();
                acp.supporters.insert(peer.node_id);
                // Go sums raw uint64s (wrapping on overflow).
                acp.support_weight = acp.support_weight.wrapping_add(weight);
            }
            for acp_num in &peer.objected_acps {
                let acp = acps.entry(*acp_num).or_default();
                acp.objectors.insert(peer.node_id);
                acp.object_weight = acp.object_weight.wrapping_add(weight);
            }
        }

        let total_weight = self
            .validators
            .total_weight(PRIMARY_NETWORK_ID)
            .map_err(RpcError::server)?;
        // Only ACPs in CurrentACPs receive an abstain tally (Go iterates
        // constants.CurrentACPs); Go subtracts raw uint64s (wrapping).
        for acp_num in CURRENT_ACPS {
            let acp = acps.entry(*acp_num).or_default();
            acp.abstain_weight = total_weight
                .wrapping_sub(acp.support_weight)
                .wrapping_sub(acp.object_weight);
        }
        Ok(AcpsReply { acps })
    }

    /// `info.getTxFee` — the static per-network fee table (Go `GetTxFee`;
    /// **deprecated** — Go logs a warning, mirrored here).
    ///
    /// # Errors
    /// Infallible (`Result` for the RPC signature).
    pub async fn get_tx_fee(&self, _args: EmptyArgs) -> Result<GetTxFeeResponse, RpcError> {
        tracing::warn!(
            service = "info",
            method = "getTxFee",
            "deprecated API called"
        );

        let mut reply = match self.parameters.network_id {
            ava_types::constants::MAINNET_ID => mainnet_get_tx_fee_response(),
            ava_types::constants::FUJI_ID => fuji_get_tx_fee_response(),
            _ => default_get_tx_fee_response(),
        };
        reply.tx_fee = self.parameters.tx_fee;
        reply.create_asset_tx_fee = self.parameters.create_asset_tx_fee;
        Ok(reply)
    }

    /// `info.getVMs` — the VMs installed on this node plus the built-in fxs
    /// (Go `GetVMs`).
    ///
    /// # Errors
    /// Propagates a `vms.Manager.ListFactories()` failure as `-32000`.
    #[rpc(name = "GetVMs")]
    pub async fn get_vms(&self, _args: EmptyArgs) -> Result<GetVmsReply, RpcError> {
        tracing::debug!(service = "info", method = "getVMs", "API called");

        let vm_ids = self.vm_manager.list_factories().map_err(RpcError::server)?;
        let mut vms = BTreeMap::new();
        for vm_id in vm_ids {
            let id_str = vm_id.to_string();
            // ids.GetRelevantAliases: drop the redundant alias == id string.
            let aliases = self
                .vm_manager
                .aliases(vm_id)
                .into_iter()
                .filter(|alias| *alias != id_str)
                .collect();
            vms.insert(id_str, aliases);
        }
        let fxs = builtin_fxs()
            .into_iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect();
        Ok(GetVmsReply { vms, fxs })
    }
}

#[cfg(test)]
// `serde_json::Value` indexing (`value["key"]`) returns `Value::Null` on a
// missing key rather than panicking; it is the idiomatic way to assert on
// JSON reply bodies (same allowance as `jsonrpc::tests`).
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::SocketAddr;
    use std::sync::Arc;

    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use ava_types::constants::{FUJI_ID, MAINNET_ID};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_version::CURRENT;

    use super::types::{GetNodeVersionReply, PeerInfo, ProofOfPossession, UptimeResult};
    use super::{Benchlist, ChainManager, Info, InfoNetwork, Parameters, ValidatorSet, VmManager};
    use crate::error::json2_code;
    use crate::jsonrpc::ServiceRegistry;

    // ------------------------------------------------------------------
    // Hand-rolled narrow, configurable mocks for the handle seams (repo
    // convention).
    // ------------------------------------------------------------------

    #[derive(Default)]
    struct MockChainManager {
        /// alias -> chain ID (`Lookup`).
        aliases: BTreeMap<String, Id>,
        /// chain ID -> primary alias (`PrimaryAlias`).
        primary: BTreeMap<Id, String>,
        /// chain IDs that are done bootstrapping.
        bootstrapped: BTreeSet<Id>,
    }

    impl ChainManager for MockChainManager {
        fn lookup(&self, alias: &str) -> Result<Id, String> {
            self.aliases
                .get(alias)
                .copied()
                .ok_or_else(|| format!("there is no ID with alias: {alias}"))
        }

        fn primary_alias(&self, chain_id: Id) -> Result<String, String> {
            self.primary
                .get(&chain_id)
                .cloned()
                .ok_or_else(|| format!("there is no alias for ID: {chain_id}"))
        }

        fn is_bootstrapped(&self, chain_id: Id) -> bool {
            self.bootstrapped.contains(&chain_id)
        }
    }

    struct MockNetwork {
        peers: Vec<PeerInfo>,
        uptime: Result<UptimeResult, String>,
    }

    impl Default for MockNetwork {
        fn default() -> Self {
            Self {
                peers: Vec::new(),
                uptime: Ok(UptimeResult::default()),
            }
        }
    }

    impl InfoNetwork for MockNetwork {
        fn peer_info(&self, node_ids: &[NodeId]) -> Vec<PeerInfo> {
            if node_ids.is_empty() {
                return self.peers.clone();
            }
            self.peers
                .iter()
                .filter(|p| node_ids.contains(&p.node_id))
                .cloned()
                .collect()
        }

        fn node_uptime(&self) -> Result<UptimeResult, String> {
            self.uptime.clone()
        }
    }

    #[derive(Default)]
    struct MockBenchlist {
        benched: BTreeMap<NodeId, Vec<Id>>,
    }

    impl Benchlist for MockBenchlist {
        fn get_benched(&self, node_id: NodeId) -> Vec<Id> {
            self.benched.get(&node_id).cloned().unwrap_or_default()
        }
    }

    #[derive(Default)]
    struct MockValidators {
        weights: BTreeMap<NodeId, u64>,
        total: u64,
    }

    impl ValidatorSet for MockValidators {
        fn get_weight(&self, _subnet_id: Id, node_id: NodeId) -> u64 {
            self.weights.get(&node_id).copied().unwrap_or_default()
        }

        fn total_weight(&self, _subnet_id: Id) -> Result<u64, String> {
            Ok(self.total)
        }
    }

    #[derive(Default)]
    struct MockVmManager {
        versions: BTreeMap<String, String>,
        factories: Vec<Id>,
        aliases: BTreeMap<Id, Vec<String>>,
    }

    impl VmManager for MockVmManager {
        fn versions(&self) -> Result<BTreeMap<String, String>, String> {
            Ok(self.versions.clone())
        }

        fn list_factories(&self) -> Result<Vec<Id>, String> {
            Ok(self.factories.clone())
        }

        fn aliases(&self, vm_id: Id) -> Vec<String> {
            self.aliases.get(&vm_id).cloned().unwrap_or_default()
        }
    }

    // ------------------------------------------------------------------
    // Builders
    // ------------------------------------------------------------------

    fn test_parameters() -> Parameters {
        Parameters {
            version: CURRENT.clone(),
            git_commit: "de4da4de".to_string(),
            node_id: NodeId::from([7u8; 20]),
            node_pop: ProofOfPossession::new([1u8; 48], [2u8; 96]),
            network_id: MAINNET_ID,
            upgrades: ava_version::upgrade::get_config(MAINNET_ID),
            tx_fee: 1_000_000,
            create_asset_tx_fee: 10_000_000,
        }
    }

    /// Bundles the handle mocks an `Info` is assembled from.
    #[derive(Default)]
    struct Handles {
        validators: MockValidators,
        chain_manager: MockChainManager,
        vm_manager: MockVmManager,
        network: MockNetwork,
        benchlist: MockBenchlist,
    }

    fn info_with(parameters: Parameters, handles: Handles) -> Arc<Info> {
        let my_ip: SocketAddr = "127.0.0.1:9651".parse().expect("static addr");
        Arc::new(Info::new(
            parameters,
            Arc::new(handles.validators),
            Arc::new(handles.chain_manager),
            Arc::new(handles.vm_manager),
            Arc::new(parking_lot::RwLock::new(my_ip)),
            Arc::new(handles.network),
            Arc::new(handles.benchlist),
        ))
    }

    fn test_info() -> Arc<Info> {
        info_with(test_parameters(), Handles::default())
    }

    fn empty() -> super::types::EmptyArgs {
        super::types::EmptyArgs::default()
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): the registered `info` method set is EXACTLY the 13
    // method names of 14 §3, resolvable under gorilla's first-letter
    // normalization (lowercase client name -> uppercased first letter ->
    // exact-remainder match against the registered Go name).
    // ------------------------------------------------------------------
    #[test]
    fn info_method_set() {
        let mut reg = ServiceRegistry::new();
        test_info().register_rpc(&mut reg);

        // The 13 client-facing names of 14 §3.
        let methods = [
            "getNodeVersion",
            "getNodeID",
            "getNodeIP",
            "getNetworkID",
            "getNetworkName",
            "getBlockchainID",
            "peers",
            "isBootstrapped",
            "upgrades",
            "uptime",
            "acps",
            "getTxFee",
            "getVMs",
        ];
        assert_eq!(reg.len(), methods.len(), "registered info method count");

        for client_name in methods {
            // gorilla shim: uppercase the first letter, match the remainder
            // exactly against the registered Go method name.
            let mut chars = client_name.chars();
            let first = chars.next().expect("non-empty method name");
            let normalized: String = first.to_uppercase().chain(chars).collect();
            assert!(
                reg.lookup("info", &normalized).is_some(),
                "info.{client_name} (normalized {normalized}) must be registered"
            );
        }

        // Acronym guard: the snake_case-derived names must NOT be registered —
        // Go's wire names carry consecutive capitals (`GetNodeID`, `GetVMs`).
        for wrong in [
            "GetNodeId",
            "GetNodeIp",
            "GetNetworkId",
            "GetBlockchainId",
            "GetVms",
        ] {
            assert!(
                reg.lookup("info", wrong).is_none(),
                "snake_case-derived {wrong} must NOT be registered (exact-remainder rule)"
            );
        }
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): getNodeVersion reply field names / json tags mirror Go
    // `GetNodeVersionReply` exactly (14 §3): version, databaseVersion,
    // rpcProtocolVersion (json.Uint32 => STRING), gitCommit, vmVersions.
    // ------------------------------------------------------------------
    #[test]
    fn get_node_version_reply_shape() {
        let reply = GetNodeVersionReply {
            version: "avalanchego/1.14.2".to_string(),
            database_version: "v1.4.5".to_string(),
            rpc_protocol_version: 45,
            git_commit: "de4da4de".to_string(),
            vm_versions: BTreeMap::from([
                ("avm".to_string(), "v1.14.2".to_string()),
                ("platform".to_string(), "v1.14.2".to_string()),
            ]),
        };
        let value = serde_json::to_value(&reply).expect("serialize GetNodeVersionReply");
        assert_eq!(
            value,
            json!({
                "version": "avalanchego/1.14.2",
                "databaseVersion": "v1.4.5",
                // Go json.Uint32 serializes as a quoted decimal string.
                "rpcProtocolVersion": "45",
                "gitCommit": "de4da4de",
                "vmVersions": {
                    "avm": "v1.14.2",
                    "platform": "v1.14.2",
                },
            }),
            "GetNodeVersionReply json shape"
        );
    }

    // ------------------------------------------------------------------
    // Per-method behavior + Go wire-shape parity
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn get_node_version_assembles_from_parameters_and_vm_manager() {
        let handles = Handles {
            vm_manager: MockVmManager {
                versions: BTreeMap::from([("avm".to_string(), "v1.14.2".to_string())]),
                ..MockVmManager::default()
            },
            ..Handles::default()
        };
        let reply = info_with(test_parameters(), handles)
            .get_node_version(empty())
            .await
            .expect("info.getNodeVersion");
        assert_eq!(
            reply.version, "avalanchego/1.14.2",
            "version.Application.String()"
        );
        assert_eq!(reply.database_version, "v1.4.5", "version.CurrentDatabase");
        assert_eq!(reply.rpc_protocol_version, 45, "version.RPCChainVMProtocol");
        assert_eq!(reply.git_commit, "de4da4de", "parameters git commit");
        assert_eq!(
            reply.vm_versions,
            BTreeMap::from([("avm".to_string(), "v1.14.2".to_string())]),
            "vmManager.Versions()"
        );
    }

    #[tokio::test]
    async fn get_node_id_reply_shape() {
        let reply = test_info()
            .get_node_id(empty())
            .await
            .expect("info.getNodeID");
        let value = serde_json::to_value(&reply).expect("serialize GetNodeIdReply");
        assert_eq!(
            value,
            json!({
                "nodeID": NodeId::from([7u8; 20]).to_string(),
                "nodePOP": {
                    // formatting.HexNC: 0x-prefixed hex, NO checksum.
                    "publicKey": format!("0x{}", "01".repeat(48)),
                    "proofOfPossession": format!("0x{}", "02".repeat(96)),
                },
            }),
            "GetNodeIDReply json shape"
        );
    }

    #[tokio::test]
    async fn get_node_ip_network_id_and_name() {
        let info = test_info();

        let ip = serde_json::to_value(info.get_node_ip(empty()).await.expect("info.getNodeIP"))
            .expect("serialize GetNodeIpReply");
        assert_eq!(
            ip,
            json!({ "ip": "127.0.0.1:9651" }),
            "GetNodeIPReply json shape"
        );

        let id = serde_json::to_value(
            info.get_network_id(empty())
                .await
                .expect("info.getNetworkID"),
        )
        .expect("serialize GetNetworkIdReply");
        // json.Uint32 => quoted decimal string.
        assert_eq!(
            id,
            json!({ "networkID": "1" }),
            "GetNetworkIDReply json shape"
        );

        let name = serde_json::to_value(
            info.get_network_name(empty())
                .await
                .expect("info.getNetworkName"),
        )
        .expect("serialize GetNetworkNameReply");
        assert_eq!(
            name,
            json!({ "networkName": "mainnet" }),
            "GetNetworkNameReply json shape"
        );
    }

    #[tokio::test]
    async fn get_blockchain_id_resolves_alias_or_errors() {
        let x_chain = Id::from([3u8; 32]);
        let handles = Handles {
            chain_manager: MockChainManager {
                aliases: BTreeMap::from([("X".to_string(), x_chain)]),
                ..MockChainManager::default()
            },
            ..Handles::default()
        };
        let info = info_with(test_parameters(), handles);

        let reply = info
            .get_blockchain_id(super::types::GetBlockchainIdArgs {
                alias: "X".to_string(),
            })
            .await
            .expect("info.getBlockchainID(X)");
        assert_eq!(reply.blockchain_id, x_chain, "resolved chain ID");

        // Unknown alias: the aliaser error surfaces verbatim as -32000.
        let err = info
            .get_blockchain_id(super::types::GetBlockchainIdArgs {
                alias: "nope".to_string(),
            })
            .await
            .expect_err("info.getBlockchainID(nope) must fail");
        assert_eq!(err.code, json2_code::SERVER, "getBlockchainID error code");
        assert_eq!(
            err.message, "there is no ID with alias: nope",
            "Go aliaser error message"
        );
    }

    fn test_peer(node_id: NodeId) -> PeerInfo {
        PeerInfo {
            ip: "10.0.0.1:9651".parse().expect("static addr"),
            public_ip: None,
            node_id,
            version: "avalanchego/1.14.2".to_string(),
            upgrade_time: 1_607_144_400,
            last_sent: chrono::Utc
                .with_ymd_and_hms(2026, 6, 11, 12, 0, 0)
                .single()
                .expect("static time"),
            last_received: chrono::Utc
                .with_ymd_and_hms(2026, 6, 11, 12, 0, 1)
                .single()
                .expect("static time"),
            observed_uptime: 100,
            tracked_subnets: BTreeSet::new(),
            supported_acps: BTreeSet::new(),
            objected_acps: BTreeSet::new(),
        }
    }

    #[tokio::test]
    async fn peers_reply_shape_and_benched_aliases() {
        let peer_id = NodeId::from([9u8; 20]);
        let benched_chain = Id::from([4u8; 32]);
        let subnet = Id::from([5u8; 32]);
        let mut peer = test_peer(peer_id);
        peer.tracked_subnets = BTreeSet::from([subnet]);
        // Go set.Set marshals sorted by encoded bytes: "103" < "23" < "5".
        peer.supported_acps = BTreeSet::from([23u32, 103u32, 5u32]);

        let handles = Handles {
            network: MockNetwork {
                peers: vec![peer],
                ..MockNetwork::default()
            },
            benchlist: MockBenchlist {
                benched: BTreeMap::from([(peer_id, vec![benched_chain])]),
            },
            chain_manager: MockChainManager {
                primary: BTreeMap::from([(benched_chain, "C".to_string())]),
                ..MockChainManager::default()
            },
            ..Handles::default()
        };
        let reply = info_with(test_parameters(), handles)
            .peers(super::types::PeersArgs::default())
            .await
            .expect("info.peers");
        let value = serde_json::to_value(&reply).expect("serialize PeersReply");
        assert_eq!(
            value,
            json!({
                // json.Uint64 => quoted string.
                "numPeers": "1",
                "peers": [{
                    "ip": "10.0.0.1:9651",
                    // Zero netip.AddrPort marshals as "".
                    "publicIP": "",
                    "nodeID": peer_id.to_string(),
                    "version": "avalanchego/1.14.2",
                    // Plain Go uint64 => JSON number.
                    "upgradeTime": 1_607_144_400u64,
                    "lastSent": "2026-06-11T12:00:00Z",
                    "lastReceived": "2026-06-11T12:00:01Z",
                    // json.Uint32 => quoted string.
                    "observedUptime": "100",
                    "trackedSubnets": [subnet.to_string()],
                    // Sorted by marshaled bytes: "103" < "23" < "5".
                    "supportedACPs": [103, 23, 5],
                    "objectedACPs": [],
                    "benched": ["C"],
                }],
            }),
            "PeersReply json shape"
        );
    }

    #[tokio::test]
    async fn peers_primary_alias_failure_uses_go_message() {
        let peer_id = NodeId::from([9u8; 20]);
        let benched_chain = Id::from([4u8; 32]);
        let handles = Handles {
            network: MockNetwork {
                peers: vec![test_peer(peer_id)],
                ..MockNetwork::default()
            },
            benchlist: MockBenchlist {
                benched: BTreeMap::from([(peer_id, vec![benched_chain])]),
            },
            // No primary alias registered for the benched chain.
            ..Handles::default()
        };
        let err = info_with(test_parameters(), handles)
            .peers(super::types::PeersArgs::default())
            .await
            .expect_err("info.peers must fail on missing primary alias");
        assert_eq!(err.code, json2_code::SERVER, "peers error code");
        assert_eq!(
            err.message,
            format!(
                "failed to get primary alias for chain ID {benched_chain}: \
                 there is no alias for ID: {benched_chain}"
            ),
            "Go peers error message"
        );
    }

    #[tokio::test]
    async fn is_bootstrapped_errors_and_success() {
        let chain = Id::from([6u8; 32]);
        let handles = Handles {
            chain_manager: MockChainManager {
                aliases: BTreeMap::from([("P".to_string(), chain)]),
                bootstrapped: BTreeSet::from([chain]),
                ..MockChainManager::default()
            },
            ..Handles::default()
        };
        let info = info_with(test_parameters(), handles);

        // Missing `chain` argument.
        let err = info
            .is_bootstrapped(super::types::IsBootstrappedArgs::default())
            .await
            .expect_err("info.isBootstrapped({}) must fail");
        assert_eq!(
            err.message, "argument 'chain' not given",
            "errNoChainProvided"
        );

        // Unknown chain: the lookup error is REPLACED with Go's message.
        let err = info
            .is_bootstrapped(super::types::IsBootstrappedArgs {
                chain: "nope".to_string(),
            })
            .await
            .expect_err("info.isBootstrapped(nope) must fail");
        assert_eq!(
            err.message, "there is no chain with alias/ID 'nope'",
            "Go unknown-chain message"
        );

        let reply = info
            .is_bootstrapped(super::types::IsBootstrappedArgs {
                chain: "P".to_string(),
            })
            .await
            .expect("info.isBootstrapped(P)");
        assert!(reply.is_bootstrapped, "chain P is bootstrapped");
    }

    #[tokio::test]
    async fn upgrades_reply_mirrors_upgrade_config_tags() {
        let reply = test_info().upgrades(empty()).await.expect("info.upgrades");
        let value = serde_json::to_value(&reply).expect("serialize UpgradesReply");

        let keys: Vec<&str> = value
            .as_object()
            .expect("UpgradesReply is an object")
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected = vec![
            "apricotPhase1Time",
            "apricotPhase2Time",
            "apricotPhase3Time",
            "apricotPhase4Time",
            "apricotPhase4MinPChainHeight",
            "apricotPhase5Time",
            "apricotPhasePre6Time",
            "apricotPhase6Time",
            "apricotPhasePost6Time",
            "banffTime",
            "cortinaTime",
            "cortinaXChainStopVertexID",
            "durangoTime",
            "etnaTime",
            "fortunaTime",
            "graniteTime",
            "graniteEpochDuration",
            "heliconTime",
        ];
        expected.sort_unstable();
        assert_eq!(keys, expected, "upgrade.Config json tag set");

        // Spot-check Go-parity values for the mainnet schedule.
        assert_eq!(
            value["apricotPhase1Time"],
            json!("2021-03-31T14:00:00Z"),
            "mainnet ApricotPhase1Time RFC3339"
        );
        assert_eq!(
            value["apricotPhase4MinPChainHeight"],
            json!(793_005),
            "plain JSON number (not json.Uint64)"
        );
        assert_eq!(
            value["cortinaXChainStopVertexID"],
            json!("jrGWDh5Po9FMj54depyunNixpia5PN4aAYxfmNzU8n752Rjga"),
            "mainnet CortinaXChainStopVertexID CB58"
        );
        assert!(
            value["graniteEpochDuration"].is_number(),
            "time.Duration marshals as a JSON number of nanoseconds"
        );
    }

    #[tokio::test]
    async fn uptime_formats_four_decimal_strings() {
        let handles = Handles {
            network: MockNetwork {
                uptime: Ok(UptimeResult {
                    rewarding_stake_percentage: 91.5,
                    weighted_average_percentage: 98.123_456,
                }),
                ..MockNetwork::default()
            },
            ..Handles::default()
        };
        let reply = info_with(test_parameters(), handles)
            .uptime(empty())
            .await
            .expect("info.uptime");
        let value = serde_json::to_value(reply).expect("serialize UptimeReply");
        // json.Float64: strconv.FormatFloat(f, 'f', 4, 64) => quoted strings.
        assert_eq!(
            value,
            json!({
                "rewardingStakePercentage": "91.5000",
                "weightedAveragePercentage": "98.1235",
            }),
            "UptimeResponse json shape"
        );

        let handles = Handles {
            network: MockNetwork {
                uptime: Err("boom".to_string()),
                ..MockNetwork::default()
            },
            ..Handles::default()
        };
        let err = info_with(test_parameters(), handles)
            .uptime(empty())
            .await
            .expect_err("info.uptime must fail");
        assert_eq!(
            err.message, "couldn't get node uptime: boom",
            "Go uptime error message"
        );
    }

    #[tokio::test]
    async fn acps_tallies_weighted_support_and_objections() {
        let validator_a = NodeId::from([1u8; 20]);
        let validator_c = NodeId::from([3u8; 20]);
        let non_validator = NodeId::from([2u8; 20]);

        let mut peer_a = test_peer(validator_a);
        peer_a.supported_acps = BTreeSet::from([23u32]);
        peer_a.objected_acps = BTreeSet::from([77u32]);
        let mut peer_b = test_peer(non_validator);
        peer_b.supported_acps = BTreeSet::from([23u32]); // weight 0 => skipped
        let mut peer_c = test_peer(validator_c);
        peer_c.supported_acps = BTreeSet::from([23u32]);

        let handles = Handles {
            network: MockNetwork {
                peers: vec![peer_a, peer_b, peer_c],
                ..MockNetwork::default()
            },
            validators: MockValidators {
                weights: BTreeMap::from([(validator_a, 10), (validator_c, 5)]),
                total: 100,
            },
            ..Handles::default()
        };
        let reply = info_with(test_parameters(), handles)
            .acps(empty())
            .await
            .expect("info.acps");
        let value = serde_json::to_value(&reply).expect("serialize ACPsReply");

        // CurrentACPs is empty at the pinned upstream, so abstainWeight stays 0
        // for ACPs only seen from peers (Go only fills it for CurrentACPs).
        let mut supporters = vec![validator_a.to_string(), validator_c.to_string()];
        supporters.sort();
        assert_eq!(
            value,
            json!({
                "acps": {
                    "23": {
                        "supportWeight": "15",
                        "supporters": supporters,
                        "objectWeight": "0",
                        "objectors": [],
                        "abstainWeight": "0",
                    },
                    "77": {
                        "supportWeight": "0",
                        "supporters": [],
                        "objectWeight": "10",
                        "objectors": [validator_a.to_string()],
                        "abstainWeight": "0",
                    },
                },
            }),
            "ACPsReply json shape"
        );
    }

    #[tokio::test]
    async fn get_tx_fee_static_per_network_tables() {
        // Mainnet table + the two Parameters-driven fields.
        let value = serde_json::to_value(
            test_info()
                .get_tx_fee(empty())
                .await
                .expect("info.getTxFee mainnet"),
        )
        .expect("serialize GetTxFeeResponse");
        assert_eq!(
            value,
            json!({
                "txFee": "1000000",
                "createAssetTxFee": "10000000",
                "createSubnetTxFee": "1000000000",
                "transformSubnetTxFee": "10000000000",
                "createBlockchainTxFee": "1000000000",
                "addPrimaryNetworkValidatorFee": "0",
                "addPrimaryNetworkDelegatorFee": "0",
                "addSubnetValidatorFee": "1000000",
                "addSubnetDelegatorFee": "1000000",
            }),
            "mainnet GetTxFeeResponse"
        );

        // Fuji.
        let mut parameters = test_parameters();
        parameters.network_id = FUJI_ID;
        let fuji = info_with(parameters, Handles::default())
            .get_tx_fee(empty())
            .await
            .expect("info.getTxFee fuji");
        assert_eq!(
            fuji.create_subnet_tx_fee, 100_000_000,
            "fuji createSubnetTxFee"
        );
        assert_eq!(
            fuji.transform_subnet_tx_fee, 1_000_000_000,
            "fuji transformSubnetTxFee"
        );

        // Default (any other network).
        let mut parameters = test_parameters();
        parameters.network_id = 1337;
        let default = info_with(parameters, Handles::default())
            .get_tx_fee(empty())
            .await
            .expect("info.getTxFee default");
        assert_eq!(
            default.transform_subnet_tx_fee, 100_000_000,
            "default transformSubnetTxFee"
        );
        assert_eq!(
            default.create_blockchain_tx_fee, 100_000_000,
            "default createBlockchainTxFee"
        );
    }

    #[tokio::test]
    async fn get_vms_filters_redundant_alias_and_lists_fxs() {
        let avm_id = Id::from([8u8; 32]);
        let handles = Handles {
            vm_manager: MockVmManager {
                factories: vec![avm_id],
                // Go ids.GetRelevantAliases drops the alias == id.String().
                aliases: BTreeMap::from([(avm_id, vec![avm_id.to_string(), "avm".to_string()])]),
                ..MockVmManager::default()
            },
            ..Handles::default()
        };
        let reply = info_with(test_parameters(), handles)
            .get_vms(empty())
            .await
            .expect("info.getVMs");
        assert_eq!(
            reply.vms,
            BTreeMap::from([(avm_id.to_string(), vec!["avm".to_string()])]),
            "redundant id-string alias filtered"
        );
        // The fx IDs are Go's zero-padded ASCII ids.ID constants — golden CB58
        // strings from api/info/service.md.
        assert_eq!(
            reply.fxs,
            BTreeMap::from([
                (
                    "spdxUxVJQbX85MGxMHbKw1sHxMnSqJ3QBzDyDYEP3h6TLuxqQ".to_string(),
                    "secp256k1fx".to_string()
                ),
                (
                    "qd2U4HDWUvMrVUeTcCHp6xH3Qpnn1XbU5MDdnBoiifFqvgXwT".to_string(),
                    "nftfx".to_string()
                ),
                (
                    "rXJsCSEYXg2TehWxCEEGj6JU2PWKTkd6cBdNLjoe2SpsKD9cy".to_string(),
                    "propertyfx".to_string()
                ),
            ]),
            "built-in fx IDs and names"
        );
    }

    // End-to-end through the registry: the boxed handler deserializes
    // params[0] and serializes the reply (macro plumbing).
    #[tokio::test]
    async fn dispatch_through_registry() {
        let mut reg = ServiceRegistry::new();
        test_info().register_rpc(&mut reg);
        let handler = reg
            .lookup("info", "GetNetworkName")
            .expect("info.GetNetworkName registered");
        let result = handler(serde_json::Value::Null)
            .await
            .expect("info.getNetworkName via registry");
        assert_eq!(
            result,
            json!({ "networkName": "mainnet" }),
            "registry handler result"
        );
    }
}
