// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared test scaffolding for the `ava-chains` pipeline + differential tests.
//!
//! A recording [`Sender`] and an in-memory loopback [`Cluster`] of
//! `SnowmanEngine`s driving the **fully-wrapped** chain-pipeline VM (the maximal
//! `inner → tracedvm → proposervm → metervm → tracedvm → change-notifier` stack
//! built by `ava_chains::create_chain::wrap_snowman_vm`). This mirrors the
//! `ava-engine` cluster harness used by `prop::consensus_liveness`, but drives
//! the wrapped VM through the pipeline rather than a bare `TestVm`, so it
//! re-asserts finalization end-to-end (the M3.27 differential exit gate).
//!
//! Time is virtual; the harness is purely message-driven (no wall-clock sleeps).

#![allow(dead_code)]
// Test scaffolding: index/arithmetic/expect on known-small fixtures are clearer
// than checked variants and match the `ava-engine` cluster harness conventions.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::expect_used
)]

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use ava_chains::create_chain::{WrappedVm, wrap_snowman_vm};
use ava_crypto::staking;
use ava_database::MemDb;
use ava_engine::common::sender::{SendConfig, Sender};
use ava_engine::error::Result as EngineResult;
use ava_engine::snowman::engine::{Config, SnowmanEngine};
use ava_proposervm::{BlockSigner, StakingIdentity};
use ava_snow::snowball::{Parameters, SnowballFactory};
use ava_snow::snowman::Topological;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, MockClock};
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use ava_validators::{DefaultManager, ValidatorManager};
use ava_vm::testutil::{NoopAppSender, TestVm, test_chain_context};
use ava_vm::{AppSender, ChainVm, Vm};
use prometheus::Registry;

/// The fully-wrapped VM the cluster drives (the chain-pipeline stack over the
/// in-memory `TestVm`).
pub type ClusterVm = WrappedVm<TestVm, FixedState>;

/// A fixed single-validator-set `ValidatorState` for the proposervm windower.
#[derive(Clone)]
pub struct FixedState {
    pub set: BTreeMap<NodeId, GetValidatorOutput>,
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

/// Generates a staking cert + an ECDSA P-256 signer over the header bytes.
#[must_use]
pub fn staking_identity() -> (StakingIdentity, NodeId) {
    let (cert_pem, key_pem) = staking::new_cert_and_key_bytes().expect("gen cert");
    let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .next()
        .expect("a cert block")
        .expect("valid cert pem")
        .to_vec();
    let node_id = staking::node_id_from_cert(&cert_der);

    let key_pair = rcgen::KeyPair::from_pem(&key_pem).expect("parse key pem");
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
    (
        StakingIdentity {
            certificate: cert_der,
            signer,
        },
        node_id,
    )
}

/// A recorded outbound message (the subset the cluster routes).
#[derive(Clone, Debug)]
pub enum Sent {
    PullQuery {
        nodes: Vec<NodeId>,
        req: u32,
        id: Id,
        height: u64,
    },
    PushQuery {
        nodes: Vec<NodeId>,
        req: u32,
        id: Id,
        height: u64,
    },
    Chits {
        node: NodeId,
        req: u32,
        preferred: Id,
        preferred_at_height: Id,
        accepted: Id,
        accepted_height: u64,
    },
    Get {
        node: NodeId,
        req: u32,
        id: Id,
    },
    Other,
}

/// A [`Sender`] recording outbound messages for loopback routing.
#[derive(Default)]
pub struct RecordingSender {
    pub log: Mutex<Vec<Sent>>,
}

impl RecordingSender {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn drain(&self) -> Vec<Sent> {
        std::mem::take(&mut self.log.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn push(&self, s: Sent) {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).push(s);
    }
}

fn sorted(nodes: &HashSet<NodeId>) -> Vec<NodeId> {
    let mut v: Vec<NodeId> = nodes.iter().copied().collect();
    v.sort();
    v
}

fn block_id(bytes: &[u8]) -> Id {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let d: [u8; 32] = h.finalize().into();
    Id::from(d)
}

#[async_trait]
impl Sender for RecordingSender {
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

    fn send_get(&self, node: NodeId, req: u32, container_id: Id) {
        self.push(Sent::Get {
            node,
            req,
            id: container_id,
        });
    }
    fn send_get_ancestors(&self, _node: NodeId, _req: u32, _container_id: Id) {}
    fn send_put(&self, _node: NodeId, _req: u32, _container: Vec<u8>) {
        self.push(Sent::Other);
    }
    fn send_ancestors(&self, _node: NodeId, _req: u32, _containers: Vec<Vec<u8>>) {}

