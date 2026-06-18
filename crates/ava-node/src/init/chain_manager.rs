// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init steps 20 and 26 (specs/12 §2.2): the chain manager (mirror Go
//! `initChainManager`) and the platform-chain kickoff (mirror Go
//! `initChains`).
//!
//! **Narrow seam (M8.29, `tests/PORTING.md`):** the full Go `chains.Manager`
//! (queued chain creation through the `create_snowman_chain` pipeline, chain
//! registrant fan-out, router `add_chain`) is not assembled yet — the concrete
//! P/X/C VM factories it would instantiate do not exist as
//! `ava_chains::Factory` impls. [`AssemblyChainManager`] owns what the rest of
//! `Node::new` and the mounted APIs need today: the chain aliaser, the
//! bootstrapped set, the registrant list, and the queued [`ChainParameters`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use ava_chains::aliaser::Aliaser;
use ava_chains::manager::ChainParameters;
use ava_engine::networking::benchlist::{Benchlist, BenchlistConfig as EngineBenchlistConfig};
use ava_engine::networking::router::ChainRouter;
use ava_engine::networking::timeout::AdaptiveTimeoutManager;
use ava_genesis::vm_genesis;
use ava_indexer::Indexer;
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_utils::clock::{Clock, RealClock};
use ava_validators::ValidatorManager;

use crate::error::{Error, Result};
use crate::init::metrics::NodeMetrics;
use crate::init::networking::RouterBridge;

/// A live `is_bootstrapped` reporter for one chain. The chain creator that
/// constructs the running chain registers a closure that reads the engine's
/// shared consensus context (Go `Manager.IsBootstrapped` = a live read of
/// `chain.Context.State.Get() == snow.NormalOp`). The closure is kept opaque so
/// `ava-node` need not depend on `ava-snow` / a concrete VM crate — the
/// chain-creator wiring (in the binary crate, which owns those deps) captures the
/// `Arc<ConsensusContext>` and returns whether it has reached `NormalOp`.
type BootstrappedReporter = Box<dyn Fn() -> bool + Send + Sync>;

/// The platform chain's well-known ID (Go `constants.PlatformChainID`).
pub const PLATFORM_CHAIN_ID: Id = Id::EMPTY;

/// The platform VM's well-known ID (Go `constants.PlatformVMID`).
#[must_use]
pub fn platform_vm_id() -> Id {
    Id::from(ava_genesis::chains::PLATFORM_VM_ID_BYTES)
}

/// The AVM's well-known ID (Go `constants.AVMID`).
#[must_use]
pub fn avm_id() -> Id {
    Id::from(ava_genesis::chains::AVM_ID_BYTES)
}

/// The EVM's well-known ID (Go `constants.EVMID`).
#[must_use]
pub fn evm_id() -> Id {
    Id::from(ava_genesis::chains::EVM_ID_BYTES)
}

/// A running chain's shutdown handles (17 §4.1/§4.4). A chain's `token` is a
/// child of its subnet's token, which is a child of the node's root
/// `subnet_token`; cancelling a subnet token therefore reaches only that
/// subnet's chains (asserted by the M8.30 cancellation-propagation test). The
/// `tasks` tracker collects the chain's async worker pool so shutdown step 5
/// can drain it.
struct ChainHandle {
    /// The chain id (for diagnostics / per-chain shutdown logging).
    chain_id: Id,
    /// The chain's subnet (the cancellation-propagation boundary).
    subnet_id: Id,
    /// The chain's cancellation token (child of the subnet token).
    token: CancellationToken,
    /// The chain's async worker pool (joined on drain).
    tasks: TaskTracker,
}

