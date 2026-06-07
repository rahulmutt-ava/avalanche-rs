// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`NoOpHandler`] — the log-and-drop default for every inbound op (port of
//! `snow/engine/common/no_ops_handlers.go`, specs 06 §4.1).
//!
//! An engine that doesn't handle a given op group (e.g. a Snowman engine that
//! does no state-sync) embeds a `NoOpHandler` and delegates the unhandled traits
//! to it. Every method logs at `debug` ("dropping request"/"dropping response")
//! and returns `Ok(())`, exactly like Go's `noOp*Handler` family.

use std::time::Instant;

use async_trait::async_trait;
use tracing::debug;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::Connector;
use ava_vm::VmEvent;

use crate::common::error::AppError;
use crate::common::handler::{
    AcceptedHandler, AncestorsHandler, AppHandler, ChitsHandler, FrontierHandler, InternalHandler,
    PutHandler, QueryHandler, SimplexHandler, StateSyncHandler,
};
use crate::error::Result;

/// The log-and-drop default for every inbound op. Zero-sized; embed it in an
/// engine and delegate the op groups it does not handle.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoOpHandler;

#[async_trait]
impl StateSyncHandler for NoOpHandler {
    async fn get_state_summary_frontier(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "get_state_summary_frontier", %node, req, "dropping request");
        Ok(())
    }

    async fn state_summary_frontier(
        &mut self,
        node: NodeId,
        req: u32,
        _summary: &[u8],
    ) -> Result<()> {
        debug!(op = "state_summary_frontier", %node, req, "dropping response");
        Ok(())
    }

    async fn get_state_summary_frontier_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "get_state_summary_frontier_failed", %node, req, "dropping response");
        Ok(())
    }

    async fn get_accepted_state_summary(
        &mut self,
        node: NodeId,
        req: u32,
        _heights: &[u64],
    ) -> Result<()> {
        debug!(op = "get_accepted_state_summary", %node, req, "dropping request");
        Ok(())
    }

    async fn accepted_state_summary(
        &mut self,
        node: NodeId,
        req: u32,
        _summary_ids: &[Id],
    ) -> Result<()> {
        debug!(op = "accepted_state_summary", %node, req, "dropping response");
        Ok(())
    }

    async fn get_accepted_state_summary_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "get_accepted_state_summary_failed", %node, req, "dropping response");
        Ok(())
    }
}

#[async_trait]
impl FrontierHandler for NoOpHandler {
    async fn get_accepted_frontier(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "get_accepted_frontier", %node, req, "dropping request");
        Ok(())
    }

    async fn accepted_frontier(&mut self, node: NodeId, req: u32, _container_id: Id) -> Result<()> {
        debug!(op = "accepted_frontier", %node, req, "dropping response");
        Ok(())
    }

    async fn get_accepted_frontier_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "get_accepted_frontier_failed", %node, req, "dropping response");
        Ok(())
    }
}

#[async_trait]
impl AcceptedHandler for NoOpHandler {
    async fn get_accepted(&mut self, node: NodeId, req: u32, _container_ids: &[Id]) -> Result<()> {
        debug!(op = "get_accepted", %node, req, "dropping request");
        Ok(())
    }

    async fn accepted(&mut self, node: NodeId, req: u32, _container_ids: &[Id]) -> Result<()> {
        debug!(op = "accepted", %node, req, "dropping response");
        Ok(())
    }

    async fn get_accepted_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "get_accepted_failed", %node, req, "dropping response");
        Ok(())
    }
}

#[async_trait]
impl AncestorsHandler for NoOpHandler {
    async fn get_ancestors(&mut self, node: NodeId, req: u32, _container_id: Id) -> Result<()> {
        debug!(op = "get_ancestors", %node, req, "dropping request");
        Ok(())
    }

    async fn ancestors(&mut self, node: NodeId, req: u32, _containers: &[Vec<u8>]) -> Result<()> {
        debug!(op = "ancestors", %node, req, "dropping response");
        Ok(())
    }

    async fn get_ancestors_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "get_ancestors_failed", %node, req, "dropping response");
        Ok(())
    }
}

#[async_trait]
impl PutHandler for NoOpHandler {
    async fn get(&mut self, node: NodeId, req: u32, _container_id: Id) -> Result<()> {
        debug!(op = "get", %node, req, "dropping request");
        Ok(())
    }

    async fn put(&mut self, node: NodeId, req: u32, _container: &[u8]) -> Result<()> {
        debug!(op = "put", %node, req, "dropping response");
        Ok(())
    }

    async fn get_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "get_failed", %node, req, "dropping response");
        Ok(())
    }
}

#[async_trait]
impl QueryHandler for NoOpHandler {
    async fn pull_query(
        &mut self,
        node: NodeId,
        req: u32,
        _container_id: Id,
        _requested_height: u64,
    ) -> Result<()> {
        debug!(op = "pull_query", %node, req, "dropping request");
        Ok(())
    }

    async fn push_query(
        &mut self,
        node: NodeId,
        req: u32,
        _container: &[u8],
        _requested_height: u64,
    ) -> Result<()> {
        debug!(op = "push_query", %node, req, "dropping request");
        Ok(())
    }
}

#[async_trait]
impl ChitsHandler for NoOpHandler {
    async fn chits(
        &mut self,
        node: NodeId,
        req: u32,
        _preferred_id: Id,
        _preferred_id_at_height: Id,
        _accepted_id: Id,
        _accepted_height: u64,
    ) -> Result<()> {
        debug!(op = "chits", %node, req, "dropping response");
        Ok(())
    }

    async fn query_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        debug!(op = "query_failed", %node, req, "dropping response");
        Ok(())
    }
}

#[async_trait]
impl AppHandler for NoOpHandler {
    async fn app_request(
        &mut self,
        node: NodeId,
        req: u32,
        _deadline: Instant,
        _request: &[u8],
    ) -> Result<()> {
        debug!(op = "app_request", %node, req, "dropping request");
        Ok(())
    }

    async fn app_response(&mut self, node: NodeId, req: u32, _response: &[u8]) -> Result<()> {
        debug!(op = "app_response", %node, req, "dropping response");
        Ok(())
    }

    async fn app_request_failed(&mut self, node: NodeId, req: u32, _err: AppError) -> Result<()> {
        debug!(op = "app_request_failed", %node, req, "dropping response");
        Ok(())
    }

    async fn app_gossip(&mut self, node: NodeId, _msg: &[u8]) -> Result<()> {
        debug!(op = "app_gossip", %node, "dropping gossip");
        Ok(())
    }
}

#[async_trait]
impl Connector for NoOpHandler {
    async fn connected(&self, node: NodeId) -> ava_validators::Result<()> {
        debug!(op = "connected", %node, "dropping notification");
        Ok(())
    }

    async fn disconnected(&self, node: NodeId) -> ava_validators::Result<()> {
        debug!(op = "disconnected", %node, "dropping notification");
        Ok(())
    }
}

#[async_trait]
impl InternalHandler for NoOpHandler {
    async fn gossip(&mut self) -> Result<()> {
        debug!(op = "gossip", "dropping notification");
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        debug!(op = "shutdown", "dropping notification");
        Ok(())
    }

    async fn notify(&mut self, msg: VmEvent) -> Result<()> {
        debug!(op = "notify", ?msg, "dropping notification");
        Ok(())
    }
}

#[async_trait]
impl SimplexHandler for NoOpHandler {
    async fn simplex(&mut self, node: NodeId, _msg: &[u8]) -> Result<()> {
        debug!(op = "simplex", %node, "dropping message");
        Ok(())
    }
}
