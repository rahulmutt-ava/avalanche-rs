// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The assembled node: [`Node`] + [`Node::new`] (specs/12 §2.1/§2.2, 17
//! §1/§2/§4; mirror Go `node/node.go::New`).
//!
//! `Node::new` runs the 26 init steps in the exact Go order (the comments in
//! Go's `New` are load-bearing: the message creator must follow metrics and
//! precede networking; the health API must precede the chain manager). The
//! order is pinned by `tests::init_order_matches_go`.
//!
//! Dispatch and the 14-step shutdown (M8.30) consume the handles assembled
//! here; this task only guarantees the fields + the cancellation-token tree
//! (root → network → peer, root → subnet → chain; 17 §4.1) exist.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use ava_api::health::Health;
use ava_api::server::Server;
use ava_chains::atomic::Memory;
use ava_chains::manager::VmManager;
use ava_chains::registry::VmRegistry;
use ava_config::node::Config;
use ava_crypto::bls::Signer;
use ava_database::DynDatabase;
use ava_engine::networking::benchlist::Benchlist;
use ava_engine::networking::router::ChainRouter;
use ava_engine::networking::timeout::AdaptiveTimeoutManager;
use ava_indexer::Indexer;
use ava_message::builder::Creator;
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::ValidatorManager;

use crate::error::Result;
use crate::init::api_services::{AdminDeps, InfoDeps};
use crate::init::chain_manager::AssemblyChainManager;
use crate::init::dispatchers::EventDispatchers;
use crate::init::health::HealthDeps;
use crate::init::identity::ProofOfPossession;
use crate::init::metrics::NodeMetrics;
use crate::init::nat::Nat;
use crate::init::networking::Networking;
use crate::init::resource::Resources;
use crate::init::vms::RuntimeManager;
use crate::init::{self, ShutdownTrigger};
use crate::logging::LogFactory;
use crate::trace::Tracer;