/// The assembly-stage chain manager: alias resolution, bootstrapped tracking,
/// registrants, and the queued chain-creation requests. Chain *creation* is
/// the documented deferral (module docs).
pub struct AssemblyChainManager {
    aliaser: Aliaser,
    /// Chains the node would shut down for if they failed (P, X, C).
    critical_chains: HashSet<Id>,
    /// Chains explicitly marked bootstrapped (the static fallback for chains
    /// with no live reporter registered).
    bootstrapped: RwLock<HashSet<Id>>,
    /// Per-chain live `is_bootstrapped` reporters installed by the chain creator
    /// (each reads its chain's running engine state). When present for a chain,
    /// the reporter wins over the static `bootstrapped` set.
    bootstrapped_reporters: Mutex<HashMap<Id, BootstrappedReporter>>,
    /// Indexer-style registrants notified when a chain is created (Go
    /// `AddRegistrant`).
    registrants: Mutex<Vec<Arc<dyn Indexer>>>,
    /// Chain-creation requests recorded by `start_chain_creator` (Go queues
    /// these into the chain creator; consumed when chain creation lands).
    queued: Mutex<Vec<ChainParameters>>,
    /// The bootstrap beacons (Go threads `n.bootstrappers` in as the platform
    /// chain's custom beacons; `ChainParameters::custom_beacons` carries only
    /// ids, so the manager keeps the full set).
    bootstrappers: Arc<dyn ValidatorManager>,
    /// Per-subnet cancellation tokens (children of the node's root
    /// `subnet_token`). A chain's token is a child of its subnet's token, so a
    /// subnet shutdown cancels only that subnet's chains.
    subnet_tokens: Mutex<std::collections::HashMap<Id, CancellationToken>>,
    /// Running chains' shutdown handles, drained by [`Self::shutdown`]
    /// (shutdown step 5).
    chains: Mutex<Vec<ChainHandle>>,
}

impl AssemblyChainManager {
    /// Build the manager over the critical-chain set and the beacon list.
    #[must_use]
    pub fn new(critical_chains: HashSet<Id>, bootstrappers: Arc<dyn ValidatorManager>) -> Self {
        Self {
            aliaser: Aliaser::new(),
            critical_chains,
            bootstrapped: RwLock::new(HashSet::new()),
            bootstrapped_reporters: Mutex::new(HashMap::new()),
            registrants: Mutex::new(Vec::new()),
            queued: Mutex::new(Vec::new()),
            bootstrappers,
            subnet_tokens: Mutex::new(std::collections::HashMap::new()),
            chains: Mutex::new(Vec::new()),
        }
    }

    /// The cancellation token for `subnet_id`, created as a child of
    /// `root_subnet_token` on first use (17 §4.1: root → subnet → chain). A
    /// chain registered under this subnet derives its token from the returned
    /// one, so cancelling a subnet's token reaches only that subnet's chains
    /// (17 §9).
    #[must_use]
    pub fn subnet_token(
        &self,
        subnet_id: Id,
        root_subnet_token: &CancellationToken,
    ) -> CancellationToken {
        self.subnet_tokens
            .lock()
            .entry(subnet_id)
            .or_insert_with(|| root_subnet_token.child_token())
            .clone()
    }

    /// Register a created chain's shutdown handles (called when chain creation
    /// lands; exercised today by the M8.30 cancellation-propagation test). The
    /// chain's token is a child of its subnet's token. Returns the chain's
    /// token + task tracker so the (future) chain worker can run under them.
    pub fn register_chain(
        &self,
        chain_id: Id,
        subnet_id: Id,
        root_subnet_token: &CancellationToken,
    ) -> (CancellationToken, TaskTracker) {
        let subnet_token = self.subnet_token(subnet_id, root_subnet_token);
        let token = subnet_token.child_token();
        let tasks = TaskTracker::new();
        self.chains.lock().push(ChainHandle {
            chain_id,
            subnet_id,
            token: token.clone(),
            tasks: tasks.clone(),
        });
        (token, tasks)
    }

    /// The number of running (registered) chains (asserted by tests).
    #[must_use]
    pub fn running_chains(&self) -> usize {
        self.chains.lock().len()
    }

