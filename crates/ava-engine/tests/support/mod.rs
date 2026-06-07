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
    PushQuery { nodes: Vec<NodeId>, req: u32, height: u64 },
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
        _container: Vec<u8>,
        requested_height: u64,
    ) {
        self.push(Sent::PushQuery {
            nodes: sorted(nodes),
            req,
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