    fn send_push_query(
        &self,
        nodes: &HashSet<NodeId>,
        req: u32,
        container: Vec<u8>,
        requested_height: u64,
    ) {
        self.push(Sent::PushQuery {
            nodes: sorted(nodes),
            req,
            id: block_id(&container),
            height: requested_height,
        });
    }
    fn send_pull_query(
        &self,
        nodes: &HashSet<NodeId>,
        req: u32,
        container_id: Id,
        requested_height: u64,
    ) {
        self.push(Sent::PullQuery {
            nodes: sorted(nodes),
            req,
            id: container_id,
            height: requested_height,
        });
    }
    fn send_chits(
        &self,
        node: NodeId,
        req: u32,
        preferred: Id,
        preferred_at_height: Id,
        accepted: Id,
        accepted_height: u64,
    ) {
        self.push(Sent::Chits {
            node,
            req,
            preferred,
            preferred_at_height,
            accepted,
            accepted_height,
        });
    }

    async fn send_app_request(
        &self,
        _nodes: &HashSet<NodeId>,
        _req: u32,
        _bytes: Vec<u8>,
    ) -> EngineResult<()> {
        Ok(())
    }
    async fn send_app_response(
        &self,
        _node: NodeId,
        _req: u32,
        _bytes: Vec<u8>,
    ) -> EngineResult<()> {
        Ok(())
    }
    async fn send_app_error(
        &self,
        _node: NodeId,
        _req: u32,
        _code: i32,
        _msg: &str,
    ) -> EngineResult<()> {
        Ok(())
    }
    async fn send_app_gossip(&self, _cfg: SendConfig, _bytes: Vec<u8>) -> EngineResult<()> {
        Ok(())
    }
}

/// Builds an initialized fully-wrapped pipeline VM around a fresh `TestVm`,
/// returning the wrapped VM and its genesis (last-accepted) id.
pub async fn init_wrapped_vm(
    token: &CancellationToken,
    node_id: NodeId,
    identity: StakingIdentity,
    reg: &Registry,
) -> (ClusterVm, Id) {
    let ctx = test_chain_context();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(std::time::UNIX_EPOCH));
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
    let vm_db = Arc::new(MemDb::new());

    let mut vm: ClusterVm = wrap_snowman_vm(
        TestVm::new(),
        "P",
        Arc::clone(&ctx),
        clock,
        validator_state,
        vm_db.clone(),
        Some(identity),
        reg,
        Arc::new(|| {}),
    )
    .expect("wrap vm");

    let app_sender: Arc<dyn AppSender> = Arc::new(NoopAppSender);
    vm.initialize(
        token,
        Arc::clone(&ctx),
        vm_db,
        b"genesis",
        b"",
        b"",
        Vec::new(),
        app_sender,
    )
    .await
    .expect("initialize wrapped vm");
    let genesis = vm.last_accepted(token).await.expect("genesis");
    (vm, genesis)
}

/// One cluster node: its engine + recording sender + node id.
pub struct Node {
    pub id: NodeId,
    pub engine: SnowmanEngine<ClusterVm, RecordingSender, DefaultManager>,
    pub sender: Arc<RecordingSender>,
}

/// An in-memory cluster of `SnowmanEngine`s over the wrapped pipeline VM, wired
/// so each engine's outbound queries loop back as inbound `pull_query`/`chits`.
pub struct Cluster {
    pub nodes: Vec<Node>,
    pub genesis: Id,
    token: CancellationToken,
}

impl Cluster {
    /// Builds an `n`-node cluster over `params`. Each node runs the full chain
    /// pipeline (wrapped VM) and represents one equally-weighted validator.
    pub async fn new(n: usize, params: Parameters) -> Self {
        let token = CancellationToken::new();

        // Per-node staking identities + the shared validator set.
        let mut identities = Vec::with_capacity(n);
        let mgr = Arc::new(DefaultManager::new());
        for _ in 0..n {
            let (identity, node_id) = staking_identity();
            mgr.add_staker(Id::EMPTY, node_id, None, Id::EMPTY, 1)
                .expect("add staker");
            identities.push((identity, node_id));
        }

        let mut nodes = Vec::with_capacity(n);
        let mut genesis = Id::EMPTY;
        for (identity, node_id) in identities {
            // Each node is its own chain instance with its own metrics registry.
            let reg = Registry::new();
            let (vm, g) = init_wrapped_vm(&token, node_id, identity, &reg).await;
            genesis = g;
            let sender = RecordingSender::new();
            let consensus =
                Topological::new_default(SnowballFactory, params, g, 0).expect("topological");
            let cfg = Config {
                subnet_id: Id::EMPTY,
                params,
                vm: Arc::new(AsyncMutex::new(vm)),
                sender: Arc::clone(&sender),
                validators: Arc::clone(&mgr),
                token: token.clone(),
            };
            let engine = SnowmanEngine::new(cfg, Box::new(consensus));
            nodes.push(Node {
                id: node_id,
                engine,
                sender,
            });
        }
        Self {
            nodes,
            genesis,
            token,
        }
    }