    /// Shutdown step 5 (17 §4.3): drain every running chain. Per chain: cancel
    /// its token (which cascades to its async workers + executor + gossip),
    /// close its task tracker, then await the workers with the
    /// `consensus-shutdown-timeout` budget; stragglers past the budget are
    /// abandoned (their tasks observe the cancelled token and unwind on their
    /// own — Go `cancel → drain-with-timeout → abort`). Engines/VMs drop when
    /// the handles are cleared. Idempotent.
    pub async fn shutdown(&self, drain_timeout: std::time::Duration) {
        let handles: Vec<ChainHandle> = std::mem::take(&mut *self.chains.lock());
        for handle in &handles {
            tracing::debug!(
                chain_id = %handle.chain_id,
                subnet_id = %handle.subnet_id,
                "shutting down chain"
            );
            handle.token.cancel();
            handle.tasks.close();
        }
        for handle in handles {
            // Drain the chain's worker pool within the consensus-shutdown
            // budget; a chain that does not settle in time is abandoned (its
            // workers already observe the cancelled token).
            if tokio::time::timeout(drain_timeout, handle.tasks.wait())
                .await
                .is_err()
            {
                tracing::warn!(
                    chain_id = %handle.chain_id,
                    "chain did not drain within consensus-shutdown-timeout; abandoning stragglers"
                );
            }
        }
    }

    /// Register `alias` for `chain_id` (Go `Manager.Alias`).
    ///
    /// # Errors
    /// Propagates the aliaser's conflict error.
    pub fn alias(&self, chain_id: Id, alias: &str) -> ava_chains::Result<()> {
        self.aliaser.alias(chain_id, alias)
    }

    /// Resolve an alias to a chain ID (Go `Manager.Lookup`).
    ///
    /// # Errors
    /// The aliaser's unknown-alias error.
    pub fn lookup(&self, alias: &str) -> ava_chains::Result<Id> {
        use ava_chains::aliaser::AliaserReader;
        self.aliaser.lookup(alias)
    }

    /// The primary alias of `chain_id` (Go `Manager.PrimaryAlias`).
    ///
    /// # Errors
    /// The aliaser's no-alias error.
    pub fn primary_alias(&self, chain_id: Id) -> ava_chains::Result<String> {
        use ava_chains::aliaser::AliaserReader;
        self.aliaser.primary_alias(chain_id)
    }

    /// All aliases of `chain_id` (Go `Manager.Aliases`).
    #[must_use]
    pub fn aliases(&self, chain_id: Id) -> Vec<String> {
        use ava_chains::aliaser::AliaserReader;
        self.aliaser.aliases(chain_id)
    }

    /// Whether `chain_id` exists and finished bootstrapping (Go
    /// `Manager.IsBootstrapped`). When the chain creator has installed a live
    /// reporter for the chain (via [`Self::set_bootstrapped_reporter`]), the
    /// reporter — reading the running engine's consensus context — is
    /// authoritative; otherwise this falls back to the static set populated by
    /// [`Self::mark_bootstrapped`].
    #[must_use]
    pub fn is_bootstrapped(&self, chain_id: Id) -> bool {
        if let Some(reporter) = self.bootstrapped_reporters.lock().get(&chain_id) {
            return reporter();
        }
        self.bootstrapped.read().contains(&chain_id)
    }

    /// Install a live `is_bootstrapped` reporter for `chain_id` (the chain
    /// creator passes a closure reading the running engine's consensus context).
    /// Once installed, [`Self::is_bootstrapped`] reflects the live engine state
    /// rather than the static set.
    pub fn set_bootstrapped_reporter(&self, chain_id: Id, reporter: BootstrappedReporter) {
        self.bootstrapped_reporters
            .lock()
            .insert(chain_id, reporter);
    }

    /// Mark `chain_id` bootstrapped in the static set (Go's engine→manager
    /// `IsBootstrapped` callback for chains driven without a live reporter).
    pub fn mark_bootstrapped(&self, chain_id: Id) {
        self.bootstrapped.write().insert(chain_id);
    }

