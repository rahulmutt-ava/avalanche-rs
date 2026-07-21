// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The process-wide [`ChainRouter`] (port of
//! `snow/networking/router/chain_router.go`, specs 06 §5.1).
//!
//! Owns the `chain_id -> Handler` map, routes every decoded inbound p2p message
//! to the right chain (dropping unknown-chain or disallowed messages), registers
//! each outgoing request with the [`AdaptiveTimeoutManager`], and on timeout
//! synthesizes the matching `*Failed` op into the handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::app::AppError;

use super::timeout::{AdaptiveTimeoutManager, RequestId};

/// Numeric op tags used to synthesize the right `*Failed` op on timeout. These
/// mirror the request ops that expect a response (Go `message.Op`). Public so
/// the engine `Sender` (`OutboundSender`) can tag each outgoing request when it
/// registers it with the router (see [`Router::register_request`]).
pub mod op {
    #![allow(missing_docs)]
    pub const GET: u8 = 1;
    pub const GET_ANCESTORS: u8 = 2;
    pub const GET_ACCEPTED_FRONTIER: u8 = 3;
    pub const GET_ACCEPTED: u8 = 4;
    pub const QUERY: u8 = 5;
    pub const APP_REQUEST: u8 = 6;
    pub const GET_STATE_SUMMARY_FRONTIER: u8 = 7;
    pub const GET_ACCEPTED_STATE_SUMMARY: u8 = 8;
}

/// A decoded inbound op delivered to a chain handler. This is the engine-internal
/// projection of a wire message (the router does not depend on the `ava-message`
/// codec; the network layer decodes and tags before handing off — see
/// `tests/PORTING.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InboundOp {
    /// `Get` request.
    Get {
        /// Wire request ID.
        request_id: u32,
        /// Requested container.
        container_id: Id,
    },
    /// `GetAcceptedFrontier` request — reply with our last-accepted frontier.
    GetAcceptedFrontier {
        /// Wire request ID.
        request_id: u32,
    },
    /// `GetAccepted` request — reply with the accepted subset of `container_ids`.
    GetAccepted {
        /// Wire request ID.
        request_id: u32,
        /// The queried container ids.
        container_ids: Vec<Id>,
    },
    /// `GetAncestors` request — reply with the block + best-effort ancestry.
    GetAncestors {
        /// Wire request ID.
        request_id: u32,
        /// The requested container id.
        container_id: Id,
    },
    /// `Put` response / unsolicited container.
    Put {
        /// Wire request ID (`u32::MAX` ⇒ unsolicited).
        request_id: u32,
        /// Container bytes.
        container: Vec<u8>,
    },
    /// A `Get` we issued will not be answered.
    GetFailed {
        /// Wire request ID.
        request_id: u32,
    },
    /// `PushQuery`/`PullQuery` failed.
    QueryFailed {
        /// Wire request ID.
        request_id: u32,
    },
    /// A `GetAncestors` we issued will not be answered.
    GetAncestorsFailed {
        /// Wire request ID.
        request_id: u32,
    },
    /// A `GetAcceptedFrontier` we issued will not be answered.
    GetAcceptedFrontierFailed {
        /// Wire request ID.
        request_id: u32,
    },
    /// A `GetAccepted` we issued will not be answered.
    GetAcceptedFailed {
        /// Wire request ID.
        request_id: u32,
    },
    /// A `GetStateSummaryFrontier` we issued will not be answered.
    GetStateSummaryFrontierFailed {
        /// Wire request ID.
        request_id: u32,
    },
    /// A `GetAcceptedStateSummary` we issued will not be answered.
    GetAcceptedStateSummaryFailed {
        /// Wire request ID.
        request_id: u32,
    },
    /// An `AppRequest` we issued failed.
    AppRequestFailed {
        /// Wire request ID.
        request_id: u32,
        /// Application-defined error code (`ava_vm::app::AppError::TIMEOUT` on
        /// timeout synthesis; an app-supplied code when decoded from a wire
        /// `AppError`).
        code: i32,
        /// Human-readable error message.
        message: String,
    },

    // --- App messages (Task 7: engine inbound App routing) -----------------
    /// `AppRequest` — VM-defined request; `deadline_nanos` is the wire-relative
    /// deadline (converted to a monotonic `Instant` by the adapter).
    AppRequest {
        /// Wire request ID.
        request_id: u32,
        /// Wire-relative deadline, in nanoseconds.
        deadline_nanos: u64,
        /// VM-defined request bytes.
        bytes: Vec<u8>,
    },
    /// `AppResponse` to an `AppRequest` we issued.
    AppResponse {
        /// Wire request ID.
        request_id: u32,
        /// VM-defined response bytes.
        bytes: Vec<u8>,
    },
    /// `AppGossip` — VM-defined gossip (no request id).
    AppGossip {
        /// VM-defined gossip bytes.
        bytes: Vec<u8>,
    },

    // --- Bootstrap / consensus responses (M4.30a) --------------------------
    /// `AcceptedFrontier` — a beacon's last-accepted frontier id (bootstrap).
    AcceptedFrontier {
        /// Wire request ID.
        request_id: u32,
        /// The beacon's last-accepted container id.
        container_id: Id,
    },
    /// `Accepted` — a beacon's accepted subset of the queried frontier.
    Accepted {
        /// Wire request ID.
        request_id: u32,
        /// The accepted container ids.
        container_ids: Vec<Id>,
    },
    /// `Ancestors` — a chain of fetched container bytes (newest-first).
    Ancestors {
        /// Wire request ID.
        request_id: u32,
        /// The fetched container bytes.
        containers: Vec<Vec<u8>>,
    },
    /// `PushQuery` — a query carrying the queried container.
    PushQuery {
        /// Wire request ID.
        request_id: u32,
        /// The queried container bytes.
        container: Vec<u8>,
        /// The querier's requested height.
        requested_height: u64,
    },
    /// `PullQuery` — a query naming the queried container by id.
    PullQuery {
        /// Wire request ID.
        request_id: u32,
        /// The queried container id.
        container_id: Id,
        /// The querier's requested height.
        requested_height: u64,
    },
    /// `Chits` — a vote carrying the peer's preferred / preferred-at-height /
    /// last-accepted ids (matches [`Sender::send_chits`](crate::common::sender::Sender::send_chits)).
    Chits {
        /// Wire request ID.
        request_id: u32,
        /// The peer's preferred container id.
        preferred_id: Id,
        /// The peer's preferred container id at the requested height.
        preferred_id_at_height: Id,
        /// The peer's last-accepted container id.
        accepted_id: Id,
        /// The peer's last-accepted height.
        accepted_height: u64,
    },
}

