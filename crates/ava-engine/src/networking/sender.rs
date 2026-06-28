// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`OutboundSender`] — the concrete, ava-network-backed [`Sender`]
//! (port of `snow/networking/sender.sender`, specs 06 §5.3).
//!
//! Each engine `send_*` call is translated into the matching `proto/p2p` wire
//! message ([`ava_message`]), then handed to the network's `ExternalSender`
//! ([`ava_network::network::Network::send`] / `gossip`). This is the production
//! replacement for the in-process loopback/recording senders used by the solo
//! in-process boot path: a real multi-node node drives consensus and app
//! traffic to real peers through this type.
//!
//! ## Recipient selection
//!
//! Targeted sends carry an [`ava_network::network::SendConfig`] (an explicit
//! node set plus optional validator/non-validator/peer sampling), and the
//! network applies the chain's subnet [`Allower`]. Gossip uses
//! [`ava_network::network::GossipConfig`]. The engine-facing [`SendConfig`]
//! and the network's `SendConfig` have identical field shapes (mirroring Go's
//! one `common.SendConfig`); [`OutboundSender`] maps between them.
//!
//! ## Deadlines
//!
//! Request ops carry a `deadline` field, the request timeout as a **relative**
//! nanosecond duration (the receiver computes the absolute expiry on arrival,
//! matching `MsgBuilder::parse_inbound`). [`OutboundSender`] writes the
//! configured `request_timeout` into every request op.
//!
//! ## Timeout registration
//!
//! Every outgoing **request** op (the ops that expect a response: `Get`,
//! `GetAncestors`, the accepted / state-summary frontier fetches, `PushQuery` /
//! `PullQuery`, and `AppRequest`) is registered with the [`Router`] before the
//! wire send, so the matching `*Failed` op is synthesized into the chain
//! handler if no peer responds by the deadline (Go `sender` + `timeout.Manager`).
//! For multi-recipient requests one timeout is registered **per recipient**
//! (each `RequestId` carries the node). **Reply** ops (`Put`, `Ancestors`,
//! `Chits`, `AppResponse`, …) are not registered — they expect no response.
//!
//! Registration is synchronous and happens-before the wire send: the timeout
//! manager uses a [`std::sync::Mutex`] (its critical section holds no `.await`),
//! so a fast response can never `remove` a pending entry the registration has
//! not yet inserted.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use ava_message::codec::{Compression, MsgBuilder};
use ava_message::proto::p2p;
use ava_network::network::{Allower, GossipConfig, Network, SendConfig as NetSendConfig};
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::common::sender::{SendConfig, Sender};
use crate::error::Result;
use crate::networking::router::{Router, op};

/// The concrete [`Sender`]: translate engine ops into `proto/p2p` wire
/// messages and dispatch them through an [`ava_network::network::Network`].
pub struct OutboundSender {
    /// The network runtime (`ExternalSender`): turns an
    /// [`ava_message::codec::OutboundMessage`] + recipient config into queued
    /// peer writes.
    network: Arc<dyn Network>,
    /// The subnet membership filter applied on every send (primary network =
    /// allow-all).
    allower: Arc<dyn Allower>,
    /// The router, used to register each outgoing request for timeout tracking
    /// (so a `*Failed` op is synthesized on a non-response).
    router: Arc<dyn Router>,
    /// The proto3 marshaler. Cheap to clone; held by value.
    mb: MsgBuilder,
    /// This chain's id, stamped into every message.
    chain_id: Id,
    /// This chain's subnet id, used for recipient selection + the allower.
    subnet_id: Id,
    /// The request timeout written into request ops' `deadline` (relative
    /// nanoseconds).
    request_timeout: Duration,
    /// Outbound compression policy (matches the negotiated peer capability;
    /// `None` for now, mirroring the loopback path — compression is a network
    /// concern the codec already supports).
    compression: Compression,
}

impl OutboundSender {
    /// Builds an [`OutboundSender`] for one chain.
    #[must_use]
    pub fn new(
        network: Arc<dyn Network>,
        allower: Arc<dyn Allower>,
        router: Arc<dyn Router>,
        chain_id: Id,
        subnet_id: Id,
        request_timeout: Duration,
    ) -> Self {
        Self {
            network,
            allower,
            router,
            mb: MsgBuilder::default(),
            chain_id,
            subnet_id,
            request_timeout,
            compression: Compression::None,
        }
    }

