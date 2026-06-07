// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared test scaffolding for the Snowman engine integration + property tests:
//! a recording mock [`Sender`], helpers to build a [`SnowmanEngine`] over the
//! in-memory test VM + real `Topological` consensus + `DefaultManager` sampler,
//! and a loopback "cluster" of engines for the liveness/preference properties.

#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use ava_engine::common::sender::Sender;
use ava_engine::error::Result as EngineResult;
use ava_engine::snowman::engine::{Config, SnowmanEngine};
use ava_snow::snowball::{DEFAULT_PARAMETERS, Parameters, SnowballFactory};
use ava_snow::snowman::Topological;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::{DefaultManager, ValidatorManager};
use ava_vm::block::ChainVm;
use ava_vm::testutil::TestVm;

/// A single recorded outbound message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    Get { node: NodeId, req: u32, id: Id },
    GetAncestors { node: NodeId, req: u32, id: Id },
    Put { node: NodeId, req: u32 },
    Ancestors { node: NodeId, req: u32, n: usize },
    PullQuery { nodes: Vec<NodeId>, req: u32, id: Id, height: u64 },
    PushQuery { nodes: Vec<NodeId>, req: u32, id: Id, height: u64 },
    Chits {
        node: NodeId,
        req: u32,
        preferred: Id,
        preferred_at_height: Id,
        accepted: Id,
        accepted_height: u64,
    },
    AcceptedFrontier { node: NodeId, req: u32, id: Id },
    Accepted { node: NodeId, req: u32, ids: Vec<Id> },
    GetAcceptedFrontier { nodes: Vec<NodeId>, req: u32 },
    GetAccepted { nodes: Vec<NodeId>, req: u32, ids: Vec<Id> },
}

/// A [`Sender`] that records every outbound message for assertions.
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

    pub fn snapshot(&self) -> Vec<Sent> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
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

#[async_trait::async_trait]
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

    fn send_get_accepted_frontier(&self, nodes: &HashSet<NodeId>, req: u32) {
        self.push(Sent::GetAcceptedFrontier {
            nodes: sorted(nodes),
            req,
        });
    }
    fn send_accepted_frontier(&self, node: NodeId, req: u32, container_id: Id) {
        self.push(Sent::AcceptedFrontier {
            node,
            req,
            id: container_id,
        });
    }
    fn send_get_accepted(&self, nodes: &HashSet<NodeId>, req: u32, ids: &[Id]) {
        self.push(Sent::GetAccepted {
            nodes: sorted(nodes),
            req,
            ids: ids.to_vec(),
        });
    }
    fn send_accepted(&self, node: NodeId, req: u32, ids: &[Id]) {
        self.push(Sent::Accepted {
            node,
            req,
            ids: ids.to_vec(),
        });
    }

    fn send_get(&self, node: NodeId, req: u32, container_id: Id) {
        self.push(Sent::Get {
            node,
            req,
            id: container_id,
        });
    }
    fn send_get_ancestors(&self, node: NodeId, req: u32, container_id: Id) {
        self.push(Sent::GetAncestors {
            node,
            req,
            id: container_id,
        });
    }
    fn send_put(&self, node: NodeId, req: u32, _container: Vec<u8>) {
        self.push(Sent::Put { node, req });
    }
    fn send_ancestors(&self, node: NodeId, req: u32, containers: Vec<Vec<u8>>) {
        self.push(Sent::Ancestors {
            node,
            req,
            n: containers.len(),
        });
    }

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
    async fn send_app_gossip(
        &self,
        _cfg: ava_engine::common::sender::SendConfig,
        _bytes: Vec<u8>,
    ) -> EngineResult<()> {
        Ok(())
    }
}

/// Builds a `DefaultManager` with `n` equally-weighted validators on the empty
/// subnet, returning the manager and the validator node ids.
#[must_use]
pub fn validators(n: usize) -> (Arc<DefaultManager>, Vec<NodeId>) {
    let mgr = Arc::new(DefaultManager::new());
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let node = NodeId::from([(i as u8).wrapping_add(1); 20]);
        mgr.add_staker(Id::EMPTY, node, None, Id::EMPTY, 1)
            .expect("add staker");
        ids.push(node);
    }
    (mgr, ids)
}

/// An initialized in-memory VM plus its genesis (last-accepted) id.
pub async fn init_vm(token: &CancellationToken) -> (TestVm, Id) {
    let vm = ava_vm::testutil::init_test_vm(token).await.expect("init vm");
    let genesis = vm.last_accepted(token).await.expect("genesis");
    (vm, genesis)
}