/// The assembled Avalanche node (Go `node.Node`; specs/12 §2.2 struct).
///
/// Handles are exposed `pub` because the node is itself the wiring layer:
/// dispatch/shutdown (M8.30), the process-context writer, and the bin all read
/// them. Nothing here is API-stable.
pub struct Node {
    /// This node's ID, derived from the staking certificate (step 1).
    pub id: NodeId,
    /// The resolved node configuration.
    pub config: Arc<Config>,
    /// The logging factory (admin `setLoggerLevel` surface; Go `LogFactory`).
    pub log_factory: Arc<LogFactory>,
    /// The BLS staking signer (step 2; Go `StakingSigner`).
    pub staking_signer: Arc<dyn Signer>,
    /// The BLS proof of possession (step 2; Go `signer.ProofOfPossession`).
    pub pop: ProofOfPossession,
    /// The VM manager + aliaser (step 4; Go `VMManager` / `VMAliaser`).
    pub vm_manager: Arc<VmManager>,
    /// The bootstrap beacons (step 5; Go `bootstrappers`).
    pub bootstrappers: Arc<dyn ValidatorManager>,
    /// The OpenTelemetry tracer (step 6; Go `tracer`).
    pub tracer: Tracer,
    /// The node metric gatherers (step 7; Go `MetricsGatherer` +
    /// `MeterDBMetricsGatherer`).
    pub metrics: NodeMetrics,
    /// The NAT router + port mapper (step 8; Go `router` / `portMapper`).
    pub nat: Nat,
    /// The HTTP API server (step 9; Go `APIServer`).
    pub api_server: Arc<Server>,
    /// The advertised API URI (process.json `uri`; Go `apiURI`).
    pub api_uri: String,
    /// The node database (step 11; Go `DB`).
    pub db: Arc<dyn DynDatabase>,
    /// Cross-chain shared memory (step 12; Go `sharedMemory`).
    pub shared_memory: Arc<Memory>,
    /// The message creator (step 13; Go `msgCreator`).
    pub msg_creator: Arc<Creator>,
    /// The validators manager (step 14; Go `vdrs`).
    pub validators: Arc<dyn ValidatorManager>,
    /// Resource manager + CPU/disk trackers and targeters (step 15).
    pub resources: Resources,
    /// The P2P networking handles (step 16; Go `Net` + `onSufficientlyConnected`
    /// + the dynamic-IP updater).
    pub networking: Networking,
    /// The Block/Tx/Vertex acceptor groups (step 17).
    pub dispatchers: EventDispatchers,
    /// The health service (step 18; worker started at step 25).
    pub health: Arc<Health>,
    /// The chain manager (step 20; Go `chainManager`).
    pub chain_manager: Arc<AssemblyChainManager>,
    /// The engine chain router (step 20; Go `chainRouter`).
    pub chain_router: Arc<ChainRouter>,
    /// The adaptive timeout manager (step 20; Go `timeoutManager`).
    pub timeout_manager: Arc<AdaptiveTimeoutManager>,
    /// The benchlist (step 20; Go `benchlistManager`).
    pub benchlist: Arc<Benchlist>,
    /// The X-Chain ID derived from genesis (step 20).
    pub x_chain_id: Id,
    /// The C-Chain ID derived from genesis (step 20).
    pub c_chain_id: Id,
    /// The VM registry (step 21; Go `VMRegistry`).
    pub vm_registry: Arc<VmRegistry>,
    /// The rpcchainvm plugin runtime manager seam (step 21; Go
    /// `runtimeManager`, stopped at shutdown step 12).
    pub runtime_manager: Arc<dyn RuntimeManager>,
    /// The indexer (step 24; Go `indexer`, closed at shutdown step 11).
    pub indexer: Arc<dyn Indexer>,
    /// The shared runtime handle (17 §1.1: the bin owns the runtime; library
    /// code only ever borrows this). Dispatch (M8.30) spawns onto it.
    pub rt: tokio::runtime::Handle,
    /// The **root** cancellation token (17 §4.1). Cancelled by `shutdown`.
    pub shutdown: CancellationToken,
    /// root → network: the network subtree (peer tokens are its children).
    pub network_token: CancellationToken,
    /// root → subnet: the subnet subtree (chain tokens are its children;
    /// handed to the chain creator when chain creation lands).
    pub subnet_token: CancellationToken,
    /// Tracks every task the node spawns so shutdown (M8.30) can join them.
    pub tasks: TaskTracker,
    /// The exit code recorded by the first shutdown demand (Go `exitCode`).
    exit_code: Arc<AtomicI32>,
    /// Whether a shutdown has been demanded (Go `shuttingDown`).
    shutting_down: Arc<AtomicBool>,
    /// Runs the 14-step shutdown exactly once (M8.30; Go `shuttingDownOnce`).
    pub shutdown_once: tokio::sync::OnceCell<()>,
}

impl Node {
    /// Assemble the node: run init steps 1–26 in the exact Go order
    /// (specs/12 §2.2; Go `node.New`).
    ///
    /// Takes the runtime [`Handle`](tokio::runtime::Handle) instead of
    /// creating a runtime (17 §1.1): the `avalanchers` bin owns the single
    /// multi-thread runtime.
    ///
    /// # Errors
    /// The typed per-step error (see [`crate::error::Error`]); like Go, the
    /// first failing step aborts assembly.
    pub async fn new(
        config: Arc<Config>,
        log_factory: Arc<LogFactory>,
        rt: tokio::runtime::Handle,
    ) -> Result<Arc<Self>> {
        Self::new_recorded(config, log_factory, rt, None).await
    }

    /// [`Node::new`] with an optional init-step recorder (the
    /// `init_order_matches_go` seam: each step pushes its Go init name).
    #[allow(clippy::too_many_lines)] // the 26-step sequence is deliberately one linear fn (Go parity).
    pub(crate) async fn new_recorded(
        config: Arc<Config>,
        log_factory: Arc<LogFactory>,
        rt: tokio::runtime::Handle,
        mut recorder: Option<&mut Vec<&'static str>>,
    ) -> Result<Arc<Self>> {
        let mut record = |name: &'static str| {
            if let Some(r) = recorder.as_deref_mut() {
                r.push(name);
            }
        };