    /// The request timeout as relative nanoseconds, saturating on overflow.
    fn deadline_nanos(&self) -> u64 {
        u64::try_from(self.request_timeout.as_nanos()).unwrap_or(u64::MAX)
    }

    /// Register an outgoing `op_tag` request to each of `nodes` for timeout
    /// tracking (one [`crate::networking::timeout::RequestId`] per recipient).
    fn register(&self, nodes: &HashSet<NodeId>, request_id: u32, op_tag: u8) {
        for &node in nodes {
            self.router
                .register_request(node, self.chain_id, request_id, op_tag);
        }
    }

    fn chain_bytes(&self) -> bytes::Bytes {
        bytes::Bytes::copy_from_slice(self.chain_id.as_bytes())
    }

    /// Marshal `inner` and dispatch it to `node_ids` over the targeted-send
    /// path. Fire-and-forget: a marshal failure is logged, not returned
    /// (matching the Go sender, which swallows enqueue errors and surfaces
    /// non-delivery through the `*Failed` handler callbacks).
    fn send_to(&self, inner: p2p::message::Message, node_ids: HashSet<NodeId>) {
        let cfg = NetSendConfig {
            node_ids,
            ..Default::default()
        };
        self.dispatch(inner, cfg);
    }

    fn dispatch(&self, inner: p2p::message::Message, cfg: NetSendConfig) {
        let m = p2p::Message {
            message: Some(inner),
        };
        match self.mb.create_outbound(&m, self.compression, false) {
            Ok(out) => {
                let _ = self.network.send(out, cfg, self.subnet_id, &*self.allower);
            }
            Err(e) => {
                tracing::warn!(error = %e, "outbound message marshal failed; dropping");
            }
        }
    }

    /// Marshal `inner` and dispatch it over the gossip path. Returns an engine
    /// [`Result`] (the app sends are fallible, matching `ava-vm`'s `AppSender`).
    fn gossip(&self, inner: p2p::message::Message, cfg: GossipConfig) -> Result<()> {
        let m = p2p::Message {
            message: Some(inner),
        };
        let out = self
            .mb
            .create_outbound(&m, self.compression, false)
            .map_err(|e| crate::error::Error::Engine(format!("gossip marshal: {e}")))?;
        let _ = self
            .network
            .gossip(out, self.subnet_id, cfg, &*self.allower);
        Ok(())
    }
}

/// Map the engine-facing [`SendConfig`] to the network's `SendConfig` (the two
/// have identical field shapes — Go has a single `common.SendConfig`).
fn to_net_cfg(c: &SendConfig) -> NetSendConfig {
    NetSendConfig {
        node_ids: c.node_ids.clone(),
        validators: c.validators,
        non_validators: c.non_validators,
        peers: c.peers,
    }
}

fn id_bytes(id: Id) -> bytes::Bytes {
    bytes::Bytes::copy_from_slice(id.as_bytes())
}

fn ids_bytes(ids: &[Id]) -> Vec<bytes::Bytes> {
    ids.iter().map(|id| id_bytes(*id)).collect()
}

#[async_trait]
impl Sender for OutboundSender {
    // --- Frontier / accepted (bootstrap) -----------------------------------

    fn send_get_state_summary_frontier(&self, nodes: &HashSet<NodeId>, req: u32) {
        self.register(nodes, req, op::GET_STATE_SUMMARY_FRONTIER);
        self.send_to(
            p2p::message::Message::GetStateSummaryFrontier(p2p::GetStateSummaryFrontier {
                chain_id: self.chain_bytes(),
                request_id: req,
                deadline: self.deadline_nanos(),
            }),
            nodes.clone(),
        );
    }

    fn send_state_summary_frontier(&self, node: NodeId, req: u32, summary: Vec<u8>) {
        self.send_to(
            p2p::message::Message::StateSummaryFrontier(p2p::StateSummaryFrontier {
                chain_id: self.chain_bytes(),
                request_id: req,
                summary: summary.into(),
            }),
            HashSet::from([node]),
        );
    }

