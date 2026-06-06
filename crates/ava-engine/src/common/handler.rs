// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The inbound-op state-machine traits (port of `snow/engine/common/engine.go`,
//! specs 06 §4.1).
//!
//! `Engine = Handler + Start + HealthCheck`, where `Handler` is the union of
//! every inbound op. We model it as **one `#[async_trait]` trait per op group**,
//! each object-safe, composed into the object-safe [`Handler`] super-trait. Every
//! method takes `(node, request_id, ...)`; all node IDs are pre-authenticated by
//! the network layer (specs 05).
//!
//! Each *request* op has a matching `*_failed` callback fired by the
//! `TimeoutManager` when no response arrives. The full op set must exist for
//! wire/handler parity even when a given engine no-ops it (see
//! [`NoOpHandler`](crate::common::no_ops::NoOpHandler)).

use std::time::Instant;

use async_trait::async_trait;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::Connector;
use ava_vm::VmEvent;

use crate::common::error::AppError;
use crate::error::Result;

// ---------------------------------------------------------------------------
// State sync (`GetStateSummaryFrontier`/`StateSummaryFrontier`,
// `GetAcceptedStateSummary`/`AcceptedStateSummary`).
// ---------------------------------------------------------------------------

/// Handles inbound state-summary requests and responses
/// (`StateSummaryFrontierHandler` + `GetStateSummaryFrontierHandler` +
/// `AcceptedStateSummaryHandler` + `GetAcceptedStateSummaryHandler`).
#[async_trait]
pub trait StateSyncHandler: Send {
    /// `GetStateSummaryFrontier` — request the engine's most recently accepted
    /// state summary. Callable by any node at any time.
    async fn get_state_summary_frontier(&mut self, node: NodeId, req: u32) -> Result<()>;

    /// `StateSummaryFrontier` — response carrying summary bytes (not guaranteed
    /// to be a valid state summary).
    async fn state_summary_frontier(
        &mut self,
        node: NodeId,
        req: u32,
        summary: &[u8],
    ) -> Result<()>;

    /// `GetStateSummaryFrontierFailed` — a `GetStateSummaryFrontier` we issued
    /// will not receive a response.
    async fn get_state_summary_frontier_failed(&mut self, node: NodeId, req: u32) -> Result<()>;

    /// `GetAcceptedStateSummary` — request summary IDs at the requested heights.
    /// Heights without a known summary are ignored.
    async fn get_accepted_state_summary(
        &mut self,
        node: NodeId,
        req: u32,
        heights: &[u64],
    ) -> Result<()>;

    /// `AcceptedStateSummary` — response carrying summary IDs (heights are not
    /// guaranteed to match the request).
    async fn accepted_state_summary(
        &mut self,
        node: NodeId,
        req: u32,
        summary_ids: &[Id],
    ) -> Result<()>;

    /// `GetAcceptedStateSummaryFailed` — a `GetAcceptedStateSummary` we issued
    /// will not receive a response.
    async fn get_accepted_state_summary_failed(&mut self, node: NodeId, req: u32) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Accepted frontier (`GetAcceptedFrontier`/`AcceptedFrontier`).
// ---------------------------------------------------------------------------

/// Handles accepted-frontier requests and responses (`AcceptedFrontierHandler` +
/// `GetAcceptedFrontierHandler`).
#[async_trait]
pub trait FrontierHandler: Send {
    /// `GetAcceptedFrontier` — request the ID of the most recently accepted
    /// container. Callable by any node at any time.
    async fn get_accepted_frontier(&mut self, node: NodeId, req: u32) -> Result<()>;

    /// `AcceptedFrontier` — response carrying the accepted-frontier container ID.
    async fn accepted_frontier(
        &mut self,
        node: NodeId,
        req: u32,
        container_id: Id,
    ) -> Result<()>;

    /// `GetAcceptedFrontierFailed` — a `GetAcceptedFrontier` we issued will not
    /// receive a response.
    async fn get_accepted_frontier_failed(&mut self, node: NodeId, req: u32) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Accepted (`GetAccepted`/`Accepted`).
// ---------------------------------------------------------------------------

/// Handles accepted-container requests and responses (`AcceptedHandler` +
/// `GetAcceptedHandler`).
#[async_trait]
pub trait AcceptedHandler: Send {
    /// `GetAccepted` — request the subset of `container_ids` this node has
    /// accepted. Callable by any node at any time.
    async fn get_accepted(&mut self, node: NodeId, req: u32, container_ids: &[Id]) -> Result<()>;

