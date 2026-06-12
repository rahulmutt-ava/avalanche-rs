// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init steps 22 and 24 (specs/12 §2.2): the admin + info JSON-RPC services
//! (mirror Go `initAdminAPI` / `initInfoAPI`) and the indexer (mirror Go
//! `initIndexer`).
//!
//! The admin/info services consume **narrow trait seams** declared in
//! `ava-api` (M8.18/M8.19); this module provides the live adapters over the
//! assembly-stage node objects. Divergences from the Go handles are documented
//! in `crates/ava-node/tests/PORTING.md`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;

use ava_api::admin::{Admin, AdminConfig, SeamError, VmReload};
use ava_api::jsonrpc::ServiceRegistry;
use ava_api::server::{ApiServer, Server};
use ava_chains::aliaser::AliaserReader;
use ava_chains::manager::VmManager;
use ava_chains::registry::VmRegistry;
use ava_config::node::Config;
use ava_database::{DynDatabase, PrefixDb};
use ava_indexer::{ContainerIndexer, Indexer, PathAdder};
use ava_logging::AvaLevel;
use ava_network::network::{Network, NetworkImpl};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::RealClock;
use ava_validators::ValidatorManager;

use crate::error::Result;
use crate::init::ShutdownTrigger;
use crate::init::chain_manager::AssemblyChainManager;
use crate::init::database::{DynDb, INDEXER_DB_PREFIX};
use crate::init::dispatchers::EventDispatchers;
use crate::init::identity::ProofOfPossession;
use crate::logging::LogFactory;

// ---------------------------------------------------------------------------
// Admin seam adapters (ava_api::admin)
// ---------------------------------------------------------------------------

/// [`LogFactory`] as the admin `logging.Factory` level surface.
struct FactoryLoggerLevels(Arc<LogFactory>);

/// The byte-stable "unknown logger" seam error.
fn unknown_logger(name: &str) -> SeamError {
    format!("logger with name {name} not found").into()
}

impl ava_api::admin::LoggerLevels for FactoryLoggerLevels {
    fn logger_names(&self) -> Vec<String> {
        self.0.logger_names()
    }

    fn log_level(&self, name: &str) -> std::result::Result<AvaLevel, SeamError> {
        let handle = self.0.log_handle(name).ok_or_else(|| unknown_logger(name))?;
        Ok(handle.level())
    }

    fn display_level(&self, name: &str) -> std::result::Result<AvaLevel, SeamError> {
        // The display core is shared by every logger (18 §5 divergence;
        // PORTING.md) — but an unknown name is still an error, like Go.
        if self.0.log_handle(name).is_none() {
            return Err(unknown_logger(name));
        }
        Ok(self.0.display_handle().level())
    }

    fn set_log_level(&self, name: &str, level: AvaLevel) -> std::result::Result<(), SeamError> {
        let handle = self.0.log_handle(name).ok_or_else(|| unknown_logger(name))?;
        handle.set_level(level).map_err(|e| SeamError::from(e.to_string()))
    }

    fn set_display_level(&self, name: &str, level: AvaLevel) -> std::result::Result<(), SeamError> {
        if self.0.log_handle(name).is_none() {
            return Err(unknown_logger(name));
        }
        self.0
            .display_handle()
            .set_level(level)
            .map_err(|e| SeamError::from(e.to_string()))
    }
}

/// [`AssemblyChainManager`] as the admin `chains.Manager` alias surface.
struct AdminChainAliaser(Arc<AssemblyChainManager>);

impl ava_api::admin::ChainAliaser for AdminChainAliaser {
    fn lookup(&self, alias: &str) -> std::result::Result<Id, SeamError> {
        self.0
            .lookup(alias)
            .map_err(|e| SeamError::from(e.to_string()))
    }

    fn alias(&self, chain_id: Id, alias: &str) -> std::result::Result<(), SeamError> {
        self.0
            .alias(chain_id, alias)
            .map_err(|e| SeamError::from(e.to_string()))
    }

    fn aliases(&self, chain_id: Id) -> std::result::Result<Vec<String>, SeamError> {
        Ok(self.0.aliases(chain_id))
    }
}