    fn send_get_accepted_state_summary(&self, nodes: &HashSet<NodeId>, req: u32, heights: &[u64]) {
        self.register(nodes, req, op::GET_ACCEPTED_STATE_SUMMARY);
        self.send_to(
            p2p::message::Message::GetAcceptedStateSummary(p2p::GetAcceptedStateSummary {
                chain_id: self.chain_bytes(),
                request_id: req,
                deadline: self.deadline_nanos(),
                heights: heights.to_vec(),
            }),
            nodes.clone(),
        );
    }

    fn send_accepted_state_summary(&self, node: NodeId, req: u32, summary_ids: &[Id]) {
        self.send_to(
            p2p::message::Message::AcceptedStateSummary(p2p::AcceptedStateSummary {
                chain_id: self.chain_bytes(),
                request_id: req,
                summary_ids: ids_bytes(summary_ids),
            }),
            HashSet::from([node]),
        );
    }

    fn send_get_accepted_frontier(&self, nodes: &HashSet<NodeId>, req: u32) {
        tracing::debug!(?nodes, request_id = req, "rung 7: GetAcceptedFrontier broadcast to beacon set (production OutboundSender)");
        self.register(nodes, req, op::GET_ACCEPTED_FRONTIER);
        self.send_to(
            p2p::message::Message::GetAcceptedFrontier(p2p::GetAcceptedFrontier {
                chain_id: self.chain_bytes(),
                request_id: req,
                deadline: self.deadline_nanos(),
            }),
            nodes.clone(),
        );
    }

    fn send_accepted_frontier(&self, node: NodeId, req: u32, container_id: Id) {
        self.send_to(
            p2p::message::Message::AcceptedFrontier(p2p::AcceptedFrontier {
                chain_id: self.chain_bytes(),
                request_id: req,
                container_id: id_bytes(container_id),
            }),
            HashSet::from([node]),
        );
    }

    fn send_get_accepted(&self, nodes: &HashSet<NodeId>, req: u32, ids: &[Id]) {
        self.register(nodes, req, op::GET_ACCEPTED);
        self.send_to(
            p2p::message::Message::GetAccepted(p2p::GetAccepted {
                chain_id: self.chain_bytes(),
                request_id: req,
                deadline: self.deadline_nanos(),
                container_ids: ids_bytes(ids),
            }),
            nodes.clone(),
        );
    }

    fn send_accepted(&self, node: NodeId, req: u32, ids: &[Id]) {
        self.send_to(
            p2p::message::Message::Accepted(p2p::Accepted {
                chain_id: self.chain_bytes(),
                request_id: req,
                container_ids: ids_bytes(ids),
            }),
            HashSet::from([node]),
        );
    }

    // --- Fetch -------------------------------------------------------------

    fn send_get(&self, node: NodeId, req: u32, container_id: Id) {
        self.router
            .register_request(node, self.chain_id, req, op::GET);
        self.send_to(
            p2p::message::Message::Get(p2p::Get {
                chain_id: self.chain_bytes(),
                request_id: req,
                deadline: self.deadline_nanos(),
                container_id: id_bytes(container_id),
            }),
            HashSet::from([node]),
        );
    }

    fn send_get_ancestors(&self, node: NodeId, req: u32, container_id: Id) {
        self.router
            .register_request(node, self.chain_id, req, op::GET_ANCESTORS);
        self.send_to(
            p2p::message::Message::GetAncestors(p2p::GetAncestors {
                chain_id: self.chain_bytes(),
                request_id: req,
                deadline: self.deadline_nanos(),
                container_id: id_bytes(container_id),
                // Snowman engine (Go `ENGINE_TYPE_CHAIN`); the X-Chain DAG path
                // is not used by this port's linear chains.
                engine_type: p2p::EngineType::Chain as i32,
            }),
            HashSet::from([node]),
        );
    }

    fn send_put(&self, node: NodeId, req: u32, container: Vec<u8>) {
        self.send_to(
            p2p::message::Message::Put(p2p::Put {
                chain_id: self.chain_bytes(),
                request_id: req,
                container: container.into(),
            }),
            HashSet::from([node]),
        );
    }

    fn send_ancestors(&self, node: NodeId, req: u32, containers: Vec<Vec<u8>>) {
        self.send_to(
            p2p::message::Message::Ancestors(p2p::Ancestors {
                chain_id: self.chain_bytes(),
                request_id: req,
                containers: containers.into_iter().map(Into::into).collect(),
            }),
            HashSet::from([node]),
        );
    }

    // --- Query / vote ------------------------------------------------------