    /// `Accepted` — response carrying the accepted container IDs (not guaranteed
    /// to be a subset of the request).
    async fn accepted(&mut self, node: NodeId, req: u32, container_ids: &[Id]) -> Result<()>;

    /// `GetAcceptedFailed` — a `GetAccepted` we issued will not receive a
    /// response.
    async fn get_accepted_failed(&mut self, node: NodeId, req: u32) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Ancestors (`GetAncestors`/`Ancestors`).
// ---------------------------------------------------------------------------

/// Handles ancestor requests and responses (`AncestorsHandler` +
/// `GetAncestorsHandler`).
#[async_trait]
pub trait AncestorsHandler: Send {
    /// `GetAncestors` — request `container_id` plus some ancestors (best effort).
    /// Callable by any node at any time.
    async fn get_ancestors(&mut self, node: NodeId, req: u32, container_id: Id) -> Result<()>;

    /// `Ancestors` — response carrying the containers (the first is expected, but
    /// not guaranteed, to be the requested container).
    async fn ancestors(&mut self, node: NodeId, req: u32, containers: &[Vec<u8>]) -> Result<()>;

    /// `GetAncestorsFailed` — a `GetAncestors` we issued will not receive a
    /// response.
    async fn get_ancestors_failed(&mut self, node: NodeId, req: u32) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Get/Put (`Get`/`Put`).
// ---------------------------------------------------------------------------

/// Handles `Get` requests and `Put` responses (`GetHandler` + `PutHandler`).
#[async_trait]
pub trait PutHandler: Send {
    /// `Get` — request a `Put` for the container whose ID is `container_id`.
    /// Callable by any node at any time.
    async fn get(&mut self, node: NodeId, req: u32, container_id: Id) -> Result<()>;

    /// `Put` — either the response to a previously sent `Get` with the same
    /// `req`, or an unsolicited container if `req == u32::MAX`. Not guaranteed to
    /// be parseable or issuable.
    async fn put(&mut self, node: NodeId, req: u32, container: &[u8]) -> Result<()>;

    /// `GetFailed` — a `Get` we issued will not receive a response.
    async fn get_failed(&mut self, node: NodeId, req: u32) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Query / vote (`PushQuery`/`PullQuery`/`Chits`).
// ---------------------------------------------------------------------------

/// Handles inbound queries (`QueryHandler`).
#[async_trait]
pub trait QueryHandler: Send {
    /// `PullQuery` — request a `Chits` for `container_id` at `requested_height`.
    /// Callable by any node at any time.
    async fn pull_query(
        &mut self,
        node: NodeId,
        req: u32,
        container_id: Id,
        requested_height: u64,
    ) -> Result<()>;

    /// `PushQuery` — request a `Chits` for the supplied `container` at
    /// `requested_height`. Not guaranteed to be parseable or issuable. Callable
    /// by any node at any time.
    async fn push_query(
        &mut self,
        node: NodeId,
        req: u32,
        container: &[u8],
        requested_height: u64,
    ) -> Result<()>;
}

/// Handles `Chits` responses and query failures (`ChitsHandler`).
#[async_trait]
pub trait ChitsHandler: Send {
    /// `Chits` — response to a previously sent `PullQuery`/`PushQuery`. It is
    /// expected, but not guaranteed, that `preferred_id` transitively references
    /// `preferred_id_at_height` and `accepted_id`.
    async fn chits(
        &mut self,
        node: NodeId,
        req: u32,
        preferred_id: Id,
        preferred_id_at_height: Id,
        accepted_id: Id,
        accepted_height: u64,
    ) -> Result<()>;

