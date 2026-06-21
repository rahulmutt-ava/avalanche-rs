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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use ava_chains::create_snowman_chain;
use ava_chains::manager::{DynProbe, Factory, ProbeableVm, VmManager};
use ava_crypto::staking;
use ava_database::{DynDatabase, MemDb};
use ava_engine::networking::handler::ChainHandlerSink;
use ava_engine::networking::router::{ChainMessageSink, ChainRouter, InboundOp};
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
    /// The C-Chain VM could not be built from genesis (`EvmVm::from_genesis`).
    #[error("c-chain vm: {0}")]
    CChainVm(#[from] ava_evm::error::Error),
    /// The C-Chain on-disk state scratch dir could not be created.
    #[error("c-chain data dir: {0}")]
    DataDir(#[from] std::io::Error),
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

/// The self-delivery target installed on a [`RecordingSender`] to make a solo
/// node's consensus poll self-resolve: the running node's own handler sink plus
/// its node id. With it installed, the engine's outbound poll/vote ops are
/// looped back as inbound ops addressed *from* the self node, so a `k=1`/`β=1`
/// poll on a self-built block completes and the block is accepted through the
/// genuine consensus path (M9.15 STEP (m)).
struct Loopback {
    /// The node the looped-back inbound ops appear to come from (the sole
    /// validator — itself).
    self_node: NodeId,
    /// The running chain's handler sink (the inbound-message ingress the router
    /// would otherwise drive from the network).
    sink: ChainHandlerSink,
}

/// An in-process [`ava_engine::common::sender::Sender`] that **records**
/// outbound messages so a node-level test can observe the bootstrapper's
/// frontier broadcast. An in-process node has no peers, so nothing is actually
/// transmitted — this is the recording stand-in for the live ava-network-backed
/// `Sender` (the documented live arm).
///
/// **Opt-in self-loopback.** When [`install_loopback`](Self::install_loopback)
/// has been called, the consensus **poll path** (`send_push_query` /
/// `send_pull_query` / `send_chits`) is additionally *delivered back* to the
/// node's own handler as inbound ops — closing the query→chits→accept loop a
/// solo node needs to finalize a self-built block. Until then (the default), every
/// outbound op is dropped exactly as before, so existing callers are unchanged.
#[derive(Default)]
pub struct RecordingSender {
    log: Mutex<Vec<Sent>>,
    loopback: std::sync::OnceLock<Loopback>,
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

    /// Install the self-loopback: route the consensus poll path back into
    /// `sink` as inbound ops appearing to come from `self_node`. Idempotent — a
    /// second call is ignored (the channel is set once at boot).
    fn install_loopback(&self, self_node: NodeId, sink: ChainHandlerSink) {
        let _ = self.loopback.set(Loopback { self_node, sink });
    }

    /// Deliver `op` back to the node's own handler as an inbound message from the
    /// self node, if the loopback has been installed. Fire-and-forget: the
    /// handler drains its queue sequentially, so the looped-back op is processed
    /// after the engine call that produced it returns (no re-entrancy).
    fn loopback(&self, op: InboundOp) {
        if let Some(lb) = self.loopback.get() {
            let sink = lb.sink.clone();
            let node = lb.self_node;
            tokio::spawn(async move {
                sink.push(node, op).await;
            });
        }
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
        req: u32,
        container: Vec<u8>,
        requested_height: u64,
    ) {
        self.push(Sent::Other);
        self.loopback(InboundOp::PushQuery {
            request_id: req,
            container,
            requested_height,
        });
    }
    fn send_pull_query(
        &self,
        _nodes: &HashSet<NodeId>,
        req: u32,
        container_id: Id,
        requested_height: u64,
    ) {
        self.push(Sent::Other);
        self.loopback(InboundOp::PullQuery {
            request_id: req,
            container_id,
            requested_height,
        });
    }
    fn send_chits(
        &self,
        _node: NodeId,
        req: u32,
        preferred: Id,
        preferred_at_height: Id,
        accepted: Id,
        accepted_height: u64,
    ) {
        self.loopback(InboundOp::Chits {
            request_id: req,
            preferred_id: preferred,
            preferred_id_at_height: preferred_at_height,
            accepted_id: accepted,
            accepted_height,
        });
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
    /// The height the consensus core was rooted at when this chain was created
    /// — `0` for a fresh genesis tip, the persisted height for a node that
    /// recovered an advanced tip from disk (read from
    /// `vm.get_block(vm.last_accepted()).height()` at boot; M9.15 STEP (k)/(m)).
    /// A restart over a base db that already holds an engine-issued tip resumes
    /// at that height.
    pub last_accepted_height: u64,
    /// The bootstrap beacon node set (sorted), as addressed by the frontier
    /// broadcast.
    pub beacons: Vec<NodeId>,
    /// The VM→engine notification channel. Sending
    /// [`VmEvent::PendingTxs`](ava_vm::vm::VmEvent::PendingTxs) drives the running
    /// engine to build + issue + (with the loopback installed) accept a block —
    /// the in-process equivalent of a VM signalling its `toEngine` channel
    /// (M9.15 STEP (m)).
    pub vm_tx: mpsc::Sender<ava_vm::vm::VmEvent>,
    /// The handler sink, kept alive for the handler's lifetime (dropping it
    /// would unregister the chain from the router).
    pub _sink: ava_engine::networking::handler::ChainHandlerSink,
    /// The chain's on-disk scratch dir (the C-Chain Firewood state db), kept
    /// alive for the booted chain's lifetime; dropping it would delete the state
    /// db out from under the running VM. `None` for chains with no on-disk state
    /// (P/X boot over in-memory state).
    pub _data_dir: Option<tempfile::TempDir>,
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
    boot_pchain(network_id, true, fresh_mem_db(), CancellationToken::new()).await
}

/// A fresh ephemeral in-memory base db — the default for the in-process boot
/// entrypoints and the `*_with_db`-less wrappers (tests / non-persistent runs).
/// The live `avalanchers` node instead threads its real `Arc<dyn DynDatabase>`
/// (the assembled `Node`'s persistent backend) through the `*_with_db` path.
fn fresh_mem_db() -> Arc<dyn DynDatabase> {
    Arc::new(MemDb::new())
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
    boot_pchain(network_id, false, fresh_mem_db(), CancellationToken::new()).await
}

/// Shared body for the two P-Chain boot entrypoints. When `include_self_beacon`
/// is `true` the chain boots with the single self node as its frontier-agreement
/// beacon (stalls at `Bootstrapping`, awaiting frontier replies the in-process
/// `RecordingSender` never delivers); when `false` the beacon set is empty and
/// the bootstrapper runs straight through to `NormalOp`.
async fn boot_pchain(
    network_id: u32,
    include_self_beacon: bool,
    base_db: Arc<dyn DynDatabase>,
    token: CancellationToken,
) -> Result<PChainBootHandle> {
    // Real P-Chain genesis for the network (the M8-complete embedded source).
    let (genesis_bytes, avax_asset_id) = ava_genesis::genesis_bytes(network_id, None)?;
    let genesis_id = ava_platformvm::genesis::genesis_id(&genesis_bytes);

    boot_chain(
        BootSpec {
            network_id,
            chain_id: ava_node::init::chain_manager::PLATFORM_CHAIN_ID,
            subnet_id: ava_types::constants::PRIMARY_NETWORK_ID,
            primary_alias: "P",
            avax_asset_id,
            genesis_id,
            include_self_beacon,
            loopback: false,
            data_dir: None,
        },
        ava_platformvm::vm::PlatformVm::new(),
        &genesis_bytes,
        base_db,
        token,
    )
    .await
}

/// Materializes the **real `ava_avm::AvmVm`** from the **production AVM genesis**
/// (the `CreateChainTx::genesis_data` the P-Chain genesis carries; parseable
/// since M5.f4 `AvmVm::initialize` ports `initGenesis` + `Linearize`), drives it
/// through the same [`create_snowman_chain`] pipeline as the P-Chain, starts the
/// handler, and returns a [`PChainBootHandle`]. Booted as a solo node (empty
/// beacons ⇒ `Bootstrapping → NormalOp` short-circuit), so a queued X-Chain
/// reaches `NormalOp` exactly as the P-Chain does (M9.15 X-dispatch).
///
/// `genesis_bytes` is the queued chain's genesis data (the dispatcher forwards
/// `ChainParameters::genesis_data`); the chain context's `avax_asset_id` is the
/// index-0 genesis asset id, and the handle's `genesis_id` is the Cortina
/// stop-vertex id the genesis block linearizes off (from the upgrade config — Go
/// `Upgrades.CortinaXChainStopVertexID`, the same source `AvmVm::initialize`
/// reads, not the inner Snowman block id it computes during `initialize`).
///
/// # Errors
/// Propagates genesis-parse / DB / VM-init / consensus-construction / identity /
/// timeout-manager failures.
pub async fn boot_xchain(
    network_id: u32,
    chain_id: Id,
    subnet_id: Id,
    genesis_bytes: &[u8],
    base_db: Arc<dyn DynDatabase>,
    token: CancellationToken,
) -> Result<PChainBootHandle> {
    // The AVAX asset id is the index-0 genesis asset (specs 09 §1); the X-Chain
    // genesis block linearizes off the Cortina stop-vertex id from the upgrade
    // config (the same value `AvmVm::initialize` uses), not the genesis bytes.
    let avax_asset_id = ava_genesis::avax_asset_id(genesis_bytes)?;
    let genesis_id = ava_version::upgrade::get_config(network_id).cortina_x_chain_stop_vertex_id;
    boot_chain(
        BootSpec {
            network_id,
            chain_id,
            subnet_id,
            primary_alias: "X",
            avax_asset_id,
            genesis_id,
            include_self_beacon: false,
            loopback: false,
            data_dir: None,
        },
        ava_avm::vm::AvmVm::new(),
        genesis_bytes,
        base_db,
        token,
    )
    .await
}

/// Boots the real [`ava_evm::vm::EvmVm`] from the queued production C-Chain
/// genesis (the genesis `CreateChainTx`'s `genesis_data`, a coreth
/// `core.Genesis` JSON) through the same solo-node pipeline as P/X, to
/// `NormalOp` (M9.15 C-Chain dispatch). The VM is built via
/// [`EvmVm::from_genesis`](ava_evm::vm::EvmVm::from_genesis) (the M6.8 genesis
/// wiring); its Firewood state db is opened in a fresh scratch dir that the
/// returned handle owns (dropping it would delete the state db under the VM).
///
/// # Errors
/// Propagates a scratch-dir / genesis-parse / Firewood-open / consensus boot
/// failure.
pub async fn boot_cchain(
    network_id: u32,
    chain_id: Id,
    subnet_id: Id,
    genesis_bytes: &[u8],
    base_db: Arc<dyn DynDatabase>,
    token: CancellationToken,
) -> Result<PChainBootHandle> {
    // The C-Chain Firewood state db lives in an owned scratch dir kept alive by
    // the boot handle for the running VM's lifetime. (This is the EVM *state*
    // trie store; the snowman/proposervm consensus metadata still persists into
    // the shared `base_db` like every other chain.)
    let data_dir = tempfile::tempdir()?;
    let (vm, genesis_id) =
        ava_evm::vm::EvmVm::from_genesis(network_id, data_dir.path(), genesis_bytes)?;
    boot_chain(
        BootSpec {
            network_id,
            chain_id,
            subnet_id,
            primary_alias: "C",
            // The C-Chain's EVM genesis carries no AVAX asset id (AVAX is the
            // P-Chain's native asset); the in-process atomic mempool is not
            // exercised during boot, so EMPTY matches `EvmVm::new`'s mempool seed.
            avax_asset_id: Id::EMPTY,
            genesis_id,
            include_self_beacon: false,
            loopback: false,
            data_dir: Some(data_dir),
        },
        vm,
        genesis_bytes,
        base_db,
        token,
    )
    .await
}

/// **Test seam (M9.15 STEP (m) — engine-driven block issuance).** Boot a single
/// in-process Snowman chain around a caller-supplied inner VM, over a
/// caller-supplied base db, **with the self-loopback installed**. The returned
/// [`PChainBootHandle::vm_tx`] drives engine-side block building: sending
/// [`VmEvent::PendingTxs`](ava_vm::vm::VmEvent::PendingTxs) makes the running
/// engine `build_block`, issue it, and — because the loopback closes the
/// `k=1`/`β=1` poll — accept it through the genuine consensus path. Solo node
/// (empty beacons), so the bootstrapper short-circuits straight to `NormalOp`.
///
/// # Errors
/// Propagates a VM-init / consensus-boot failure.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn boot_chain_with_loopback<V>(
    network_id: u32,
    chain_id: Id,
    subnet_id: Id,
    primary_alias: &'static str,
    avax_asset_id: Id,
    genesis_id: Id,
    inner_vm: V,
    genesis_bytes: Vec<u8>,
    base_db: Arc<dyn DynDatabase>,
) -> Result<PChainBootHandle>
where
    V: ava_vm::block::ChainVm + 'static,
{
    boot_chain(
        BootSpec {
            network_id,
            chain_id,
            subnet_id,
            primary_alias,
            avax_asset_id,
            genesis_id,
            include_self_beacon: false,
            loopback: true,
            data_dir: None,
        },
        inner_vm,
        &genesis_bytes,
        base_db,
        CancellationToken::new(),
    )
    .await
}

/// **Test seam (M9.15 SAE in-process dispatch).** Boot a single in-process
/// Snowman chain around a caller-supplied inner consensus VM through the same
/// [`BootSpec`]/[`boot_chain`] core the startup-boot paths use, with the
/// self-loopback **off** (`loopback: false`). As a solo node (empty beacons),
/// the bootstrapper short-circuits `Bootstrapping → NormalOp` without any poll
/// — so this seam proves an arbitrary [`ChainVm`](ava_vm::block::ChainVm)
/// dispatches and runs to `NormalOp` through the genuine consensus pipeline, no
/// issuance required.
///
/// This is the sibling of [`boot_chain_with_loopback`]; the two differ only in
/// the `loopback` flag. It exists so the SAE in-process boot test can drive a
/// real `ava_saevm_core::Vm` (wrapped via `ava_saevm_adaptor::convert`) through
/// the same pipeline P/X/C use, without yet adding a production SAE dispatch
/// branch (the production `BlockBuilderSeam`/`ExecutorSeam` wiring is
/// M7.21/M7.26).
///
/// # Errors
/// Propagates a VM-init / consensus-boot failure.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn boot_generic_chain<V>(
    network_id: u32,
    chain_id: Id,
    subnet_id: Id,
    primary_alias: &'static str,
    avax_asset_id: Id,
    genesis_id: Id,
    inner_vm: V,
    genesis_bytes: Vec<u8>,
    base_db: Arc<dyn DynDatabase>,
) -> Result<PChainBootHandle>
where
    V: ava_vm::block::ChainVm + 'static,
{
    boot_chain(
        BootSpec {
            network_id,
            chain_id,
            subnet_id,
            primary_alias,
            avax_asset_id,
            genesis_id,
            include_self_beacon: false,
            loopback: false,
            data_dir: None,
        },
        inner_vm,
        &genesis_bytes,
        base_db,
        CancellationToken::new(),
    )
    .await
}

/// The chain-identity + boot-mode inputs the generic [`boot_chain`] core needs;
/// the VM, genesis bytes, and cancellation token are passed alongside.
struct BootSpec {
    network_id: u32,
    chain_id: Id,
    subnet_id: Id,
    primary_alias: &'static str,
    avax_asset_id: Id,
    genesis_id: Id,
    include_self_beacon: bool,
    /// Install the self-loopback on the [`RecordingSender`] so a solo node's
    /// consensus poll self-resolves and a self-built block is accepted through
    /// the genuine engine path (M9.15 STEP (m)). `false` (the default for the
    /// startup-boot paths) keeps the record-and-drop behavior unchanged.
    loopback: bool,
    /// The chain's on-disk state dir, moved into the boot handle to outlive the
    /// running VM (the C-Chain Firewood db). `None` for in-memory chains (P/X).
    data_dir: Option<tempfile::TempDir>,
}

/// Generic in-process chain boot: wires the network-facing loopback impls (the
/// recording sender, no-op app sender, fixed single-validator state, real
/// router over a clock-injected adaptive-timeout manager), drives `inner_vm`
/// through the full [`create_snowman_chain`] pipeline under `spec`, starts the
/// handler, and returns the [`PChainBootHandle`]. Shared by [`boot_pchain`] and
/// [`boot_xchain`] (M9.15 X/C dispatch); generic over the inner [`ChainVm`].
async fn boot_chain<V>(
    spec: BootSpec,
    inner_vm: V,
    genesis_bytes: &[u8],
    base_db: Arc<dyn DynDatabase>,
    token: CancellationToken,
) -> Result<PChainBootHandle>
where
    V: ava_vm::block::ChainVm + 'static,
{
    let reg = Registry::new();

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

    // A per-network ChainContext (network id + fork schedule from the chosen
    // network), so the VM initializes with the production identity surface.
    let chain_ctx = Arc::new(ava_snow::ChainContext {
        network_id: spec.network_id,
        subnet_id: spec.subnet_id,
        chain_id: spec.chain_id,
        node_id,
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(spec.network_id),
        x_chain_id: Id::EMPTY,
        c_chain_id: Id::EMPTY,
        avax_asset_id: spec.avax_asset_id,
        chain_data_dir: std::path::PathBuf::new(),
    });

    let sender = Arc::new(RecordingSender::default());
    let app_sender: Arc<dyn AppSender> = Arc::new(NoopAppSender);

    // Frontier-agreement beacon set: the single self node (weight 1) when
    // bootstrapping from a peer, or empty for a solo node that short-circuits
    // straight to NormalOp.
    let mut beacons = BTreeMap::new();
    if spec.include_self_beacon {
        beacons.insert(node_id, 1u64);
    }

    let chain = create_snowman_chain(
        &token,
        spec.chain_id,
        spec.subnet_id,
        single_node_params(),
        // The node's one persistent base db (Go's model: a single base DB,
        // `prefixdb`-namespaced per chain by `build_db_stack`). Bridged through
        // the object-safe `DynDb` so the generic `create_snowman_chain` runs
        // over the dynamically-chosen backend without making boot generic.
        ava_node::init::database::DynDb::new(base_db),
        spec.primary_alias,
        chain_ctx,
        clock,
        validator_state,
        Some(identity),
        inner_vm,
        Vec::new(),
        genesis_bytes,
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
    let vm_tx = chain.vm_tx.clone();
    let last_accepted_height = chain.last_accepted_height;

    // Opt-in self-loopback: route the engine's own poll path (push/pull query +
    // chits) back into this chain's handler as inbound ops from the self node, so
    // a `k=1`/`β=1` poll on a self-built block self-resolves and the block is
    // accepted through the genuine consensus path (M9.15 STEP (m)). Installed
    // before the handler starts; the startup-boot paths leave it off.
    if spec.loopback {
        sender.install_loopback(node_id, chain.sink.clone());
    }

    // Start the handler task: it activates the initial (`Bootstrapping`) engine,
    // which flips `ctx.state` and broadcasts `GetAcceptedFrontier`.
    let join = chain.handler.start();

    Ok(PChainBootHandle {
        ctx,
        sender,
        join,
        token,
        genesis_id: spec.genesis_id,
        last_accepted_height,
        beacons: beacon_nodes,
        vm_tx,
        _sink: chain.sink,
        _data_dir: spec.data_dir,
    })
}

// ---------------------------------------------------------------------------
// Production chain creator (M9.15 step (a) — drive the queued chains)
// ---------------------------------------------------------------------------

/// The production chain creator: construct and drive every chain that step-26
/// `init_chains` *queued* on the [`AssemblyChainManager`], reflecting each
/// running chain's consensus context into the manager's `is_bootstrapped`
/// (the value `info.isBootstrapped` serves).
///
/// **Scope (M9.15 X/C dispatch):** this slice dispatches the **platform chain**
/// (`vm_id == platform_vm_id()`) **and the X-Chain** (`vm_id == avm_id()`),
/// booting each as a solo node so the bootstrapper short-circuits
/// `Bootstrapping → NormalOp` via the empty-beacon path — the proven
/// [`boot_in_process_pchain_to_normalop`] template, generalized over the inner
/// VM ([`boot_chain`]). Each booted chain is registered with the manager (so
/// `running_chains()` counts it and `shutdown()` drains it) under a token
/// derived from the node's root subnet token, and a live reporter is installed
/// so `is_bootstrapped(chain_id)` reflects the engine reaching `NormalOp`.
///
/// The C-Chain (`vm_id == evm_id()`) now boots too, through the
/// [`EvmVm::from_genesis`](ava_evm::vm::EvmVm::from_genesis) construction seam
/// (M6.8 genesis wiring), so a live solo node flips `is_bootstrapped` true for
/// all three standard chains.
///
/// **Documented deferrals (the larger chains milestone):** SAE VM dispatch and
/// the real ava-network-backed `Sender` for multi-node frontier exchange — both
/// tracked in plan/M9.15.
///
/// All booted chains share **one persistent base db** (`base_db`), namespaced
/// per chain by `build_db_stack`'s `prefixdb(chain_id)` — Go's model (one base
/// DB, a prefixed sub-db per chain). This variant of [`run_queued_chains`] takes
/// that base db explicitly so the live `avalanchers` node can thread its real
/// `node.db` through; the no-arg [`run_queued_chains`] wrapper supplies a fresh
/// ephemeral [`MemDb`] for tests / non-persistent runs.
///
/// `init_chains` (specs/12 §2.2) queues the X- and C-Chains live off the genesis
/// `CreateChainTx`s — each carries the production `genesis_data` `AvmVm`/`EvmVm`
/// parse — so a live solo node flips `is_bootstrapped(X)` and `(C)` too.
///
/// # Errors
/// Propagates a chain boot failure (genesis / DB / VM-init / consensus /
/// identity / timeout-manager).
pub async fn run_queued_chains(
    manager: &Arc<ava_node::init::chain_manager::AssemblyChainManager>,
    network_id: u32,
) -> Result<Vec<PChainBootHandle>> {
    run_queued_chains_with_db(manager, network_id, fresh_mem_db()).await
}

/// Like [`run_queued_chains`], but driving every queued chain over the
/// caller-supplied persistent `base_db` (shared across chains, prefixed per
/// chain by `build_db_stack`). This is the persistence-bearing path the live
/// node uses; see [`run_queued_chains`] for the dispatch semantics.
///
/// # Errors
/// Propagates a chain boot failure (genesis / DB / VM-init / consensus /
/// identity / timeout-manager).
pub async fn run_queued_chains_with_db(
    manager: &Arc<ava_node::init::chain_manager::AssemblyChainManager>,
    network_id: u32,
    base_db: Arc<dyn DynDatabase>,
) -> Result<Vec<PChainBootHandle>> {
    use ava_node::init::chain_manager::{avm_id, evm_id, platform_vm_id};
    use ava_snow::EngineState;

    // The node's root subnet token (the cancellation root the manager derives
    // per-subnet / per-chain tokens from; 17 §4.1).
    let root_subnet_token = CancellationToken::new();
    let mut handles = Vec::new();

    for params in manager.queued_chains() {
        // Register the chain first so its handler runs under the
        // manager-derived token (subnet shutdown then reaches it). The task
        // tracker is unused in-process (the handler joins via its JoinHandle).
        // We only register a chain we actually boot, so the per-`vm_id` branch
        // does its own registration after deciding to dispatch.
        let handle = if params.vm_id == platform_vm_id() {
            let (chain_token, _tasks) =
                manager.register_chain(params.id, params.subnet_id, &root_subnet_token);
            // Boot the real PlatformVm as a solo node (empty beacons ⇒
            // Bootstrapping → NormalOp). All chains share the one base db,
            // prefixdb-namespaced per chain by `build_db_stack`.
            boot_pchain(network_id, false, Arc::clone(&base_db), chain_token).await?
        } else if params.vm_id == avm_id() {
            let (chain_token, _tasks) =
                manager.register_chain(params.id, params.subnet_id, &root_subnet_token);
            // Boot the real AvmVm from the queued production X genesis through
            // the same solo-node pipeline.
            boot_xchain(
                network_id,
                params.id,
                params.subnet_id,
                &params.genesis_data,
                Arc::clone(&base_db),
                chain_token,
            )
            .await?
        } else if params.vm_id == evm_id() {
            let (chain_token, _tasks) =
                manager.register_chain(params.id, params.subnet_id, &root_subnet_token);
            // Boot the real EvmVm from the queued production C genesis through the
            // same solo-node pipeline (M6.8 genesis wiring via EvmVm::from_genesis).
            boot_cchain(
                network_id,
                params.id,
                params.subnet_id,
                &params.genesis_data,
                Arc::clone(&base_db),
                chain_token,
            )
            .await?
        } else {
            // SAE (`saevm_id()`) and any unknown VM are not dispatched here yet.
            //
            // The in-process boot machinery itself is SAE-ready — a real
            // `ava_saevm_core::Vm` wrapped via `ava_saevm_adaptor::convert`
            // already runs to `NormalOp` through `boot_generic_chain` (see the
            // `saevm_chain_boots_to_normalop` test). What is still missing for a
            // *production* boot is:
            //   1. no production `BlockBuilderSeam`/`ExecutorSeam` wiring exists
            //      yet (the concrete seams are M7.21/M7.26; only the testutil
            //      fakes can construct a live `Vm` today), and there is no
            //      genesis-bytes → SAE `Vm` materialization
            //      (`BaseVm::initialize` is a stubbed TODO), so we cannot build
            //      a real SAE VM from a queued chain's `genesis_data`; and
            //   2. the `local` network genesis queues no SAE chain (only X/C),
            //      so on a solo `local` node this branch is unreachable in
            //      practice — there is nothing to skip.
            // Faking a production SAE boot here would be dishonest; the branch
            // stays a warn until the seams + genesis materialization land.
            tracing::warn!(
                chain_id = %params.id,
                vm_id = %params.vm_id,
                "skipping queued chain: SAE / unknown VM production dispatch not yet wired \
                 (SAE seams M7.21/M7.26 + genesis materialization pending; no SAE chain in \
                 `local` genesis ⇒ unreachable on a solo local node)"
            );
            continue;
        };

        // Reflect the running engine's consensus context into the manager:
        // is_bootstrapped(chain) becomes a live read of `ctx.state == NormalOp`.
        let ctx = Arc::clone(&handle.ctx);
        manager.set_bootstrapped_reporter(
            params.id,
            Box::new(move || matches!(**ctx.state.load(), EngineState::NormalOp)),
        );

        handles.push(handle);
    }

    Ok(handles)
}

/// The node-startup chain-creator entrypoint the `avalanchers` binary's
/// `dispatch` path calls (M9.15 live-dispatch wiring): drive the chains that
/// step-26 `init_chains` *queued* on `manager` so a **live** `avalanchers`
/// process reflects each running engine through `info.isBootstrapped` (via the
/// per-chain reporter [`run_queued_chains`] installs on `manager`).
///
/// `beaconless` gates the solo short-circuit. A node with **no** configured
/// bootstrap beacons boots its critical chains straight to `NormalOp` — the
/// empty-beacon `Bootstrapping → NormalOp` path, exactly what a Go
/// `--network-id=local` node with no default beacons does — so a solo node's
/// `info.isBootstrapped(P)` flips `true` at startup. A node **with** configured
/// beacons must instead reach `NormalOp` by actually connecting to and
/// bootstrapping from peers over the real ava-network-backed `Sender` (the
/// documented **live arm**); this therefore **skips** it and leaves
/// `info.isBootstrapped` honestly `false` until that path lands, rather than
/// falsely short-circuiting a node that has not bootstrapped.
///
/// Returns the live [`PChainBootHandle`]s — the caller must keep them alive for
/// the node's lifetime (each booted chain is also registered with `manager`, so
/// node shutdown step 5 cancels and drains it).
///
/// **Documented deferrals (unchanged):** the booted chains still use
/// [`run_queued_chains`]'s own in-process router/loopback `Sender`; the real
/// multi-node `Sender` remains the larger chains-milestone work (plan/M9.15).
/// The persistent base db **is** now threaded — [`drive_startup_chains_with_db`]
/// takes the assembled `Node`'s real `Arc<dyn DynDatabase>` so chain state
/// survives across restarts; this no-db wrapper supplies a fresh ephemeral
/// [`MemDb`] for tests.
///
/// # Errors
/// Propagates a chain boot failure from [`run_queued_chains`].
pub async fn drive_startup_chains(
    manager: &Arc<ava_node::init::chain_manager::AssemblyChainManager>,
    network_id: u32,
    beaconless: bool,
) -> Result<Vec<PChainBootHandle>> {
    drive_startup_chains_with_db(manager, network_id, beaconless, fresh_mem_db()).await
}

/// Like [`drive_startup_chains`], but driving the queued chains over the
/// assembled node's real persistent `base_db` (so consensus / VM state survives
/// a restart). This is the path the live `avalanchers` binary calls with
/// `node.db`; see [`drive_startup_chains`] for the beacon-gating semantics.
///
/// # Errors
/// Propagates a chain boot failure from [`run_queued_chains_with_db`].
pub async fn drive_startup_chains_with_db(
    manager: &Arc<ava_node::init::chain_manager::AssemblyChainManager>,
    network_id: u32,
    beaconless: bool,
    base_db: Arc<dyn DynDatabase>,
) -> Result<Vec<PChainBootHandle>> {
    if !beaconless {
        tracing::info!(
            network_id,
            "node has configured bootstrap beacons; deferring chain creation to the live-Sender bootstrap path (M9.15 live arm)"
        );
        return Ok(Vec::new());
    }
    run_queued_chains_with_db(manager, network_id, base_db).await
}