    fn send_push_query(
        &self,
        nodes: &HashSet<NodeId>,
        req: u32,
        container: Vec<u8>,
        requested_height: u64,
    ) {
        self.register(nodes, req, op::QUERY);
        self.send_to(
            p2p::message::Message::PushQuery(p2p::PushQuery {
                chain_id: self.chain_bytes(),
                request_id: req,
                deadline: self.deadline_nanos(),
                container: container.into(),
                requested_height,
            }),
            nodes.clone(),
        );
    }

    fn send_pull_query(
        &self,
        nodes: &HashSet<NodeId>,
        req: u32,
        container_id: Id,
        requested_height: u64,
    ) {
        self.register(nodes, req, op::QUERY);
        self.send_to(
            p2p::message::Message::PullQuery(p2p::PullQuery {
                chain_id: self.chain_bytes(),
                request_id: req,
                deadline: self.deadline_nanos(),
                container_id: id_bytes(container_id),
                requested_height,
            }),
            nodes.clone(),
        );
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
        self.send_to(
            p2p::message::Message::Chits(p2p::Chits {
                chain_id: self.chain_bytes(),
                request_id: req,
                preferred_id: id_bytes(preferred),
                accepted_id: id_bytes(accepted),
                preferred_id_at_height: id_bytes(preferred_at_height),
                accepted_height,
            }),
            HashSet::from([node]),
        );
    }

    // --- App ---------------------------------------------------------------

    async fn send_app_request(
        &self,
        nodes: &HashSet<NodeId>,
        req: u32,
        bytes: Vec<u8>,
    ) -> Result<()> {
        self.register(nodes, req, op::APP_REQUEST);
        let m = p2p::message::Message::AppRequest(p2p::AppRequest {
            chain_id: self.chain_bytes(),
            request_id: req,
            deadline: self.deadline_nanos(),
            app_bytes: bytes.into(),
        });
        let out = p2p::Message { message: Some(m) };
        let msg = self
            .mb
            .create_outbound(&out, self.compression, false)
            .map_err(|e| crate::error::Error::Engine(format!("app_request marshal: {e}")))?;
        let cfg = NetSendConfig {
            node_ids: nodes.clone(),
            ..Default::default()
        };
        let _ = self.network.send(msg, cfg, self.subnet_id, &*self.allower);
        Ok(())
    }

    async fn send_app_response(&self, node: NodeId, req: u32, bytes: Vec<u8>) -> Result<()> {
        let m = p2p::message::Message::AppResponse(p2p::AppResponse {
            chain_id: self.chain_bytes(),
            request_id: req,
            app_bytes: bytes.into(),
        });
        let out = p2p::Message { message: Some(m) };
        let msg = self
            .mb
            .create_outbound(&out, self.compression, false)
            .map_err(|e| crate::error::Error::Engine(format!("app_response marshal: {e}")))?;
        let cfg = NetSendConfig {
            node_ids: HashSet::from([node]),
            ..Default::default()
        };
        let _ = self.network.send(msg, cfg, self.subnet_id, &*self.allower);
        Ok(())
    }

    async fn send_app_error(&self, node: NodeId, req: u32, code: i32, msg: &str) -> Result<()> {
        let m = p2p::message::Message::AppError(p2p::AppError {
            chain_id: self.chain_bytes(),
            request_id: req,
            error_code: code,
            error_message: msg.to_string(),
        });
        let out = p2p::Message { message: Some(m) };
        let outbound = self
            .mb
            .create_outbound(&out, self.compression, false)
            .map_err(|e| crate::error::Error::Engine(format!("app_error marshal: {e}")))?;
        let cfg = NetSendConfig {
            node_ids: HashSet::from([node]),
            ..Default::default()
        };
        let _ = self
            .network
            .send(outbound, cfg, self.subnet_id, &*self.allower);
        Ok(())
    }

    async fn send_app_gossip(&self, cfg: SendConfig, bytes: Vec<u8>) -> Result<()> {
        let net = to_net_cfg(&cfg);
        self.gossip(
            p2p::message::Message::AppGossip(p2p::AppGossip {
                chain_id: self.chain_bytes(),
                app_bytes: bytes.into(),
            }),
            GossipConfig {
                validators: net.validators,
                non_validators: net.non_validators,
                peers: net.peers,
            },
        )
    }
}