    /// Issues `block_bytes` (a child of an issued/accepted block) into every
    /// engine via a `Put` from the first node.
    pub async fn issue_block_all(&mut self, block_bytes: &[u8]) {
        let provider = self.nodes[0].id;
        for node in &mut self.nodes {
            node.engine
                .put(provider, u32::MAX, block_bytes)
                .await
                .expect("put block");
        }
    }

    /// Drives one synchronized poll wave (see the `ava-engine` harness): every
    /// outstanding pull-query is delivered to every recipient, whose chits are
    /// fed back to the querier (driving `record_poll` → repoll). Returns the
    /// number of queries routed.
    pub async fn run_round(&mut self) -> usize {
        let node_ids: Vec<NodeId> = self.nodes.iter().map(|n| n.id).collect();
        let mut queries: Vec<(usize, u32, Id, u64)> = Vec::new();
        for (i, node) in self.nodes.iter().enumerate() {
            for s in node.sender.drain() {
                match s {
                    Sent::PullQuery {
                        req, id, height, ..
                    }
                    | Sent::PushQuery {
                        req, id, height, ..
                    } => queries.push((i, req, id, height)),
                    _ => {}
                }
            }
        }

        for (querier_idx, req, container_id, height) in &queries {
            let querier_node = node_ids[*querier_idx];
            #[allow(clippy::needless_range_loop)]
            for recipient_idx in 0..self.nodes.len() {
                self.nodes[recipient_idx]
                    .engine
                    .pull_query(querier_node, *req, *container_id, *height)
                    .await
                    .expect("pull_query");
                let chit = {
                    let mut log = self.nodes[recipient_idx]
                        .sender
                        .log
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let pos = log
                        .iter()
                        .rposition(|s| matches!(s, Sent::Chits { req: r, .. } if r == req));
                    pos.map(|p| match log.remove(p) {
                        Sent::Chits {
                            preferred,
                            preferred_at_height,
                            accepted,
                            accepted_height,
                            ..
                        } => (preferred, preferred_at_height, accepted, accepted_height),
                        _ => unreachable!(),
                    })
                };
                if let Some((pref, pref_at_h, accepted, accepted_h)) = chit {
                    let recipient_node = node_ids[recipient_idx];
                    self.nodes[*querier_idx]
                        .engine
                        .chits(recipient_node, *req, pref, pref_at_h, accepted, accepted_h)
                        .await
                        .expect("chits");
                }
            }
        }
        queries.len()
    }

    /// Whether every node has accepted `id` (its consensus last-accepted).
    #[must_use]
    pub fn all_accepted(&self, id: Id) -> bool {
        self.nodes
            .iter()
            .all(|n| n.engine.consensus_last_accepted().0 == id)
    }

    /// The (id, height) every node reports as its consensus last-accepted, or
    /// `None` if the nodes disagree (a fork — must never happen).
    #[must_use]
    pub fn agreed_last_accepted(&self) -> Option<(Id, u64)> {
        let first = self.nodes.first()?.engine.consensus_last_accepted();
        if self
            .nodes
            .iter()
            .all(|n| n.engine.consensus_last_accepted() == first)
        {
            Some(first)
        } else {
            None
        }
    }

    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}

/// The canonical bytes of a test block (`parent ‖ be64(height) ‖ payload`).
#[must_use]
pub fn encode_block(parent: Id, height: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(40 + payload.len());
    bytes.extend_from_slice(&parent.to_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// The id of an encoded test block (`sha256(bytes)`).
#[must_use]
pub fn block_id_of(bytes: &[u8]) -> Id {
    block_id(bytes)
}