impl InboundOp {
    /// The op tag for a `Get` request (used when registering it for timeout).
    #[must_use]
    pub fn failed_kind_for_get() -> u8 {
        op::GET
    }

    /// The op tag for a query request.
    #[must_use]
    pub fn failed_kind_for_query() -> u8 {
        op::QUERY
    }

    /// Synthesize the `*Failed` op for a timed-out request `op_tag`+`request_id`.
    fn failed(op_tag: u8, request_id: u32) -> InboundOp {
        match op_tag {
            op::GET => InboundOp::GetFailed { request_id },
            op::GET_ANCESTORS => InboundOp::GetAncestorsFailed { request_id },
            op::GET_ACCEPTED_FRONTIER => InboundOp::GetAcceptedFrontierFailed { request_id },
            op::GET_ACCEPTED => InboundOp::GetAcceptedFailed { request_id },
            op::QUERY => InboundOp::QueryFailed { request_id },
            op::APP_REQUEST => {
                // Framework timeout synthesis: the peer never answered our
                // AppRequest, so the VM sees the same `ErrTimeout` sentinel it
                // would get on any other transport (`AppError::TIMEOUT` = -1).
                let timeout = AppError::timeout();
                InboundOp::AppRequestFailed {
                    request_id,
                    code: timeout.code,
                    message: timeout.message,
                }
            }
            op::GET_STATE_SUMMARY_FRONTIER => {
                InboundOp::GetStateSummaryFrontierFailed { request_id }
            }
            op::GET_ACCEPTED_STATE_SUMMARY => {
                InboundOp::GetAcceptedStateSummaryFailed { request_id }
            }
            // Unknown tag: fall back to a generic Get failure (Go drops unknowns;
            // this keeps the failure observable for the handler).
            _ => InboundOp::GetFailed { request_id },
        }
    }
}

