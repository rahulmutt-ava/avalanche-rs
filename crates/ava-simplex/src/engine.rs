// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Feature-gated Simplex engine **stub** (`engine.go`), specs 06 §8.
//!
//! Gated behind `#[cfg(feature = "simplex")]` and **off by default**. The full
//! round-based BFT state machine (epoch/round, round-robin leader, view-change
//! timeouts, QC accumulation) is deferred; this stub presents the
//! [`ava_engine::Engine`] / [`ava_engine::Handler`] surface the chain
//! router/handler expects, so Simplex can be slotted in where the Snowman
//! engine would be once the BFT core lands.
//!
//! Every inbound op currently delegates to the log-and-drop
//! [`ava_engine::NoOpHandler`]; the `simplex` op specifically traces that a
//! Simplex protocol message arrived (the point at which the real engine will
//! decode the `p2p.Simplex` envelope and drive a round).

use std::time::Instant;

use async_trait::async_trait;
use tracing::debug;

use ava_engine::common::error::AppError;
use ava_engine::common::handler::{
    AcceptedHandler, AncestorsHandler, AppHandler, ChitsHandler, FrontierHandler, InternalHandler,
    PutHandler, QueryHandler, SimplexHandler, StateSyncHandler,
};
use ava_engine::common::no_ops::NoOpHandler;
use ava_engine::{Engine, Result as EngineResult};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::{Connector, Result as ValidatorResult};
use ava_vm::VmEvent;

use crate::parameters::Parameters;

/// A stubbed Simplex consensus engine.
///
/// Holds the validated [`Parameters`] and delegates every op to an inner
/// [`NoOpHandler`]. Construct via [`SimplexEngine::new`], which verifies the
/// parameters up front.
pub struct SimplexEngine {
    params: Parameters,
    inner: NoOpHandler,
}

impl SimplexEngine {
    /// Builds the stub from verified [`Parameters`]
    /// ([`Parameters::verify`](crate::Parameters::verify) is run first).
    pub fn new(params: Parameters) -> crate::Result<Self> {
        params.verify()?;
        Ok(Self {
            params,
            inner: NoOpHandler,
        })
    }

    /// The parameters this engine was built with.
    pub fn parameters(&self) -> &Parameters {
        &self.params
    }
}

// Delegate every op group to the inner NoOpHandler. The blanket `Handler`/
// `AllGetsServer` impls in `ava-engine` then apply to `SimplexEngine` for free.

#[async_trait]
impl StateSyncHandler for SimplexEngine {
    async fn get_state_summary_frontier(&mut self, node: NodeId, req: u32) -> EngineResult<()> {
        self.inner.get_state_summary_frontier(node, req).await
    }
    async fn state_summary_frontier(
        &mut self,
        node: NodeId,
        req: u32,
        summary: &[u8],
    ) -> EngineResult<()> {
        self.inner.state_summary_frontier(node, req, summary).await
    }
    async fn get_state_summary_frontier_failed(
        &mut self,
        node: NodeId,
        req: u32,
    ) -> EngineResult<()> {
        self.inner
            .get_state_summary_frontier_failed(node, req)
            .await
    }
    async fn get_accepted_state_summary(
        &mut self,
        node: NodeId,
        req: u32,
        heights: &[u64],
    ) -> EngineResult<()> {
        self.inner
            .get_accepted_state_summary(node, req, heights)
            .await
    }
    async fn accepted_state_summary(
        &mut self,
        node: NodeId,
        req: u32,
        summary_ids: &[Id],
    ) -> EngineResult<()> {
        self.inner
            .accepted_state_summary(node, req, summary_ids)
            .await
    }
    async fn get_accepted_state_summary_failed(
        &mut self,
        node: NodeId,
        req: u32,
    ) -> EngineResult<()> {
        self.inner
            .get_accepted_state_summary_failed(node, req)
            .await
    }
}

#[async_trait]
impl FrontierHandler for SimplexEngine {
    async fn get_accepted_frontier(&mut self, node: NodeId, req: u32) -> EngineResult<()> {
        self.inner.get_accepted_frontier(node, req).await
    }
    async fn accepted_frontier(
        &mut self,
        node: NodeId,
        req: u32,
        container_id: Id,
    ) -> EngineResult<()> {
        self.inner.accepted_frontier(node, req, container_id).await
    }
    async fn get_accepted_frontier_failed(&mut self, node: NodeId, req: u32) -> EngineResult<()> {
        self.inner.get_accepted_frontier_failed(node, req).await
    }
}

#[async_trait]
impl AcceptedHandler for SimplexEngine {
    async fn get_accepted(
        &mut self,
        node: NodeId,
        req: u32,
        container_ids: &[Id],
    ) -> EngineResult<()> {
        self.inner.get_accepted(node, req, container_ids).await
    }
    async fn accepted(&mut self, node: NodeId, req: u32, container_ids: &[Id]) -> EngineResult<()> {
        self.inner.accepted(node, req, container_ids).await
    }
    async fn get_accepted_failed(&mut self, node: NodeId, req: u32) -> EngineResult<()> {
        self.inner.get_accepted_failed(node, req).await
    }
}