        // The shutdown surface exists before the Node does: the root token of
        // the 17 §4.1 tree plus the trigger handed to subsystems that can
        // demand a shutdown mid-assembly (disk-space health check, indexer
        // fatal close). M8.30 swaps the trigger's tail for the full 14-step
        // sequence.
        let shutdown = CancellationToken::new();
        let network_token = shutdown.child_token();
        let subnet_token = shutdown.child_token();
        let exit_code = Arc::new(AtomicI32::new(0));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let trigger: ShutdownTrigger = {
            let exit_code = Arc::clone(&exit_code);
            let shutting_down = Arc::clone(&shutting_down);
            let root = shutdown.clone();
            Arc::new(move |code: i32| {
                if !shutting_down.swap(true, Ordering::SeqCst) {
                    exit_code.store(code, Ordering::SeqCst);
                }
                root.cancel();
            })
        };

        // 1. Staking TLS certificate → NodeID.
        record("staking_cert");
        let identity = &config.staking_config.identity;
        let id = init::identity::node_id_from_identity(identity)?;

        // 2. BLS staking signer + proof of possession.
        record("staking_signer");
        let staking_signer = init::identity::new_staking_signer(&config.staking_config.signer)?;
        let pop = init::identity::proof_of_possession(staking_signer.as_ref())?;

        // 3. The "initializing node" banner.
        record("log_banner");
        init::identity::log_banner(&config, id, &pop);

        // 4. VMAliaser + VMManager (seeding `--vm-aliases`).
        record("vm_manager");
        let vm_manager = init::vms::init_vm_manager(&config.vm_aliases)?;

        // 5. Bootstrap beacons.
        record("bootstrappers");
        let bootstrappers =
            init::bootstrappers::new_bootstrappers(&config.bootstrap_config.bootstrappers)?;

        // 6. Tracer.
        record("tracer");
        let tracer = crate::trace::new(&config.trace_config)?;

        // 7. Metrics (the prefix multi-gatherer).
        record("metrics");
        let metrics = init::metrics::init_metrics()?;

        // 8. NAT (router probe + port mapper).
        record("nat");
        let nat = init::nat::init_nat(&config.ip_config).await?;

        // 9. API server.
        record("api_server");
        let (api_server, api_uri) =
            init::api_server::init_api_server(&config, id, &metrics, &nat, &shutdown).await?;

        // 10. Metrics API.
        record("metrics_api");
        init::metrics::init_metrics_api(
            config.http_config.api_config.metrics_api_enabled,
            &metrics,
            api_server.as_ref(),
        )?;

        // 11. Database (+ genesis-hash check + ungraceful-shutdown marker).
        record("database");
        let db = init::database::init_database(&config, &metrics)?;

        // 12. Shared memory.
        record("shared_memory");
        let shared_memory = init::database::init_shared_memory(&db);

        // 13. Message creator — after metrics, before networking/chain
        //     manager/engine (shares the `avalanche_network` registerer).
        record("message_creator");
        let (msg_creator, network_registry) =
            init::message::init_message_creator(&config.network_config, &metrics)?;

        // 14. Validators manager (+ overridden manager when sybil protection
        //     is off).
        record("validators");
        let validators = init::validators::new_validators(
            config.staking_config.sybil_protection_enabled,
            PRIMARY_NETWORK_ID,
        );

        // 15. Resource manager + CPU/disk trackers and targeters.
        record("resource_manager");
        let resources = init::resource::init_resource_manager(&config, &metrics, &validators)?;

        // 16. Networking.
        record("networking");
        let networking = init::networking::init_networking(
            &config,
            id,
            identity,
            &staking_signer,
            &msg_creator,
            &network_registry,
            &validators,
            &bootstrappers,
            &nat,
            &network_token,
        )
        .await?;

        // 17. Event dispatchers (Block/Tx/Vertex acceptor groups).
        record("event_dispatchers");
        let dispatchers = init::dispatchers::init_event_dispatchers();

