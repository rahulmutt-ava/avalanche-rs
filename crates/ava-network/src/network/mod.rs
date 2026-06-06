// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `Network` trait surface (`specs/05` §3.1).
//!
//! Mirrors Go's `network.Network` interface — the outward API consumed by
//! `06`'s sender (`send`/`gossip`), by the node assembly (`dispatch`/
//! `start_close`/`manually_track`), and by the health/info endpoints
//! (`peer_info`/`node_uptime`).
//!
//! M2.11 defines the trait and its parameter/return types. The concrete
//! `NetworkImpl` (listener + dialer + peer set + runTimers) lands in M2.18;
//! until then the trait stands alone so `06`/the node can be written against
//! it. No method bodies live here (it is a trait), so the
//! no-`todo!()`/no-`unwrap()` library rules hold trivially.

pub mod bloom;
pub mod ip_tracker;
pub mod net_impl;
pub mod peer_set;
pub mod testutil;
pub mod tracked_ip;

pub use net_impl::NetworkImpl;

use std::collections::HashSet;
use std::net::SocketAddr;

use ava_message::codec::OutboundMessage;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::Result;

/// Decides whether a given node is an eligible recipient for a `send`/`gossip`
/// (Go `subnets.Allower`). Held as a trait object on the send path.
pub trait Allower: Send + Sync {
    /// Returns `true` if `node_id` is allowed to receive the message.
    fn is_allowed(&self, node_id: &NodeId) -> bool;
}

/// Targeted-send selection (Go `common.SendConfig`): an explicit node set plus
/// optional sampling over validators/non-validators/peers.
#[derive(Debug, Default, Clone)]
pub struct SendConfig {
    /// Explicit recipients.
    pub node_ids: HashSet<NodeId>,
    /// Number of validators to additionally sample.
    pub validators: usize,
    /// Number of non-validators to additionally sample.
    pub non_validators: usize,
    /// Number of arbitrary peers to additionally sample.
    pub peers: usize,
}

/// Gossip fan-out selection (Go gossip size knobs).
#[derive(Debug, Default, Clone)]
pub struct GossipConfig {
    /// Number of validators to gossip to.
    pub validators: usize,
    /// Number of non-validators to gossip to.
    pub non_validators: usize,
    /// Number of arbitrary peers to gossip to.
    pub peers: usize,
}

/// A connected peer's externally-visible info (Go `peer.Info`, trimmed to the
/// fields the info/health endpoints surface; extended as later tasks need).
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// The peer's NodeID.
    pub node_id: NodeId,
    /// The peer's observed public IP:port.
    pub ip: SocketAddr,
    /// The peer's reported version string.
    pub version: String,
    /// Whether the connection was inbound (peer dialed us).
    pub is_ingress: bool,
}

/// Result of a node-uptime query (Go `UptimeResult`).
#[derive(Debug, Clone, Copy, Default)]
pub struct UptimeResult {
    /// Uptime as weighted by stake, in `[0, 1]`.
    pub weighted_average_percentage: f64,
    /// Uptime as a simple peer average, in `[0, 1]`.
    pub rewarding_stake_percentage: f64,
}

/// The networking runtime surface (`specs/05` §3.1). Implemented by
/// `NetworkImpl` (M2.18).
#[async_trait::async_trait]
pub trait Network: Send + Sync {
    /// Run until closed or a fatal error (mirrors Go `Dispatch()`).
    async fn dispatch(self: std::sync::Arc<Self>) -> Result<()>;

    /// Begin a graceful shutdown. Idempotent.
    fn start_close(&self);

    /// Manually pin a node's IP so the dialer keeps reconnecting to it.
    fn manually_track(&self, node_id: NodeId, ip: SocketAddr);

    /// Info for the given nodes (all connected peers if `node_ids` is empty).
    fn peer_info(&self, node_ids: &[NodeId]) -> Vec<PeerInfo>;

    /// This node's uptime as observed by its peers.
    fn node_uptime(&self) -> Result<UptimeResult>;

    /// Send `msg` to the selected nodes (Go `ExternalSender.Send`). Returns the
    /// set of nodes the message was actually queued to.
    fn send(
        &self,
        msg: OutboundMessage,
        cfg: SendConfig,
        subnet: Id,
        allower: &dyn Allower,
    ) -> HashSet<NodeId>;

    /// Gossip `msg` to a sampled subset of `subnet` peers (Go
    /// `ExternalSender.Gossip`). Returns the set of nodes it was queued to.
    fn gossip(
        &self,
        msg: OutboundMessage,
        subnet: Id,
        cfg: GossipConfig,
        allower: &dyn Allower,
    ) -> HashSet<NodeId>;
}