#[async_trait]
impl AncestorsHandler for SimplexEngine {
    async fn get_ancestors(
        &mut self,
        node: NodeId,
        req: u32,
        container_id: Id,
    ) -> EngineResult<()> {
        self.inner.get_ancestors(node, req, container_id).await
    }
    async fn ancestors(
        &mut self,
        node: NodeId,
        req: u32,
        containers: &[Vec<u8>],
    ) -> EngineResult<()> {
        self.inner.ancestors(node, req, containers).await
    }
    async fn get_ancestors_failed(&mut self, node: NodeId, req: u32) -> EngineResult<()> {
        self.inner.get_ancestors_failed(node, req).await
    }
}

#[async_trait]
impl PutHandler for SimplexEngine {
    async fn get(&mut self, node: NodeId, req: u32, container_id: Id) -> EngineResult<()> {
        self.inner.get(node, req, container_id).await
    }
    async fn put(&mut self, node: NodeId, req: u32, container: &[u8]) -> EngineResult<()> {
        self.inner.put(node, req, container).await
    }
    async fn get_failed(&mut self, node: NodeId, req: u32) -> EngineResult<()> {
        self.inner.get_failed(node, req).await
    }
}

#[async_trait]
impl QueryHandler for SimplexEngine {
    async fn pull_query(
        &mut self,
        node: NodeId,
        req: u32,
        container_id: Id,
        requested_height: u64,
    ) -> EngineResult<()> {
        self.inner
            .pull_query(node, req, container_id, requested_height)
            .await
    }
    async fn push_query(
        &mut self,
        node: NodeId,
        req: u32,
        container: &[u8],
        requested_height: u64,
    ) -> EngineResult<()> {
        self.inner
            .push_query(node, req, container, requested_height)
            .await
    }
}

#[async_trait]
impl ChitsHandler for SimplexEngine {
    async fn chits(
        &mut self,
        node: NodeId,
        req: u32,
        preferred_id: Id,
        preferred_id_at_height: Id,
        accepted_id: Id,
        accepted_height: u64,
    ) -> EngineResult<()> {
        self.inner
            .chits(
                node,
                req,
                preferred_id,
                preferred_id_at_height,
                accepted_id,
                accepted_height,
            )
            .await
    }
    async fn query_failed(&mut self, node: NodeId, req: u32) -> EngineResult<()> {
        self.inner.query_failed(node, req).await
    }
}

#[async_trait]
impl AppHandler for SimplexEngine {
    async fn app_request(
        &mut self,
        node: NodeId,
        req: u32,
        deadline: Instant,
        request: &[u8],
    ) -> EngineResult<()> {
        self.inner.app_request(node, req, deadline, request).await
    }
    async fn app_response(&mut self, node: NodeId, req: u32, response: &[u8]) -> EngineResult<()> {
        self.inner.app_response(node, req, response).await
    }
    async fn app_request_failed(
        &mut self,
        node: NodeId,
        req: u32,
        err: AppError,
    ) -> EngineResult<()> {
        self.inner.app_request_failed(node, req, err).await
    }
    async fn app_gossip(&mut self, node: NodeId, msg: &[u8]) -> EngineResult<()> {
        self.inner.app_gossip(node, msg).await
    }
}

#[async_trait]
impl Connector for SimplexEngine {
    async fn connected(&self, node: NodeId) -> ValidatorResult<()> {
        self.inner.connected(node).await
    }
    async fn disconnected(&self, node: NodeId) -> ValidatorResult<()> {
        self.inner.disconnected(node).await
    }
}

#[async_trait]
impl InternalHandler for SimplexEngine {
    async fn gossip(&mut self) -> EngineResult<()> {
        self.inner.gossip().await
    }
    async fn shutdown(&mut self) -> EngineResult<()> {
        self.inner.shutdown().await
    }
    async fn notify(&mut self, msg: VmEvent) -> EngineResult<()> {
        self.inner.notify(msg).await
    }
}

#[async_trait]
impl SimplexHandler for SimplexEngine {
    async fn simplex(&mut self, node: NodeId, msg: &[u8]) -> EngineResult<()> {
        // The real engine decodes the p2p.Simplex envelope and drives a round
        // here; the stub records the arrival and drops it.
        debug!(
            op = "simplex",
            %node,
            bytes = msg.len(),
            "simplex engine stub: dropping protocol message"
        );
        Ok(())
    }
}

#[async_trait]
impl Engine for SimplexEngine {
    async fn start(&mut self, _start_req_id: u32) -> EngineResult<()> {
        debug!("simplex engine stub: start (no-op)");
        Ok(())
    }

    fn health_check(&self) -> EngineResult<serde_json::Value> {
        Ok(serde_json::json!({
            "engine": "simplex-stub",
            "validators": self.params.initial_validators.len(),
        }))
    }
}