        // 18. Health API — **before** the chain manager (12 §3.4).
        record("health_api");
        let health = init::health::init_health_api(&HealthDeps {
            config: &config,
            metrics: &metrics,
            api_server: api_server.as_ref(),
            net: &networking.net,
            router_bridge: &networking.router_bridge,
            db: &db,
            resources: &resources.manager,
            validators: &validators,
            node_id: id,
            bls_public_key: pop.public_key,
            shutdown: Arc::clone(&trigger),
        })?;

        // 19. Default VM aliases.
        record("default_vm_aliases");
        init::vms::add_default_vm_aliases(&vm_manager)?;

        // 20. Chain manager (timeout manager, chain router, benchlist).
        record("chain_manager");
        let chain_manager_init = init::chain_manager::init_chain_manager(
            &config,
            &metrics,
            &bootstrappers,
            &networking.router_bridge,
            id,
        )?;

        // 21. VMs (registry + plugin runtime manager).
        record("vms");
        let vms = init::vms::init_vms(&metrics, &vm_manager, &shutdown).await?;

        // 22. Admin API, then info API.
        record("admin_api");
        init::api_services::init_admin_api(&AdminDeps {
            config: Arc::clone(&config),
            log_factory: Arc::clone(&log_factory),
            db: Arc::clone(&db),
            chain_manager: Arc::clone(&chain_manager_init.manager),
            api_server: Arc::clone(&api_server),
            vm_registry: Arc::clone(&vms.registry),
            vm_manager: Arc::clone(&vm_manager),
            token: shutdown.clone(),
        })?;
        record("info_api");
        init::api_services::init_info_api(&InfoDeps {
            config: Arc::clone(&config),
            node_id: id,
            pop,
            validators: Arc::clone(&validators),
            chain_manager: Arc::clone(&chain_manager_init.manager),
            vm_manager: Arc::clone(&vm_manager),
            my_ip: Arc::clone(&networking.my_ip),
            net: Arc::clone(&networking.net),
            api_server: Arc::clone(&api_server),
        })?;

        // 23. Chain aliases, then API aliases.
        record("chain_aliases");
        init::aliases::init_chain_aliases(
            &chain_manager_init.manager,
            chain_manager_init.x_chain_id,
            chain_manager_init.c_chain_id,
            &config.chain_aliases,
        )?;
        record("api_aliases");
        init::aliases::init_api_aliases(
            api_server.as_ref(),
            chain_manager_init.x_chain_id,
            chain_manager_init.c_chain_id,
        )?;

        // 24. Indexer.
        record("indexer");
        let indexer = init::api_services::init_indexer(
            &config,
            &db,
            &api_server,
            &dispatchers,
            &chain_manager_init.manager,
            Arc::clone(&trigger),
        )?;

        // 25. Health worker start + profiler.
        record("health_start_profiler");
        init::health::start_health_and_profiler(&health, &config);

        // 26. Chains (queue the platform chain; its genesis creates X and C).
        record("init_chains");
        init::chain_manager::init_chains(&chain_manager_init.manager, &config.genesis_bytes)?;

