// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init steps 18 and 25 (specs/12 §2.2): the health service + its application
//! checks (mirror Go `initHealthAPI` — **before** the chain manager) and the
//! worker start + profiler (mirror Go `health.Start` / `initProfiler`).

use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::json;

use ava_api::health::{APPLICATION_TAG, CheckError, CheckResult, Checker, Health};
use ava_api::server::ApiServer;
use ava_config::node::Config;
use ava_database::DynDatabase;
use ava_engine::networking::router::Router as EngineRouter;
use ava_network::network::NetworkImpl;
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::node_id::NodeId;
use ava_validators::ValidatorManager;

use crate::error::Result;
use crate::init::ShutdownTrigger;
use crate::init::metrics::NodeMetrics;
use crate::init::resource::SystemResourceManager;

/// The `network` health check: delegates to the network's own health view.
struct NetworkChecker(Arc<NetworkImpl>);

impl Checker for NetworkChecker {
    fn health_check(&self) -> BoxFuture<'static, CheckResult> {
        let net = Arc::clone(&self.0);
        async move {
            let peers = net.connected_peers();
            Ok(json!({ "connectedPeers": peers.len() }))
        }
        .boxed()
    }
}

/// The `router` health check (Go registers `n.chainRouter`).
struct RouterChecker(Arc<dyn EngineRouter>);

impl Checker for RouterChecker {
    fn health_check(&self) -> BoxFuture<'static, CheckResult> {
        let healthy = self.0.health_check();
        async move {
            if healthy {
                Ok(json!({}))
            } else {
                Err(CheckError::new("router unhealthy"))
            }
        }
        .boxed()
    }
}

/// The `database` health check (Go registers `n.DB`).
struct DatabaseChecker(Arc<dyn DynDatabase>);

impl Checker for DatabaseChecker {
    fn health_check(&self) -> BoxFuture<'static, CheckResult> {
        let result = self
            .0
            .health_check()
            .map_err(|e| CheckError::new(e.to_string()));
        async move { result }.boxed()
    }
}

/// The `diskspace` health check: unhealthy below the warning threshold,
/// node-shutdown below the required threshold (mirror Go's closure).
struct DiskSpaceChecker {
    resources: Arc<dyn SystemResourceManager>,
    required_pct: u64,
    warning_pct: u64,
    shutdown: ShutdownTrigger,
}

impl Checker for DiskSpaceChecker {
    fn health_check(&self) -> BoxFuture<'static, CheckResult> {
        let available_bytes = self.resources.available_disk_bytes();
        let available_pct = self.resources.available_disk_percentage();
        let detail = json!({
            "availableDiskBytes": available_bytes,
            "availableDiskPercentage": available_pct,
        });
        let result = if available_pct < self.required_pct {
            tracing::error!(
                available_disk_bytes = available_bytes,
                remaining_disk_percentage = available_pct,
                required_disk_percentage = self.required_pct,
                "low on disk space. Shutting down..."
            );
            (self.shutdown)(1);
            Err(CheckError::new(format!(
                "remaining available disk space percentage ({available_pct}%) is below minimum \
                 required available space percentage ({}%)",
                self.required_pct
            )))
        } else if available_pct < self.warning_pct {
            Err(CheckError::new(format!(
                "remaining available disk space percentage ({available_pct}%) is below warning \
                 threshold available space percentage ({}%)",
                self.warning_pct
            )))
        } else {
            Ok(detail)
        };
        async move { result }.boxed()
    }
}

/// The `bls` health check: the node's BLS key must match its registration in
/// the primary-network validator set.
struct BlsChecker {
    validators: Arc<dyn ValidatorManager>,
    node_id: NodeId,
    public_key: [u8; 48],
}

impl Checker for BlsChecker {
    fn health_check(&self) -> BoxFuture<'static, CheckResult> {
        let result = match self.validators.get_validator(PRIMARY_NETWORK_ID, self.node_id) {
            None => Ok(json!("node is not a validator")),
            Some(vdr) => match vdr.public_key {
                None => Ok(json!("validator doesn't have a BLS key")),
                Some(vdr_pk) => {
                    if vdr_pk.compress() == self.public_key {
                        Ok(json!("node has the correct BLS key"))
                    } else {
                        Err(CheckError::new(format!(
                            "node has BLS key 0x{}, but is registered to the validator set with 0x{}",
                            hex(&self.public_key),
                            hex(&vdr_pk.compress()),
                        )))
                    }
                }
            },
        };
        async move { result }.boxed()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The collaborators of [`init_health_api`].
pub struct HealthDeps<'a> {
    /// The node config (thresholds + API enablement).
    pub config: &'a Config,
    /// The node gatherer (health + upgrade namespaces).
    pub metrics: &'a NodeMetrics,
    /// The API server (route mounting).
    pub api_server: &'a dyn ApiServer,
    /// The P2P network (the `network` check).
    pub net: &'a Arc<NetworkImpl>,
    /// The router bridge whose engine-router slot step 20 fills (the `router`
    /// check reads through it lazily).
    pub router_bridge: &'a Arc<crate::init::networking::RouterBridge>,
    /// The node database (the `database` check).
    pub db: &'a Arc<dyn DynDatabase>,
    /// The system resources seam (the `diskspace` check).
    pub resources: &'a Arc<dyn SystemResourceManager>,
    /// The validators manager (the `bls` check).
    pub validators: &'a Arc<dyn ValidatorManager>,
    /// This node's id.
    pub node_id: NodeId,
    /// This node's compressed BLS public key.
    pub bls_public_key: [u8; 48],
    /// Demanded by the disk-space check when below the required threshold.
    pub shutdown: ShutdownTrigger,
}