/// The chains [`VmRegistry`] + [`VmManager`] as the admin `loadVMs` surface
/// (Go `VMRegistry.Reload` + `ids.GetRelevantAliases`).
struct AdminVmRegistry {
    registry: Arc<VmRegistry>,
    manager: Arc<VmManager>,
    token: CancellationToken,
}

#[async_trait::async_trait]
impl ava_api::admin::VmRegistry for AdminVmRegistry {
    async fn reload(&self) -> std::result::Result<VmReload, SeamError> {
        let (installed, failed) = self
            .registry
            .reload(&self.token)
            .await
            .map_err(|e| SeamError::from(e.to_string()))?;

        let aliaser = self.manager.aliaser();
        let mut new_vms = BTreeMap::new();
        for vm_id in installed {
            new_vms.insert(vm_id, aliaser.aliases(vm_id));
        }
        let failed_vms = failed
            .into_iter()
            .map(|(vm_id, e)| (vm_id, e.to_string()))
            .collect();
        Ok(VmReload { new_vms, failed_vms })
    }
}

/// The collaborators of [`init_admin_api`].
pub struct AdminDeps {
    /// The node config (enablement, profile dir, providedFlags for
    /// `getConfig`).
    pub config: Arc<Config>,
    /// The logging factory (`setLoggerLevel` / `getLoggerLevel`).
    pub log_factory: Arc<LogFactory>,
    /// The node database (`dbGet`).
    pub db: Arc<dyn DynDatabase>,
    /// The chain manager (`aliasChain` / `getChainAliases`).
    pub chain_manager: Arc<AssemblyChainManager>,
    /// The HTTP server (route mount + `alias` registration).
    pub api_server: Arc<Server>,
    /// The VM registry + manager (`loadVMs`).
    pub vm_registry: Arc<VmRegistry>,
    /// The VM manager (alias resolution of freshly loaded VMs).
    pub vm_manager: Arc<VmManager>,
    /// Cancels a `loadVMs` plugin probe at node shutdown.
    pub token: CancellationToken,
}