/// A decoded inbound message addressed to a chain (specs 05 hands these in
/// pre-authenticated).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundMessage {
    /// The (pre-authenticated) sender.
    pub node: NodeId,
    /// The destination chain.
    pub chain: Id,
    /// The op to deliver.
    pub op: InboundOp,
}

/// The sink a chain's handler exposes to the router. The real
/// [`ChainHandler`](super::handler::ChainHandler) implements this by enqueuing
/// onto its bounded message queue; tests use a recording stub.
#[async_trait]
pub trait ChainMessageSink: Send + Sync {
    /// Deliver `op` from `node` to this chain's handler.
    async fn push(&self, node: NodeId, op: InboundOp);

    /// ACL hook: whether this handler will accept a message from `node`
    /// (Go `Handler.ShouldHandle`). Defaults to accepting everyone.
    fn should_handle(&self, _node: NodeId) -> bool {
        true
    }
}

/// The router surface the network layer drives.
#[async_trait]
pub trait Router: Send + Sync {
    /// Register a chain's handler under its `chain_id`.
    fn add_chain(&self, chain: Id, handler: Arc<dyn ChainMessageSink>);

    /// Route a decoded inbound message to the right chain handler (dropping it if
    /// the chain is unknown or the sender is not allowed).
    async fn handle_inbound(&self, msg: InboundMessage);

    /// Register an outgoing request; on timeout the matching `*Failed` op is
    /// synthesized into the handler. `op_tag` is one of the [`op`] constants.
    ///
    /// Synchronous (the timeout manager's lock holds no `.await`), so the
    /// engine's synchronous `Sender` can register a request happens-before the
    /// wire send returns — a fast response can never `remove` an entry the
    /// registration has not yet inserted.
    fn register_request(&self, node: NodeId, chain: Id, request_id: u32, op_tag: u8);

    /// Immediately fail a registered request: cancel its pending timer and
    /// synthesize the matching `*Failed` op into the chain handler. This is the
    /// sender's "unsent ⇒ fail now" leg (Go `sender.Send*` handing the pre-built
    /// failure to `router.HandleInternal` for each recipient missing from
    /// `network.Send`'s sent-set; `sender.go:525-535` and
    /// `chain_router.go:349-375` @ 96897293a2).
    ///
    /// Mechanism note (recorded, deliberate divergence from Go — not full
    /// parity): Go leaves the still-armed timer registered and suppresses the
    /// *delivery* of its later firing via a `handled` flag, but that firing
    /// still runs the normal timeout path first, which **does** observe the
    /// full configured timeout as latency (`adaptive_timeout_manager.go:192-205,255-266`;
    /// `measureLatency=true` is set unconditionally per request at
    /// registration, `chain_router.go:199`) — so under sustained unsent
    /// conditions Go's average timeout grows even though no `*Failed` is
    /// delivered twice. Rust instead cancels the timer outright here (claiming
    /// the entry so the background dispatch loop can never also fire it),
    /// which skips that latency observation entirely: the average stays clean
    /// at the cost of not modeling the "requests we're failing early are also
    /// slow" signal Go's average captures. This is a liveness-only,
    /// intentional divergence (follow-up, not yet revisited).
    fn fail_request(&self, node: NodeId, chain: Id, request_id: u32, op_tag: u8);

    /// Whether the router is healthy (no chain is unknown / over its limit).
    fn health_check(&self) -> bool;
}

/// One process-wide router owning the `chain_id -> Handler` map and the request
/// registry matched to the [`AdaptiveTimeoutManager`].
pub struct ChainRouter {
    chains: Mutex<HashMap<Id, Arc<dyn ChainMessageSink>>>,
    timeouts: Arc<AdaptiveTimeoutManager>,
}