        Ok(Arc::new(Self {
            id,
            config,
            log_factory,
            staking_signer,
            pop,
            vm_manager,
            bootstrappers,
            tracer,
            metrics,
            nat,
            api_server,
            api_uri,
            db,
            shared_memory,
            msg_creator,
            validators,
            resources,
            networking,
            dispatchers,
            health,
            chain_manager: chain_manager_init.manager,
            chain_router: chain_manager_init.chain_router,
            timeout_manager: chain_manager_init.timeout_manager,
            benchlist: chain_manager_init.benchlist,
            x_chain_id: chain_manager_init.x_chain_id,
            c_chain_id: chain_manager_init.c_chain_id,
            vm_registry: vms.registry,
            runtime_manager: vms.runtime_manager,
            indexer,
            rt,
            shutdown,
            network_token,
            subnet_token,
            tasks: TaskTracker::new(),
            exit_code,
            shutting_down,
            shutdown_once: tokio::sync::OnceCell::new(),
        }))
    }

    /// The exit code recorded by the first shutdown demand.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::SeqCst)
    }

    /// Whether a shutdown has been demanded.
    #[must_use]
    pub fn shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ava_config::flags::{FLAG_SPECS, build_command};
    use ava_config::parse::get_node_config;
    use ava_config::precedence::Layered;

    use super::Node;
    use crate::logging::LogFactory;

    /// A minimal, network-quiet node config: local network, in-memory DB,
    /// ephemeral staking identity, OS-assigned ports, explicit public IP (so
    /// the NAT step never probes the LAN).
    fn test_config(data_dir: &std::path::Path) -> ava_config::node::Config {
        let args: Vec<String> = [
            "avalanchers",
            "--network-id=local",
            "--db-type=memdb",
            "--staking-ephemeral-cert-enabled",
            "--staking-ephemeral-signer-enabled",
            "--http-host=127.0.0.1",
            "--http-port=0",
            "--staking-port=0",
            "--public-ip=127.0.0.1",
            "--api-admin-enabled",
            "--index-enabled",
        ]
        .into_iter()
        .map(String::from)
        .chain([format!("--data-dir={}", data_dir.display())])
        .collect();

        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            args,
            FLAG_SPECS,
            std::iter::empty(),
        )
        .unwrap_or_else(|e| panic!("Layered::build_with_env(): {e}"));
        get_node_config(&layered).unwrap_or_else(|e| panic!("get_node_config(): {e}"))
    }

    /// The Go 26-step init order of `node.go::New` (12 §2.2). Steps 22/23 are
    /// two Go init calls each; every entry is one recorded `init_*`.
    const GO_INIT_ORDER: [&str; 28] = [
        "staking_cert",          // 1
        "staking_signer",        // 2
        "log_banner",            // 3
        "vm_manager",            // 4
        "bootstrappers",         // 5
        "tracer",                // 6
        "metrics",               // 7
        "nat",                   // 8
        "api_server",            // 9
        "metrics_api",           // 10
        "database",              // 11
        "shared_memory",         // 12
        "message_creator",       // 13 — after metrics, before networking
        "validators",            // 14
        "resource_manager",      // 15
        "networking",            // 16
        "event_dispatchers",     // 17
        "health_api",            // 18 — before chain_manager
        "default_vm_aliases",    // 19
        "chain_manager",         // 20
        "vms",                   // 21
        "admin_api",             // 22a
        "info_api",              // 22b
        "chain_aliases",         // 23a
        "api_aliases",           // 23b
        "indexer",               // 24
        "health_start_profiler", // 25
        "init_chains",           // 26
    ];

    #[tokio::test(flavor = "multi_thread")]
    async fn init_order_matches_go() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir(): {e}"));
        let config = Arc::new(test_config(dir.path()));

        let handles = crate::logging::init(&config.logging_config)
            .unwrap_or_else(|e| panic!("logging::init(): {e}"));
        let log_factory = Arc::new(LogFactory::new(config.logging_config.clone(), handles));

        let mut recorded = Vec::new();
        let node = Node::new_recorded(
            Arc::clone(&config),
            log_factory,
            tokio::runtime::Handle::current(),
            Some(&mut recorded),
        )
        .await
        .unwrap_or_else(|e| panic!("Node::new(): {e}"));

        assert_eq!(
            recorded, GO_INIT_ORDER,
            "Node::new() init order must match Go node.New"
        );

        // The token tree exists: cancelling the root reaches both subtrees.
        assert!(!node.shutdown.is_cancelled(), "root token starts live");
        node.shutdown.cancel();
        assert!(
            node.network_token.is_cancelled(),
            "root → network child token"
        );
        assert!(
            node.subnet_token.is_cancelled(),
            "root → subnet child token"
        );

        // Assembly invariants (Go parity).
        assert_eq!(
            node.chain_manager.queued_chains().len(),
            1,
            "init_chains() queues exactly the platform chain"
        );
        assert!(
            node.networking.router_bridge.engine_router().is_some(),
            "init_chain_manager() fills the RouterBridge engine-router slot"
        );
        assert_eq!(node.exit_code(), 0, "no shutdown demanded during init");
        assert!(
            !node.shutting_down(),
            "node is not shutting down after init"
        );
    }
}