/// A lazy `router` checker reading the bridge's engine-router slot (filled at
/// step 20; the worker only runs checks after step 25, so the slot is always
/// set by then — Go has the same ordering, registering the not-yet-initialized
/// `chainRouter` here).
struct LazyRouterChecker(Arc<crate::init::networking::RouterBridge>);

impl Checker for LazyRouterChecker {
    fn health_check(&self) -> BoxFuture<'static, CheckResult> {
        match self.0.engine_router() {
            Some(router) => RouterChecker(router).health_check(),
            None => async move { Err(CheckError::new("router not initialized")) }.boxed(),
        }
    }
}

/// Step 18: build the health service (always) and, when the health API is
/// enabled, register the application checks and mount `/ext/health` (mirror Go
/// `initHealthAPI`; **must run before the chain manager**, 12 §3.4).
///
/// The Go `futureupgrade` check is a documented deferral; its `time_until`
/// gauge is registered at `+Inf` for metrics-layout parity
/// (`tests/PORTING.md`).
///
/// # Errors
/// Health construction, check registration, metrics-namespace, and
/// route-mount failures.
pub fn init_health_api(deps: &HealthDeps<'_>) -> Result<Arc<Health>> {
    let health_registry = ava_api::metrics::make_and_register(
        deps.metrics.gatherer.as_ref(),
        &crate::init::namespace::health(),
    )?;
    let health = Arc::new(Health::new(&health_registry)?);

    if !deps.config.http_config.api_config.health_api_enabled {
        tracing::info!("skipping health API initialization because it has been disabled");
        return Ok(health);
    }

    tracing::info!("initializing Health API");

    health.register_health_check(
        "network",
        Arc::new(NetworkChecker(Arc::clone(deps.net))),
        &[APPLICATION_TAG.to_owned()],
    )?;
    health.register_health_check(
        "router",
        Arc::new(LazyRouterChecker(Arc::clone(deps.router_bridge))),
        &[APPLICATION_TAG.to_owned()],
    )?;
    health.register_health_check(
        "database",
        Arc::new(DatabaseChecker(Arc::clone(deps.db))),
        &[APPLICATION_TAG.to_owned()],
    )?;
    health.register_health_check(
        "diskspace",
        Arc::new(DiskSpaceChecker {
            resources: Arc::clone(deps.resources),
            required_pct: deps.config.required_available_disk_space_percentage,
            warning_pct: deps.config.warning_available_disk_space_percentage,
            shutdown: Arc::clone(&deps.shutdown),
        }),
        &[APPLICATION_TAG.to_owned()],
    )?;
    health.register_health_check(
        "bls",
        Arc::new(BlsChecker {
            validators: Arc::clone(deps.validators),
            node_id: deps.node_id,
            public_key: deps.bls_public_key,
        }),
        &[APPLICATION_TAG.to_owned()],
    )?;

    // Go's `futureupgrade` check + `time_until` gauge: the gauge is kept at
    // +Inf for layout parity; the mode-upgrade-time check is deferred.
    let upgrade_registry = ava_api::metrics::make_and_register(
        deps.metrics.gatherer.as_ref(),
        &crate::init::namespace::upgrade(),
    )?;
    if let Ok(gauge) = prometheus::Gauge::new(
        "time_until",
        "Time until an upcoming network upgrade (ns). +Inf means the upgrade is unscheduled.",
    ) {
        gauge.set(f64::INFINITY);
        let _ = upgrade_registry.register(Box::new(gauge));
    }

    deps.api_server.add_route(
        ava_api::health::handler::handler(Arc::clone(&health)),
        "health",
        "",
    )?;
    Ok(health)
}

/// Step 25: start the health worker at `--health-check-frequency` and the
/// continuous profiler (mirror Go `health.Start` + `initProfiler`).
///
/// The continuous profiler is a documented deferral (`tests/PORTING.md`): the
/// Rust node has the on-demand admin profiler (M8.19) but no `continuous/`
/// dispatcher yet.
pub fn start_health_and_profiler(health: &Health, config: &Config) {
    health.start(config.health_check_freq);
    if config.profiler_config.enabled {
        tracing::warn!(
            dir = %config.profiler_config.dir,
            "continuous profiling is configured but not supported yet (tests/PORTING.md)"
        );
    } else {
        tracing::info!("skipping profiler initialization because it has been disabled");
    }
}