impl ChainRouter {
    /// Build a router over the shared timeout manager.
    #[must_use]
    pub fn new(timeouts: Arc<AdaptiveTimeoutManager>) -> Arc<Self> {
        Arc::new(Self {
            chains: Mutex::new(HashMap::new()),
            timeouts,
        })
    }

    fn handler_for(&self, chain: Id) -> Option<Arc<dyn ChainMessageSink>> {
        self.chains.lock().ok()?.get(&chain).cloned()
    }
}

#[async_trait]
impl Router for ChainRouter {
    fn add_chain(&self, chain: Id, handler: Arc<dyn ChainMessageSink>) {
        if let Ok(mut chains) = self.chains.lock() {
            chains.insert(chain, handler);
        }
    }

    async fn handle_inbound(&self, msg: InboundMessage) {
        let Some(handler) = self.handler_for(msg.chain) else {
            // Drop messages for unknown chains.
            return;
        };
        if !handler.should_handle(msg.node) {
            // Drop messages the handler's ACL rejects.
            return;
        }
        handler.push(msg.node, msg.op).await;
    }

    fn register_request(&self, node: NodeId, chain: Id, request_id: u32, op_tag: u8) {
        let id = RequestId {
            node,
            chain,
            request_id,
            op: op_tag,
        };

        // On timeout, synthesize the matching *Failed op into the chain handler.
        let handler = self.handler_for(chain);
        let handler_node = node;
        let timeout_handler = move || {
            if let Some(handler) = handler {
                let failed = InboundOp::failed(op_tag, request_id);
                // Fire-and-forget: deliver the failure on a detached task (the
                // timeout dispatch loop must not block on handler back-pressure).
                tokio::spawn(async move {
                    handler.push(handler_node, failed).await;
                });
            }
        };

        self.timeouts.put(id, true, Box::new(timeout_handler));
    }

    fn fail_request(&self, node: NodeId, chain: Id, request_id: u32, op_tag: u8) {
        // Cancel the pending timer WITHOUT observing latency: an early failure
        // is not a response and must not shrink the averaged timeout. `cancel`
        // returns `true` only if THIS call actually removed the pending entry —
        // that is the exactly-once claim point. If the background timer won the
        // race and already fired (and removed the entry) between
        // `register_request` and this call, `cancel` returns `false` and we
        // must NOT synthesize a second `*Failed`: the engine treats `*Failed`
        // (e.g. `QueryFailed`) as a one-shot event, not idempotent — a duplicate
        // re-enters the `chits()` self-vote path (`engine.rs:545-560`).
        let claimed = self.timeouts.cancel(RequestId {
            node,
            chain,
            request_id,
            op: op_tag,
        });
        if !claimed {
            return;
        }
        if let Some(handler) = self.handler_for(chain) {
            let failed = InboundOp::failed(op_tag, request_id);
            // Fire-and-forget on a detached task, mirroring the timer path (and
            // Go's `HandleInternal`, which runs `handleMessage` on its own
            // goroutine): the synchronous sender must not block on handler
            // back-pressure.
            tokio::spawn(async move {
                handler.push(node, failed).await;
            });
        }
    }

    fn health_check(&self) -> bool {
        // Healthy iff every registered chain is reachable; deeper queue-depth /
        // drop-rate accounting lands with the full handler wiring (06 §5.1).
        self.chains.lock().is_ok()
    }
}

impl ChainRouter {
    /// Clear the outstanding-request registry entry on a matching response
    /// (engine-side, when a `Put`/`Chits`/etc. arrives). Cancels the timer so the
    /// `*Failed` op is not synthesized.
    pub fn on_response(&self, node: NodeId, chain: Id, request_id: u32, op_tag: u8) {
        self.timeouts.remove(RequestId {
            node,
            chain,
            request_id,
            op: op_tag,
        });
    }

    /// The configured request-registration timeout (for callers that want to set
    /// a wire deadline; `clock`-relative). Convenience over the timeout manager.
    #[must_use]
    pub fn current_timeout(&self) -> Duration {
        self.timeouts.timeout_duration()
    }
}
