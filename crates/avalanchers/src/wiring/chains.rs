// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! In-process chain-manager assembly for the `avalanchers` binary (M3.28).
//!
//! This module provides **production (non-test-gated) equivalents** of the
//! concrete impls the `ava-chains` pipeline is generic over (see
//! `crates/ava-chains/tests/support/mod.rs`) so the binary can:
//!
//! 1. build the [`ava_chains::VmManager`],
//! 2. register a built-in **no-op test-VM** [`Factory`] under a fixed VM [`Id`]
//!    (the factory's product satisfies the manager's `ProbeableVm`
//!    `version`/`shutdown` probe), and
//! 3. create **one in-process Snowman chain** through the full
//!    [`ava_chains::create_snowman_chain`] pipeline
//!    (`inner → tracedvm → proposervm → metervm → tracedvm → change-notifier`),
//!    wired with concrete in-process loopback `Sender`/`Router`/`ValidatorManager`
//!    /`Clock`/`ValidatorState`/`AppSender` impls (an in-process node needs no
//!    real network).
//!
//! No chain is auto-run on a bare `avalanchers` invocation; this wiring is
//! exercised by the `in_process_chain` integration test and is the seam the rest
//! of node assembly grows from in later milestones.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};
use tokio_util::sync::CancellationToken;

use ava_chains::create_snowman_chain;
use ava_chains::manager::{DynProbe, Factory, ProbeableVm, VmManager};
use ava_crypto::staking;
use ava_database::MemDb;
use ava_engine::networking::router::ChainRouter;
use ava_engine::networking::timeout::{AdaptiveTimeoutConfig, AdaptiveTimeoutManager};
use ava_proposervm::{BlockSigner, StakingIdentity};
use ava_snow::snowball::Parameters;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, MockClock};
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use ava_validators::{DefaultManager, ValidatorManager};
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::error::Result as VmResult;
use ava_vm::testutil::TestVm;
use prometheus::Registry;

/// The fixed VM id the built-in no-op test VM is registered under.
const TEST_VM_ID: Id = Id::EMPTY;

/// The version the built-in no-op test VM reports (matches `TestVm::version`).
const TEST_VM_VERSION: &str = "testvm/0.0.0";