    /// Whether `chain_id` is one of the node-critical chains.
    #[must_use]
    pub fn is_critical(&self, chain_id: Id) -> bool {
        self.critical_chains.contains(&chain_id)
    }

    /// Subscribe a registrant to chain-creation events (Go `AddRegistrant`).
    pub fn add_registrant(&self, registrant: Arc<dyn Indexer>) {
        self.registrants.lock().push(registrant);
    }

    /// Step 26: record the platform-chain creation request (Go
    /// `StartChainCreator`). Actual chain creation is the documented deferral.
    ///
    /// # Errors
    /// Infallible today; the `Result` mirrors Go's signature for when creation
    /// lands.
    pub fn start_chain_creator(&self, params: ChainParameters) -> Result<()> {
        tracing::info!(
            chain_id = %params.id,
            vm_id = %params.vm_id,
            "queueing chain creation (chain construction lands with the chains milestone)"
        );
        self.queued.lock().push(params);
        Ok(())
    }

    /// The chain-creation requests recorded so far (consumed by the future
    /// chain-creator wiring; asserted by tests).
    #[must_use]
    pub fn queued_chains(&self) -> Vec<ChainParameters> {
        self.queued.lock().clone()
    }

    /// The bootstrap beacons threaded through to the platform chain.
    #[must_use]
    pub fn bootstrappers(&self) -> Arc<dyn ValidatorManager> {
        Arc::clone(&self.bootstrappers)
    }
}

/// Everything step 20 hands back to `Node::new`.
pub struct ChainManagerInit {
    /// The assembly-stage chain manager.
    pub manager: Arc<AssemblyChainManager>,
    /// The adaptive timeout manager (Go `n.timeoutManager`).
    pub timeout_manager: Arc<AdaptiveTimeoutManager>,
    /// The engine chain router (Go `n.chainRouter`, initialized here).
    pub chain_router: Arc<ChainRouter>,
    /// The benchlist (Go `n.benchlistManager`, created with the router).
    pub benchlist: Arc<Benchlist>,
    /// The X-Chain's genesis-derived ID.
    pub x_chain_id: Id,
    /// The C-Chain's genesis-derived ID.
    pub c_chain_id: Id,
}

/// Step 20: derive the X/C chain IDs from genesis, build the timeout manager,
/// the engine chain router (filling the [`RouterBridge`] slot), the benchlist,
/// and the assembly chain manager (mirror Go `initChainManager`).
///
/// # Errors
/// - Genesis parsing failures (`vm_genesis`).
/// - Metrics-namespace registration failures.
/// - Timeout-manager construction failures.
pub fn init_chain_manager(
    config: &ava_config::node::Config,
    metrics: &NodeMetrics,
    bootstrappers: &Arc<dyn ValidatorManager>,
    router_bridge: &RouterBridge,
    node_id: ava_types::node_id::NodeId,
) -> Result<ChainManagerInit> {
    let x_chain_id = vm_genesis(&config.genesis_bytes, avm_id())?.id();
    let c_chain_id = vm_genesis(&config.genesis_bytes, evm_id())?.id();

    let critical_chains: HashSet<Id> = [PLATFORM_CHAIN_ID, x_chain_id, c_chain_id]
        .into_iter()
        .collect();

    let _requests_registry = ava_api::metrics::make_and_register(
        metrics.gatherer.as_ref(),
        &crate::init::namespace::requests(),
    )?;
    let _responses_registry = ava_api::metrics::make_and_register(
        metrics.gatherer.as_ref(),
        &crate::init::namespace::responses(),
    )?;
    let _benchlist_registry = ava_api::metrics::make_and_register(
        metrics.gatherer.as_ref(),
        &crate::init::namespace::benchlist(),
    )?;

    let clock: Arc<dyn Clock> = Arc::new(RealClock);
    let timeout_manager = Arc::new(
        AdaptiveTimeoutManager::new(&config.adaptive_timeout_config, clock)
            .map_err(|e| Error::ChainManager(e.to_string()))?,
    );

    // The engine router replaces Go's `chainRouter.Initialize` (the Rust
    // router takes its timeout manager at construction).
    let chain_router = ChainRouter::new(Arc::clone(&timeout_manager));
    router_bridge.set_engine_router(chain_router.clone());

    // The Rust benchlist is the simplified M3 port: only the bench-duration
    // cap maps from the Go config block (divergence noted in
    // `tests/PORTING.md`). Seeded from the NodeID for per-node determinism.
    let benchlist = Arc::new(Benchlist::new(
        EngineBenchlistConfig {
            max_bench_duration: config.benchlist_config.bench_duration,
            ..EngineBenchlistConfig::default()
        },
        seed_from_node_id(node_id),
    ));

    let manager = Arc::new(AssemblyChainManager::new(
        critical_chains,
        Arc::clone(bootstrappers),
    ));

    Ok(ChainManagerInit {
        manager,
        timeout_manager,
        chain_router,
        benchlist,
        x_chain_id,
        c_chain_id,
    })
}