/// Builds a `SnowmanEngine` over the supplied VM/sender/validators with a real
/// `Topological` consensus rooted at `genesis`.
pub fn build_engine(
    params: Parameters,
    vm: TestVm,
    sender: Arc<RecordingSender>,
    validators: Arc<DefaultManager>,
    genesis: Id,
    token: CancellationToken,
) -> SnowmanEngine<TestVm, RecordingSender, DefaultManager> {
    let consensus = Topological::new_default(SnowballFactory, params, genesis, 0)
        .expect("topological");
    let cfg = Config {
        subnet_id: Id::EMPTY,
        params,
        vm: Arc::new(AsyncMutex::new(vm)),
        sender,
        validators,
        token,
    };
    SnowmanEngine::new(cfg, Box::new(consensus))
}

/// The default consensus parameters used by the integration tests.
#[must_use]
pub fn default_params() -> Parameters {
    DEFAULT_PARAMETERS
}

/// Encodes the canonical bytes of a test block (`parent ++ be64(height) ++
/// payload`) so a `Put`/`PushQuery` can be synthesized without the VM.
#[must_use]
pub fn encode_block(parent: Id, height: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(40 + payload.len());
    bytes.extend_from_slice(&parent.to_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// The id of an encoded test block (`sha256(bytes)`), matching `TestBlock`.
#[must_use]
pub fn block_id(bytes: &[u8]) -> Id {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest: [u8; 32] = hasher.finalize().into();
    Id::from(digest)
}

/// One engine node in the cluster: its engine, its [`RecordingSender`], and its
/// node id (= the validator it represents).
pub struct Node {
    pub id: NodeId,
    pub engine: SnowmanEngine<TestVm, RecordingSender, DefaultManager>,
    pub sender: Arc<RecordingSender>,
}

/// An in-memory cluster of `SnowmanEngine`s sharing one validator set, wired so
/// that each engine's recorded outbound queries are looped back as inbound
/// `pull_query`/`chits` on the other engines (the mock Router). Time is virtual;
/// the harness is purely message-driven (no wall-clock sleeps).
pub struct Cluster {
    pub nodes: Vec<Node>,
    pub genesis: Id,
    token: CancellationToken,
}

impl Cluster {
    /// Builds a cluster of `n` engines over `params`, each with `n` equally-
    /// weighted validators (one per node).
    pub async fn new(n: usize, params: Parameters) -> Self {
        let token = CancellationToken::new();
        let (mgr, ids) = validators(n);
        let mut nodes = Vec::with_capacity(n);
        let mut genesis = Id::EMPTY;
        for &id in &ids {
            let (vm, g) = init_vm(&token).await;
            genesis = g;
            let sender = RecordingSender::new();
            let engine = build_engine(
                params,
                vm,
                sender.clone(),
                Arc::clone(&mgr),
                g,
                token.clone(),
            );
            nodes.push(Node {
                id,
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
    /// engine via a `Put` from the first node. The resulting outbound queries
    /// are **retained** so the first [`run_round`] can route them.
    pub async fn issue_block_all(&mut self, block_bytes: &[u8]) {
        let provider = self.nodes[0].id;
        for node in &mut self.nodes {
            node.engine
                .put(provider, u32::MAX, block_bytes)
                .await
                .expect("put block");
        }
    }

    /// Drives one synchronized poll wave: every pull-query currently outstanding
    /// (emitted by the prior `put`/repoll) is delivered to every recipient (which
    /// answers with chits), and those chits are fed back to the querier (driving
    /// `record_poll` → `set_preference` → the engine's own repoll, which emits the
    /// *next* round's pull-query). Returns the number of queries routed.
    ///
    /// The query set is snapshotted before any routing so the queries a node
    /// emits *during* this wave belong to the next wave. The chit-answer Chits are
    /// consumed from each recipient's log without disturbing its pending queries.
    pub async fn run_round(&mut self) -> usize {
        // Snapshot the currently-outstanding pull-queries, then clear all logs so
        // the queries emitted during this wave (the next wave) are captured fresh.
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
            // Index loop is required: the body mutably borrows
            // `self.nodes[recipient_idx]` while also reading `node_ids`.
            #[allow(clippy::needless_range_loop)]
            for recipient_idx in 0..self.nodes.len() {
                // The recipient answers the query (records a Chits to the querier).
                self.nodes[recipient_idx]
                    .engine
                    .pull_query(querier_node, *req, *container_id, *height)
                    .await
                    .expect("pull_query");
                // Consume only the Chits answering this request from the log,
                // leaving the recipient's own pending queries in place.
                let chit = {
                    let mut log = self.nodes[recipient_idx]
                        .sender
                        .log
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let pos = log.iter().rposition(|s| {
                        matches!(s, Sent::Chits { req: r, .. } if r == req)
                    });
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

    /// Whether every node has accepted `block_id` (its consensus last-accepted).
    #[must_use]
    pub fn all_accepted(&self, id: Id) -> bool {
        self.nodes
            .iter()
            .all(|n| n.engine.consensus_last_accepted().0 == id)
    }

    /// Whether every node reports `id` as its current preference.
    #[must_use]
    pub fn all_prefer(&self, id: Id) -> bool {
        self.nodes.iter().all(|n| n.engine.preference() == id)
    }

    /// The halt token (for shutdown tests).
    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}