    /// `QueryFailed` — a `PullQuery`/`PushQuery` we issued will not receive a
    /// response.
    async fn query_failed(&mut self, node: NodeId, req: u32) -> Result<()>;
}

// ---------------------------------------------------------------------------
// App (`AppRequest`/`AppResponse`/`AppGossip`/`AppError`).
// ---------------------------------------------------------------------------

/// Handles inbound application messages (`AppHandler` =
/// `AppRequestHandler` + `AppResponseHandler` + `AppGossipHandler`).
///
/// This is the **engine-facing** `AppHandler`, distinct from `ava-vm`'s VM-facing
/// `AppHandler`: the engine routes these to/from the VM.
#[async_trait]
pub trait AppHandler: Send {
    /// `AppRequest` — request for an `AppResponse` with the same `req`. The
    /// meaning of `request` is VM-specific and not guaranteed well-formed.
    /// Callable by any node at any time.
    async fn app_request(
        &mut self,
        node: NodeId,
        req: u32,
        deadline: Instant,
        request: &[u8],
    ) -> Result<()>;

    /// `AppResponse` — response to a previously sent `AppRequest`. VM-specific,
    /// not guaranteed well-formed.
    async fn app_response(&mut self, node: NodeId, req: u32, response: &[u8]) -> Result<()>;

    /// `AppRequestFailed` — an `AppRequest` we issued failed; `err` carries the
    /// application-level [`AppError`].
    async fn app_request_failed(
        &mut self,
        node: NodeId,
        req: u32,
        err: AppError,
    ) -> Result<()>;

    /// `AppGossip` — a gossip message from `node`. Not expected in response to
    /// any event and need not be responded to.
    async fn app_gossip(&mut self, node: NodeId, msg: &[u8]) -> Result<()>;
}

// ---------------------------------------------------------------------------
// All-gets server: the read-only request side
// (`AllGetsServer` = every `Get*` request handler).
// ---------------------------------------------------------------------------

/// `AllGetsServer` — the union of all read-only `Get*` request handlers, served
/// by every engine. It is implied by the individual per-op handlers; this marker
/// super-trait mirrors Go's `AllGetsServer` for parity.
pub trait AllGetsServer:
    StateSyncHandler + FrontierHandler + AcceptedHandler + AncestorsHandler + PutHandler + Send
{
}

impl<T> AllGetsServer for T where
    T: StateSyncHandler + FrontierHandler + AcceptedHandler + AncestorsHandler + PutHandler + Send
{
}

// ---------------------------------------------------------------------------
// Internal (`Connected`/`Disconnected`, `Gossip`, `Shutdown`, `Notify`).
// ---------------------------------------------------------------------------

/// Handles internal engine events (`InternalHandler` = `validators.Connector` +
/// `Gossip`/`Shutdown`/`Notify`).
#[async_trait]
pub trait InternalHandler: Connector + Send {
    /// `Gossip` — gossip a container on the accepted frontier to the network.
    async fn gossip(&mut self) -> Result<()>;

    /// `Shutdown` — shut this engine down; called when the environment exits.
    async fn shutdown(&mut self) -> Result<()>;

    /// `Notify` — a [`VmEvent`] from the virtual machine (e.g. `PendingTxs`).
    async fn notify(&mut self, msg: VmEvent) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Simplex (`Simplex`).
// ---------------------------------------------------------------------------

/// Handles inbound Simplex consensus messages (`SimplexHandler`).
///
/// Go passes a decoded `*p2p.Simplex`; here we take the raw message bytes to
/// keep the trait decoupled from the generated proto type (the Simplex engine
/// decodes them). See `tests/PORTING.md`.
#[async_trait]
pub trait SimplexHandler: Send {
    /// `Simplex` — a Simplex protocol message from `node`.
    async fn simplex(&mut self, node: NodeId, msg: &[u8]) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Handler: the union of every inbound op (object-safe).
// ---------------------------------------------------------------------------

/// `snow/engine/common.Handler` — the union of every inbound op group. Object-safe
/// so the chain router can drive `&mut dyn Handler` / `Box<dyn Handler>`.
pub trait Handler:
    AllGetsServer
    + StateSyncHandler
    + FrontierHandler
    + AcceptedHandler
    + AncestorsHandler
    + PutHandler
    + QueryHandler
    + ChitsHandler
    + AppHandler
    + InternalHandler
    + SimplexHandler
    + Send
{
}

impl<T> Handler for T where
    T: AllGetsServer
        + StateSyncHandler
        + FrontierHandler
        + AcceptedHandler
        + AncestorsHandler
        + PutHandler
        + QueryHandler
        + ChitsHandler
        + AppHandler
        + InternalHandler
        + SimplexHandler
        + Send
{
}