/// Step 26: queue the platform chain (mirror Go `initChains` — its genesis
/// specifies the other chains to create).
///
/// # Errors
/// Propagates `start_chain_creator`.
pub fn init_chains(manager: &AssemblyChainManager, genesis_bytes: &[u8]) -> Result<()> {
    tracing::info!("initializing chains");
    manager.start_chain_creator(ChainParameters {
        id: PLATFORM_CHAIN_ID,
        subnet_id: PRIMARY_NETWORK_ID,
        genesis_data: genesis_bytes.to_vec(),
        vm_id: platform_vm_id(),
        fx_ids: Vec::new(),
        // Go passes the beacon validators.Manager; `ChainParameters` carries
        // ids only — the manager retains the full set (`bootstrappers()`).
        custom_beacons: Vec::new(),
    })
}

/// Derive a deterministic per-node benchlist seed from the first 8 NodeID
/// bytes.
fn seed_from_node_id(node_id: ava_types::node_id::NodeId) -> u64 {
    let bytes = node_id.as_bytes();
    let mut seed = [0u8; 8];
    for (dst, src) in seed.iter_mut().zip(bytes.iter()) {
        *dst = *src;
    }
    u64::from_be_bytes(seed)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use ava_validators::DefaultManager;

    use super::*;

    fn manager() -> AssemblyChainManager {
        let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
        AssemblyChainManager::new(std::iter::once(PLATFORM_CHAIN_ID).collect(), bootstrappers)
    }

    #[test]
    fn is_bootstrapped_defaults_false_then_static_mark_flips_it() {
        let mgr = manager();
        assert!(
            !mgr.is_bootstrapped(PLATFORM_CHAIN_ID),
            "is_bootstrapped(P) defaults false before any chain runs"
        );
        mgr.mark_bootstrapped(PLATFORM_CHAIN_ID);
        assert!(
            mgr.is_bootstrapped(PLATFORM_CHAIN_ID),
            "mark_bootstrapped flips the static set"
        );
    }

    #[test]
    fn live_reporter_is_authoritative_over_static_set() {
        let mgr = manager();
        // Even with the static set marked, an installed live reporter wins:
        // it reflects the running engine's consensus context.
        mgr.mark_bootstrapped(PLATFORM_CHAIN_ID);
        let live = Arc::new(AtomicBool::new(false));
        let probe = Arc::clone(&live);
        mgr.set_bootstrapped_reporter(
            PLATFORM_CHAIN_ID,
            Box::new(move || probe.load(Ordering::SeqCst)),
        );
        assert!(
            !mgr.is_bootstrapped(PLATFORM_CHAIN_ID),
            "the live reporter (NormalOp not yet reached) overrides the static mark"
        );
        live.store(true, Ordering::SeqCst); // engine reaches NormalOp
        assert!(
            mgr.is_bootstrapped(PLATFORM_CHAIN_ID),
            "is_bootstrapped reflects the live reporter once it reports NormalOp"
        );
    }
}