/// Errors raised while assembling the in-process chain manager / chain.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A `VmManager` registration / lookup failed.
    #[error("chain manager: {0}")]
    Manager(#[from] ava_chains::error::Error),
    /// The adaptive-timeout manager could not be built.
    #[error("timeout manager: {0}")]
    Timeout(#[from] ava_engine::networking::timeout::TimeoutError),
    /// A validator-set assembly failed.
    #[error("validators: {0}")]
    Validators(#[from] ava_validators::Error),
    /// The VM pipeline could not report a last-accepted height.
    #[error("vm: {0}")]
    Vm(#[from] ava_vm::error::Error),
    /// A staking identity could not be generated.
    #[error("staking identity: {0}")]
    Identity(String),
    /// The network genesis could not be built / parsed.
    #[error("genesis: {0}")]
    Genesis(#[from] ava_genesis::GenesisError),
}

/// Result alias for the wiring module.
pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Built-in no-op test-VM factory
// ---------------------------------------------------------------------------

/// The probe surface the [`VmManager`] queries at registration: a fresh no-op
/// VM that reports a fixed version and shuts down cleanly.
struct NoopProbe;

#[async_trait]
impl ProbeableVm for NoopProbe {
    async fn version(&self, _token: &CancellationToken) -> ava_chains::error::Result<String> {
        Ok(TEST_VM_VERSION.to_string())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> ava_chains::error::Result<()> {
        Ok(())
    }
}

/// A built-in static [`Factory`] whose product the manager probes for its
/// version. The chain-creation pipeline constructs the concrete inner VM
/// directly (the factory exists so the manager records the VM under its id with
/// a known version, mirroring Go `manager.RegisterFactory`).
struct TestVmFactory;

#[async_trait]
impl Factory for TestVmFactory {
    async fn new_vm(&self) -> ava_chains::error::Result<Box<dyn std::any::Any + Send>> {
        Ok(Box::new(DynProbe(Box::new(NoopProbe))))
    }
}

/// Builds a [`VmManager`] and registers the built-in no-op test-VM [`Factory`]
/// under [`TEST_VM_ID`] (probing its `version`/`shutdown` once).
///
/// # Errors
/// Propagates a [`VmManager::register_factory`] failure.
pub async fn register_test_vm_factory() -> Result<VmManager> {
    let token = CancellationToken::new();
    let manager = VmManager::new();
    manager
        .register_factory(&token, TEST_VM_ID, Arc::new(TestVmFactory))
        .await?;
    Ok(manager)
}

// ---------------------------------------------------------------------------
// In-process loopback impls (production equivalents of `tests/support`)
// ---------------------------------------------------------------------------

/// A fixed single-validator-set [`ValidatorState`] for the proposervm windower.
#[derive(Clone)]
struct FixedState {
    set: BTreeMap<NodeId, GetValidatorOutput>,
}

#[async_trait]
impl ValidatorState for FixedState {
    async fn get_minimum_height(&self) -> ava_validators::Result<u64> {
        Ok(0)
    }
    async fn get_current_height(&self) -> ava_validators::Result<u64> {
        Ok(1)
    }
    async fn get_subnet_id(&self, _chain: Id) -> ava_validators::Result<Id> {
        Ok(Id::EMPTY)
    }
    async fn get_validator_set(
        &self,
        _height: u64,
        _subnet: Id,
    ) -> ava_validators::Result<BTreeMap<NodeId, GetValidatorOutput>> {
        Ok(self.set.clone())
    }
    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> ava_validators::Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        Ok((BTreeMap::new(), 1))
    }
    async fn get_warp_validator_sets(
        &self,
        _height: u64,
    ) -> ava_validators::Result<std::collections::HashMap<Id, WarpSet>> {
        Ok(std::collections::HashMap::new())
    }
}

/// A no-op in-process [`ava_engine::common::sender::Sender`]. An in-process node
/// has no peers to query, so every outbound op is dropped.
#[derive(Default)]
struct NoopSender;

#[async_trait]
impl ava_engine::common::sender::Sender for NoopSender {
    fn send_get_state_summary_frontier(&self, _nodes: &HashSet<NodeId>, _req: u32) {}
    fn send_state_summary_frontier(&self, _node: NodeId, _req: u32, _summary: Vec<u8>) {}
    fn send_get_accepted_state_summary(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _heights: &[u64],
    ) {
    }
    fn send_accepted_state_summary(&self, _node: NodeId, _req: u32, _summary_ids: &[Id]) {}
    fn send_get_accepted_frontier(&self, _nodes: &HashSet<NodeId>, _req: u32) {}
    fn send_accepted_frontier(&self, _node: NodeId, _req: u32, _container_id: Id) {}
    fn send_get_accepted(&self, _nodes: &HashSet<NodeId>, _req: u32, _ids: &[Id]) {}
    fn send_accepted(&self, _node: NodeId, _req: u32, _ids: &[Id]) {}
    fn send_get(&self, _node: NodeId, _req: u32, _container_id: Id) {}
    fn send_get_ancestors(&self, _node: NodeId, _req: u32, _container_id: Id) {}
    fn send_put(&self, _node: NodeId, _req: u32, _container: Vec<u8>) {}
    fn send_ancestors(&self, _node: NodeId, _req: u32, _containers: Vec<Vec<u8>>) {}
    fn send_push_query(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _container: Vec<u8>,
        _requested_height: u64,
    ) {
    }
    fn send_pull_query(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _container_id: Id,
        _requested_height: u64,
    ) {
    }
    fn send_chits(
        &self,
        _node: NodeId,
        _req: u32,
        _preferred: Id,
        _preferred_at_height: Id,
        _accepted: Id,
        _accepted_height: u64,
    ) {
    }

    async fn send_app_request(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _bytes: Vec<u8>,
    ) -> ava_engine::error::Result<()> {
        Ok(())
    }
    async fn send_app_response(
        &self,
        _node: NodeId,
        _req: u32,
        _bytes: Vec<u8>,
    ) -> ava_engine::error::Result<()> {
        Ok(())
    }
    async fn send_app_error(
        &self,
        _node: NodeId,
        _req: u32,
        _code: i32,
        _msg: &str,
    ) -> ava_engine::error::Result<()> {
        Ok(())
    }
    async fn send_app_gossip(
        &self,
        _cfg: ava_engine::common::sender::SendConfig,
        _bytes: Vec<u8>,
    ) -> ava_engine::error::Result<()> {
        Ok(())
    }
}

/// A no-op in-process [`AppSender`] (the VM emits no app messages to peers).
#[derive(Default)]
struct NoopAppSender;

#[async_trait]
impl AppSender for NoopAppSender {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        _nodes: &HashSet<NodeId>,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn send_app_error(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _code: i32,
        _message: &str,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn send_app_gossip(
        &self,
        _token: &CancellationToken,
        _config: SendConfig,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }
}

/// Generates a staking cert + an ECDSA P-256 signer over the proposervm header
/// bytes, returning the identity and the derived node id.
fn staking_identity() -> Result<(StakingIdentity, NodeId)> {
    let (cert_pem, key_pem) =
        staking::new_cert_and_key_bytes().map_err(|e| Error::Identity(format!("gen cert: {e}")))?;
    let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .next()
        .ok_or_else(|| Error::Identity("no cert block".to_string()))?
        .map_err(|e| Error::Identity(format!("parse cert pem: {e}")))?
        .to_vec();
    let node_id = staking::node_id_from_cert(&cert_der);

    let key_pair = rcgen::KeyPair::from_pem(&key_pem)
        .map_err(|e| Error::Identity(format!("parse key: {e}")))?;
    let pkcs8 = key_pair.serialize_der();
    let signer: BlockSigner = Arc::new(move |msg: &[u8]| {
        let rng = SystemRandom::new();
        let signing = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng)
            .map_err(|e| format!("import pkcs8: {e:?}"))?;
        let sig = signing
            .sign(&rng, msg)
            .map_err(|e| format!("sign: {e:?}"))?;
        Ok(sig.as_ref().to_vec())
    });
    Ok((
        StakingIdentity {
            certificate: cert_der,
            signer,
        },
        node_id,
    ))
}

/// Single-node consensus parameters (k=1, one self-validator).
fn single_node_params() -> Parameters {
    Parameters {
        k: 1,
        alpha_preference: 1,
        alpha_confidence: 1,
        beta: 1,
        concurrent_repolls: 1,
        optimal_processing: 1,
        max_outstanding_items: 256,
        max_item_processing_time: Duration::from_secs(30),
    }
}

/// A relaxed adaptive-timeout config for the in-process router.
fn timeout_config() -> AdaptiveTimeoutConfig {
    AdaptiveTimeoutConfig {
        initial_timeout: Duration::from_secs(2),
        minimum_timeout: Duration::from_secs(2),
        maximum_timeout: Duration::from_secs(10),
        timeout_coefficient: 1.0,
        timeout_halflife: Duration::from_secs(5),
    }
}

// ---------------------------------------------------------------------------
// In-process chain assembly
// ---------------------------------------------------------------------------

/// Assembles one in-process Snowman chain around the built-in no-op test VM,
/// driving the full [`create_snowman_chain`] pipeline, and returns the chain's
/// **last-accepted height** (genesis = 0).
///
/// All network-facing dependencies are no-op/loopback in-process impls: an
/// in-process node needs no peers. The real `ava_engine` [`ChainRouter`] (over a
/// real [`AdaptiveTimeoutManager`]) is used so the handler sink registers exactly
/// as it does in a networked node.
///
/// # Errors
/// Propagates DB / VM-init / consensus-construction / identity failures.
pub async fn build_in_process_chain() -> Result<u64> {
    let token = CancellationToken::new();
    let reg = Registry::new();

    // Self validator: one equally-weighted staker with a fresh staking identity.
    let (identity, node_id) = staking_identity()?;
    let validators = Arc::new(DefaultManager::new());
    validators.add_staker(Id::EMPTY, node_id, None, Id::EMPTY, 1)?;

    let mut set = BTreeMap::new();
    set.insert(
        node_id,
        GetValidatorOutput {
            node_id,
            public_key: None,
            weight: 1,
        },
    );
    let validator_state = FixedState { set };

    // The real router over a real adaptive-timeout manager (clock-injected).
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(SystemTime::UNIX_EPOCH));
    let timeouts = Arc::new(AdaptiveTimeoutManager::new(
        &timeout_config(),
        Arc::clone(&clock),
    )?);
    let router = ChainRouter::new(timeouts);

    let chain_id = Id::EMPTY;
    let subnet_id = Id::EMPTY;
    let chain_ctx = ava_vm::testutil::test_chain_context();
    let sender = Arc::new(NoopSender);
    let app_sender: Arc<dyn AppSender> = Arc::new(NoopAppSender);

    // Frontier-agreement beacon set: the single self node, weight 1.
    let mut beacons = BTreeMap::new();
    beacons.insert(node_id, 1u64);

    let chain = create_snowman_chain(
        &token,
        chain_id,
        subnet_id,
        single_node_params(),
        MemDb::new(),
        "test",
        chain_ctx,
        clock,
        validator_state,
        Some(identity),
        TestVm::new(),
        Vec::new(),
        b"genesis",
        sender,
        app_sender,
        validators,
        // Frontier-agreement beacons: this single self node, weight 1.
        beacons,
        router.as_ref(),
        &reg,
    )
    .await?;

    // M4.30b: the engine moved into the handler's `EngineManager`; the chain's
    // observability handle is now the shared `ConsensusContext`. Before the
    // handler starts, it sits in `Initializing` (genesis is seeded by the VM at
    // `initialize`, last-accepted height 0). The pipeline having assembled the
    // chain at genesis is the smoke invariant; genesis is height 0.
    debug_assert_eq!(
        **chain.ctx.state.load(),
        ava_snow::EngineState::Initializing,
        "freshly created chain sits in Initializing until the handler starts"
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// Real P-Chain in-process boot (M4.30c)
// ---------------------------------------------------------------------------

/// An outbound message observed by the [`RecordingSender`]. Only the
/// frontier-discovery broadcast is decoded; everything else is `Other`.
#[derive(Clone, Debug)]
pub enum Sent {
    /// `SendGetAcceptedFrontier` — the bootstrapper's frontier-discovery query
    /// to the beacon set.
    GetAcceptedFrontier {
        /// The (sorted) beacon node set the broadcast addressed.
        nodes: Vec<NodeId>,
        /// The request id.
        req: u32,
    },
    /// Any other outbound op (dropped in-process; recorded only as a marker).
    Other,
}

/// An in-process [`ava_engine::common::sender::Sender`] that **records**
/// outbound messages so a node-level test can observe the bootstrapper's
/// frontier broadcast. An in-process node has no peers, so nothing is actually
/// transmitted — this is the recording stand-in for the live ava-network-backed
/// `Sender` (the documented live arm).
#[derive(Default)]
pub struct RecordingSender {
    log: Mutex<Vec<Sent>>,
}

impl RecordingSender {
    fn push(&self, s: Sent) {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).push(s);
    }

    /// Drains and returns the recorded outbound messages.
    #[must_use]
    pub fn drain(&self) -> Vec<Sent> {
        std::mem::take(&mut self.log.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

fn sorted_nodes(nodes: &HashSet<NodeId>) -> Vec<NodeId> {
    let mut v: Vec<NodeId> = nodes.iter().copied().collect();
    v.sort();
    v
}

#[async_trait]
impl ava_engine::common::sender::Sender for RecordingSender {
    fn send_get_state_summary_frontier(&self, _nodes: &HashSet<NodeId>, _req: u32) {}
    fn send_state_summary_frontier(&self, _node: NodeId, _req: u32, _summary: Vec<u8>) {}
    fn send_get_accepted_state_summary(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _heights: &[u64],
    ) {
    }
    fn send_accepted_state_summary(&self, _node: NodeId, _req: u32, _summary_ids: &[Id]) {}
    fn send_get_accepted_frontier(&self, nodes: &HashSet<NodeId>, req: u32) {
        self.push(Sent::GetAcceptedFrontier {
            nodes: sorted_nodes(nodes),
            req,
        });
    }
    fn send_accepted_frontier(&self, _node: NodeId, _req: u32, _container_id: Id) {}
    fn send_get_accepted(&self, _nodes: &HashSet<NodeId>, _req: u32, _ids: &[Id]) {}
    fn send_accepted(&self, _node: NodeId, _req: u32, _ids: &[Id]) {}
    fn send_get(&self, _node: NodeId, _req: u32, _container_id: Id) {
        self.push(Sent::Other);
    }
    fn send_get_ancestors(&self, _node: NodeId, _req: u32, _container_id: Id) {
        self.push(Sent::Other);
    }
    fn send_put(&self, _node: NodeId, _req: u32, _container: Vec<u8>) {}
    fn send_ancestors(&self, _node: NodeId, _req: u32, _containers: Vec<Vec<u8>>) {}
    fn send_push_query(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _container: Vec<u8>,
        _requested_height: u64,
    ) {
        self.push(Sent::Other);
    }
    fn send_pull_query(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _container_id: Id,
        _requested_height: u64,
    ) {
        self.push(Sent::Other);
    }
    fn send_chits(
        &self,
        _node: NodeId,
        _req: u32,
        _preferred: Id,
        _preferred_at_height: Id,
        _accepted: Id,
        _accepted_height: u64,
    ) {
    }

    async fn send_app_request(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _bytes: Vec<u8>,
    ) -> ava_engine::error::Result<()> {
        Ok(())
    }
    async fn send_app_response(
        &self,
        _node: NodeId,
        _req: u32,
        _bytes: Vec<u8>,
    ) -> ava_engine::error::Result<()> {
        Ok(())
    }
    async fn send_app_error(
        &self,
        _node: NodeId,
        _req: u32,
        _code: i32,
        _msg: &str,
    ) -> ava_engine::error::Result<()> {
        Ok(())
    }
    async fn send_app_gossip(
        &self,
        _cfg: ava_engine::common::sender::SendConfig,
        _bytes: Vec<u8>,
    ) -> ava_engine::error::Result<()> {
        Ok(())
    }
}

/// The handle returned by [`boot_in_process_pchain`]: everything a node-level
/// test needs to observe and tear down the booted P-Chain.
pub struct PChainBootHandle {
    /// The shared consensus context — the observability handle for the engine
    /// phase (`ctx.state`: `Initializing → Bootstrapping → NormalOp`).
    pub ctx: Arc<ava_snow::ConsensusContext>,
    /// The recording sender; `drain()` reveals the frontier broadcast.
    pub sender: Arc<RecordingSender>,
    /// The handler task; cancel [`Self::token`] then `await` it for a clean
    /// (leak-free) shutdown.
    pub join: tokio::task::JoinHandle<()>,
    /// The handler's cancellation token (cancel to stop the handler task).
    pub token: CancellationToken,
    /// The P-Chain genesis block id (`sha256(p_chain_genesis_bytes)`).
    pub genesis_id: Id,
    /// The bootstrap beacon node set (sorted), as addressed by the frontier
    /// broadcast.
    pub beacons: Vec<NodeId>,
    /// The handler sink, kept alive for the handler's lifetime (dropping it
    /// would unregister the chain from the router).
    pub _sink: ava_engine::networking::handler::ChainHandlerSink,
}

/// Materializes the **real `ava_platformvm::PlatformVm`** (seeded from the
/// `network_id` P-Chain genesis), drives it through the full
/// [`create_snowman_chain`] pipeline, starts the handler, and returns a
/// [`PChainBootHandle`]. Once the handler task runs, the bootstrapper flips the
/// shared context to `EngineState::Bootstrapping` and broadcasts
/// `GetAcceptedFrontier` to the beacon set.
///
/// All network-facing dependencies are in-process loopback impls except the
/// [`RecordingSender`], which records the outbound frontier broadcast. The
/// real ava-network-backed `Sender` (engine→wire + real peers) is the
/// documented **live arm** and is out of scope here.
///
/// # Errors
/// Propagates genesis-build, DB / VM-init, consensus-construction, identity, or
/// timeout-manager failures.
pub async fn boot_in_process_pchain(network_id: u32) -> Result<PChainBootHandle> {
    boot_pchain(network_id, true).await
}

/// Like [`boot_in_process_pchain`], but boots as a **solo node with an empty
/// beacon set** so the chain runs all the way to `EngineState::NormalOp`.
///
/// With nothing to bootstrap *from*, the bootstrapper short-circuits
/// `Bootstrapping → NormalOp` (`ava_engine::snowman::bootstrap` empty-beacon
/// path) — exactly as a Go `--network-id=local` node with no default beacons
/// does — so NormalOp is reached without the live ava-network-backed `Sender`.
/// This is the template the production node-assembly chain-creator replicates
/// to drive a single `avalanchers` node to NormalOp (M9.15 step (a)).
///
/// # Errors
/// Propagates genesis-build, DB / VM-init, consensus-construction, identity, or
/// timeout-manager failures.
pub async fn boot_in_process_pchain_to_normalop(network_id: u32) -> Result<PChainBootHandle> {
    boot_pchain(network_id, false).await
}

/// Shared body for the two P-Chain boot entrypoints. When `include_self_beacon`
/// is `true` the chain boots with the single self node as its frontier-agreement
/// beacon (stalls at `Bootstrapping`, awaiting frontier replies the in-process
/// `RecordingSender` never delivers); when `false` the beacon set is empty and
/// the bootstrapper runs straight through to `NormalOp`.
async fn boot_pchain(network_id: u32, include_self_beacon: bool) -> Result<PChainBootHandle> {
    let token = CancellationToken::new();
    let reg = Registry::new();

    // Real P-Chain genesis for the network (the M8-complete embedded source).
    let (genesis_bytes, avax_asset_id) = ava_genesis::genesis_bytes(network_id, None)?;
    let genesis_id = ava_platformvm::genesis::genesis_id(&genesis_bytes);

    // Self validator: one equally-weighted staker with a fresh staking identity.
    let (identity, node_id) = staking_identity()?;
    let validators = Arc::new(DefaultManager::new());
    validators.add_staker(
        ava_types::constants::PRIMARY_NETWORK_ID,
        node_id,
        None,
        Id::EMPTY,
        1,
    )?;

    let mut set = BTreeMap::new();
    set.insert(
        node_id,
        GetValidatorOutput {
            node_id,
            public_key: None,
            weight: 1,
        },
    );
    let validator_state = FixedState { set };

    // The real router over a real adaptive-timeout manager (clock-injected;
    // virtual time — no wall clock).
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(SystemTime::UNIX_EPOCH));
    let timeouts = Arc::new(AdaptiveTimeoutManager::new(
        &timeout_config(),
        Arc::clone(&clock),
    )?);
    let router = ChainRouter::new(timeouts);

    // The platform chain id / primary-network subnet (Go `constants`).
    let chain_id = ava_node::init::chain_manager::PLATFORM_CHAIN_ID;
    let subnet_id = ava_types::constants::PRIMARY_NETWORK_ID;

    // A per-network ChainContext (network id + fork schedule from the chosen
    // network), so the VM initializes with the production identity surface.
    let chain_ctx = Arc::new(ava_snow::ChainContext {
        network_id,
        subnet_id,
        chain_id,
        node_id,
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(network_id),
        x_chain_id: Id::EMPTY,
        c_chain_id: Id::EMPTY,
        avax_asset_id,
        chain_data_dir: std::path::PathBuf::new(),
    });

    let sender = Arc::new(RecordingSender::default());
    let app_sender: Arc<dyn AppSender> = Arc::new(NoopAppSender);

    // Frontier-agreement beacon set: the single self node (weight 1) when
    // bootstrapping from a peer, or empty for a solo node that short-circuits
    // straight to NormalOp.
    let mut beacons = BTreeMap::new();
    if include_self_beacon {
        beacons.insert(node_id, 1u64);
    }

    let chain = create_snowman_chain(
        &token,
        chain_id,
        subnet_id,
        single_node_params(),
        MemDb::new(),
        "P",
        chain_ctx,
        clock,
        validator_state,
        Some(identity),
        ava_platformvm::vm::PlatformVm::new(),
        Vec::new(),
        &genesis_bytes,
        Arc::clone(&sender),
        app_sender,
        validators,
        beacons.clone(),
        router.as_ref(),
        &reg,
    )
    .await?;

    let ctx = Arc::clone(&chain.ctx);
    let beacon_nodes: Vec<NodeId> = beacons.keys().copied().collect();

    // Start the handler task: it activates the initial (`Bootstrapping`) engine,
    // which flips `ctx.state` and broadcasts `GetAcceptedFrontier`.
    let join = chain.handler.start();

    Ok(PChainBootHandle {
        ctx,
        sender,
        join,
        token,
        genesis_id,
        beacons: beacon_nodes,
        _sink: chain.sink,
    })
}
