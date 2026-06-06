// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The engine-facing [`Sender`] trait + [`SendConfig`] (port of
//! `snow/networking/sender/sender.go`'s engine surface, specs 06 §5.3).
//!
//! The concrete `OutboundSender` (a later task) implements this by translating
//! each `send_*` call into wire messages (specs 05), choosing recipients
//! (validator sampling / [`SendConfig`]), registering the request + deadline with
//! the `TimeoutManager`, and handing bytes to the network. Only **engines** see
//! this trait; the VM binding boundary (specs 07) sees only `ava-vm`'s
//! `AppSender`.

use std::collections::HashSet;

use async_trait::async_trait;

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::Result;

/// `snow/engine/common.SendConfig` — who to send a gossip message to over p2p.
///
/// Mirrors the field shape of `ava-vm`'s `SendConfig` exactly so the two
/// round-trip identically over `proto/appsender`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendConfig {
    /// Explicit set of nodes to send to.
    pub node_ids: HashSet<NodeId>,
    /// If `>=` the number of connected validators, send to all connected
    /// validators.
    pub validators: usize,
    /// If `>=` the number of connected non-validators, send to all connected
    /// non-validators.
    pub non_validators: usize,
    /// If `>=` the number of connected peers, send to all connected peers.
    pub peers: usize,
}

/// `snow/networking/sender.Sender` — the outbound side the consensus engine
/// drives. Each `send_*` becomes a wire message addressed to the chosen
/// recipient(s); request ops register a deadline with the `TimeoutManager`.
///
/// The bootstrap/query/fetch sends are fire-and-forget (`fn`, no `Result`),
/// matching Go, where the sender swallows enqueue errors and surfaces failures
/// through the `*Failed` handler callbacks. The app sends are `async` and
/// fallible, matching `ava-vm`'s `AppSender`.
#[async_trait]
pub trait Sender: Send + Sync {
    // --- Frontier / accepted (bootstrap) -----------------------------------

    /// `SendGetStateSummaryFrontier`.
    fn send_get_state_summary_frontier(&self, nodes: &HashSet<NodeId>, req: u32);
    /// `SendStateSummaryFrontier`.
    fn send_state_summary_frontier(&self, node: NodeId, req: u32, summary: Vec<u8>);
    /// `SendGetAcceptedStateSummary`.
    fn send_get_accepted_state_summary(&self, nodes: &HashSet<NodeId>, req: u32, heights: &[u64]);
    /// `SendAcceptedStateSummary`.
    fn send_accepted_state_summary(&self, node: NodeId, req: u32, summary_ids: &[Id]);

    /// `SendGetAcceptedFrontier`.
    fn send_get_accepted_frontier(&self, nodes: &HashSet<NodeId>, req: u32);
    /// `SendAcceptedFrontier`.
    fn send_accepted_frontier(&self, node: NodeId, req: u32, container_id: Id);
    /// `SendGetAccepted`.
    fn send_get_accepted(&self, nodes: &HashSet<NodeId>, req: u32, ids: &[Id]);
    /// `SendAccepted`.
    fn send_accepted(&self, node: NodeId, req: u32, ids: &[Id]);

    // --- Fetch -------------------------------------------------------------

    /// `SendGet`.
    fn send_get(&self, node: NodeId, req: u32, container_id: Id);
    /// `SendGetAncestors`.
    fn send_get_ancestors(&self, node: NodeId, req: u32, container_id: Id);
    /// `SendPut`.
    fn send_put(&self, node: NodeId, req: u32, container: Vec<u8>);
    /// `SendAncestors`.
    fn send_ancestors(&self, node: NodeId, req: u32, containers: Vec<Vec<u8>>);

    // --- Query / vote ------------------------------------------------------

    /// `SendPushQuery`.
    fn send_push_query(
        &self,
        nodes: &HashSet<NodeId>,
        req: u32,
        container: Vec<u8>,
        requested_height: u64,
    );
    /// `SendPullQuery`.
    fn send_pull_query(
        &self,
        nodes: &HashSet<NodeId>,
        req: u32,
        container_id: Id,
        requested_height: u64,
    );
    /// `SendChits`.
    fn send_chits(
        &self,
        node: NodeId,
        req: u32,
        preferred: Id,
        preferred_at_height: Id,
        accepted: Id,
        accepted_height: u64,
    );

    // --- App ---------------------------------------------------------------

    /// `SendAppRequest`.
    async fn send_app_request(
        &self,
        nodes: &HashSet<NodeId>,
        req: u32,
        bytes: Vec<u8>,
    ) -> Result<()>;
    /// `SendAppResponse`.
    async fn send_app_response(&self, node: NodeId, req: u32, bytes: Vec<u8>) -> Result<()>;
    /// `SendAppError`.
    async fn send_app_error(&self, node: NodeId, req: u32, code: i32, msg: &str) -> Result<()>;
    /// `SendAppGossip`.
    async fn send_app_gossip(&self, cfg: SendConfig, bytes: Vec<u8>) -> Result<()>;
}
