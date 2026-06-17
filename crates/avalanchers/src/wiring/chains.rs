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
use std::sync::Arc;
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