/// Step 22a: build + mount `/ext/admin` (mirror Go `initAdminAPI`). Skipped
/// (with an info log) when `--api-admin-enabled=false` — the default.
///
/// Go marshals the whole resolved config for `admin.getConfig`; `ava_config`'s
/// `Config` has no serde yet, so the Rust reply is the providedFlags map
/// (documented deferral, `tests/PORTING.md`).
///
/// # Errors
/// Route-mount failures.
pub fn init_admin_api(deps: &AdminDeps) -> Result<()> {
    if !deps.config.http_config.api_config.admin_api_enabled {
        tracing::info!("skipping admin API initialization because it has been disabled");
        return Ok(());
    }

    tracing::info!("initializing admin API");

    let node_config = serde_json::to_value(&deps.config.provided_flags)
        .unwrap_or(serde_json::Value::Null);

    let admin = Admin::new(AdminConfig {
        profile_dir: PathBuf::from(&deps.config.profiler_config.dir),
        log_levels: Arc::new(FactoryLoggerLevels(Arc::clone(&deps.log_factory))),
        node_config,
        db: Arc::new(DynDb::new(Arc::clone(&deps.db))),
        chain_manager: Arc::new(AdminChainAliaser(Arc::clone(&deps.chain_manager))),
        http_server: Arc::clone(&deps.api_server) as Arc<dyn ava_api::admin::AliasAdder>,
        vm_registry: Arc::new(AdminVmRegistry {
            registry: Arc::clone(&deps.vm_registry),
            manager: Arc::clone(&deps.vm_manager),
            token: deps.token.clone(),
        }),
    });

    deps.api_server
        .add_route(admin.into_handler(), "admin", "")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Info seam adapters (ava_api::info)
// ---------------------------------------------------------------------------

/// [`AssemblyChainManager`] as the info `chains.Manager` slice.
struct InfoChainManager(Arc<AssemblyChainManager>);

impl ava_api::info::ChainManager for InfoChainManager {
    fn lookup(&self, alias: &str) -> std::result::Result<Id, String> {
        self.0.lookup(alias).map_err(|e| e.to_string())
    }

    fn primary_alias(&self, chain_id: Id) -> std::result::Result<String, String> {
        self.0.primary_alias(chain_id).map_err(|e| e.to_string())
    }

    fn is_bootstrapped(&self, chain_id: Id) -> bool {
        self.0.is_bootstrapped(chain_id)
    }
}

/// The validators manager as the info `validators.Manager` slice.
struct InfoValidators(Arc<dyn ValidatorManager>);

impl ava_api::info::ValidatorSet for InfoValidators {
    fn get_weight(&self, subnet_id: Id, node_id: NodeId) -> u64 {
        self.0.get_weight(subnet_id, node_id)
    }

    fn total_weight(&self, subnet_id: Id) -> std::result::Result<u64, String> {
        self.0.total_weight(subnet_id).map_err(|e| e.to_string())
    }
}

/// The chains [`VmManager`] as the info `vms.Manager` slice.
struct InfoVmManager(Arc<VmManager>);

impl ava_api::info::VmManager for InfoVmManager {
    fn versions(&self) -> std::result::Result<BTreeMap<String, String>, String> {
        Ok(self.0.versions().into_iter().collect())
    }

    fn list_factories(&self) -> std::result::Result<Vec<Id>, String> {
        Ok(self.0.list_factories())
    }

    fn aliases(&self, vm_id: Id) -> Vec<String> {
        self.0.aliaser().aliases(vm_id)
    }
}

/// The P2P network as the info `network.Network` slice. The Rust peer info is
/// the trimmed M2 surface (node id / ip / version); the remaining Go fields
/// are zero-valued until the network exposes them (`tests/PORTING.md`).
struct InfoNet(Arc<NetworkImpl>);

impl ava_api::info::InfoNetwork for InfoNet {
    fn peer_info(&self, node_ids: &[NodeId]) -> Vec<ava_api::info::types::PeerInfo> {
        Network::peer_info(self.0.as_ref(), node_ids)
            .into_iter()
            .map(|p| ava_api::info::types::PeerInfo {
                ip: p.ip,
                public_ip: None,
                node_id: p.node_id,
                version: p.version,
                upgrade_time: 0,
                last_sent: chrono::DateTime::UNIX_EPOCH,
                last_received: chrono::DateTime::UNIX_EPOCH,
                observed_uptime: 0,
                tracked_subnets: std::collections::BTreeSet::new(),
                supported_acps: std::collections::BTreeSet::new(),
                objected_acps: std::collections::BTreeSet::new(),
            })
            .collect()
    }

    fn node_uptime(
        &self,
    ) -> std::result::Result<ava_api::info::types::UptimeResult, String> {
        let uptime = Network::node_uptime(self.0.as_ref()).map_err(|e| e.to_string())?;
        Ok(ava_api::info::types::UptimeResult {
            rewarding_stake_percentage: uptime.rewarding_stake_percentage,
            weighted_average_percentage: uptime.weighted_average_percentage,
        })
    }
}

/// The engine benchlist as the info `benchlist.Manager` slice. The M3
/// benchlist has no per-chain bench registry yet, so `getBenched` reports no
/// benched chains (`tests/PORTING.md`).
struct InfoBenchlist;

impl ava_api::info::Benchlist for InfoBenchlist {
    fn get_benched(&self, _node_id: NodeId) -> Vec<Id> {
        Vec::new()
    }
}

/// The collaborators of [`init_info_api`].
pub struct InfoDeps {
    /// The node config (enablement, network id, upgrade schedule, fees).
    pub config: Arc<Config>,
    /// This node's id.
    pub node_id: NodeId,
    /// This node's BLS proof of possession.
    pub pop: ProofOfPossession,
    /// The validators manager (`info.uptime` weights).
    pub validators: Arc<dyn ValidatorManager>,
    /// The chain manager (alias + bootstrapped surface).
    pub chain_manager: Arc<AssemblyChainManager>,
    /// The VM manager (`info.getVMs`).
    pub vm_manager: Arc<VmManager>,
    /// The advertised public IP (`info.getNodeIP`).
    pub my_ip: Arc<RwLock<SocketAddr>>,
    /// The P2P network (`info.peers` / `info.uptime`).
    pub net: Arc<NetworkImpl>,
    /// The API server (route mount).
    pub api_server: Arc<Server>,
}

/// Step 22b: build + mount `/ext/info` (mirror Go `initInfoAPI`). Skipped
/// (with an info log) when `--api-info-enabled=false`.
///
/// # Errors
/// Route-mount failures.
pub fn init_info_api(deps: &InfoDeps) -> Result<()> {
    if !deps.config.http_config.api_config.info_api_enabled {
        tracing::info!("skipping info API initialization because it has been disabled");
        return Ok(());
    }

    tracing::info!("initializing info API");

    let parameters = ava_api::info::Parameters {
        version: ava_version::CURRENT.clone(),
        // The build-time commit global lands with the avalanchers bin wiring
        // (Go `version.GitCommit`; PORTING.md).
        git_commit: String::new(),
        node_id: deps.node_id,
        node_pop: ava_api::info::types::ProofOfPossession {
            public_key: deps.pop.public_key,
            proof_of_possession: deps.pop.proof_of_possession,
        },
        network_id: deps.config.network_id,
        upgrades: deps.config.upgrade_config.clone(),
        tx_fee: deps.config.tx_fee_config.tx_fee,
        create_asset_tx_fee: deps.config.tx_fee_config.create_asset_tx_fee,
    };

    let info = Arc::new(ava_api::info::Info::new(
        parameters,
        Arc::new(InfoValidators(Arc::clone(&deps.validators))),
        Arc::new(InfoChainManager(Arc::clone(&deps.chain_manager))),
        Arc::new(InfoVmManager(Arc::clone(&deps.vm_manager))),
        Arc::clone(&deps.my_ip),
        Arc::new(InfoNet(Arc::clone(&deps.net))),
        Arc::new(InfoBenchlist),
    ));

    let mut registry = ServiceRegistry::new();
    info.register_rpc(&mut registry);
    let handler = Router::new()
        .route("/", axum::routing::any(ava_api::jsonrpc::dispatch))
        .with_state(Arc::new(registry));

    deps.api_server.add_route(handler, "info", "")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Indexer (step 24)
// ---------------------------------------------------------------------------

/// The [`Server`] as the indexer's route-mounting seam (Go
/// `server.PathAdder`).
struct ServerPathAdder(Arc<Server>);

impl PathAdder for ServerPathAdder {
    fn add_route(
        &self,
        handler: ava_api::BoxedHandler,
        base: &str,
        endpoint: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        ApiServer::add_route(self.0.as_ref(), handler, base, endpoint)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }
}

/// The node DB type the indexer runs over: the `0x00`-prefixed view of the
/// dynamically-chosen backend.
pub type IndexerDb = PrefixDb<DynDb>;

/// Step 24: build the indexer over the `0x00`-prefixed node DB and register it
/// as a chain-manager registrant (mirror Go `initIndexer`).
///
/// Go's `ShutdownF` calls `n.Shutdown(0)` (its `// TODO put exit code here` is
/// preserved): a fatal indexer close demands a clean-exit shutdown.
///
/// # Errors
/// [`crate::error::Error::Indexer`] when the indexer database is unusable.
pub fn init_indexer(
    config: &Config,
    db: &Arc<dyn DynDatabase>,
    api_server: &Arc<Server>,
    dispatchers: &EventDispatchers,
    chain_manager: &AssemblyChainManager,
    shutdown: ShutdownTrigger,
) -> Result<Arc<dyn Indexer>> {
    let prefixed: Arc<IndexerDb> =
        Arc::new(PrefixDb::new(INDEXER_DB_PREFIX, DynDb::new(Arc::clone(db))));

    let indexer = ContainerIndexer::new(ava_indexer::Config {
        db: prefixed,
        indexing_enabled: config.http_config.api_config.index_api_enabled,
        allow_incomplete_index: config.http_config.api_config.index_allow_incomplete,
        block_acceptor_group: Arc::clone(&dispatchers.block),
        tx_acceptor_group: Arc::clone(&dispatchers.tx),
        vertex_acceptor_group: Arc::clone(&dispatchers.vertex),
        path_adder: Arc::new(ServerPathAdder(Arc::clone(api_server))),
        shutdown_f: Arc::new(move || shutdown(0)),
        clock: Arc::new(RealClock),
    })?;
    let indexer: Arc<dyn Indexer> = Arc::new(indexer);

    // The chain manager notifies the indexer when a chain is created.
    chain_manager.add_registrant(Arc::clone(&indexer));

    Ok(indexer)
}
