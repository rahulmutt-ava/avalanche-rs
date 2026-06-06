// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The outbound app-message handle handed to the VM at `initialize`
//! (`snow/engine/common.AppSender`, specs 07 §2.6).

use std::collections::HashSet;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::node_id::NodeId;

use crate::error::Result;

/// `snow/engine/common.SendConfig` — specifies who to send a gossip message to
/// over the p2p network.
///
/// Defined locally (rather than re-exported from the consensus `Sender` crate)
/// to keep `ava-vm` free of a networking dependency; the field shape mirrors Go
/// exactly so it round-trips over `proto/appsender`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendConfig {
    /// Explicit set of nodes to send to.
    pub node_ids: HashSet<NodeId>,
    /// If `>=` the number of connected validators, the message is sent to all
    /// connected validators.
    pub validators: usize,
    /// If `>=` the number of connected non-validators, the message is sent to
    /// all connected non-validators.
    pub non_validators: usize,
    /// If `>=` the number of connected peers, the message is sent to all
    /// connected peers.
    pub peers: usize,
}

/// `snow/engine/common.AppSender` — the VM-facing subset of the consensus
/// `Sender`, handed to the VM at `initialize` (specs 07 §2.6).
///
/// The VM never sees the full consensus `Sender`; it only gets this app-message
/// surface. Go's `context.Context` becomes a `&CancellationToken`.
#[async_trait]
pub trait AppSender: Send + Sync {
    /// Send an application-level request to `nodes`. A successful return
    /// guarantees the VM will receive exactly one `AppResponse` or
    /// `AppRequestFailed` per node with this `request_id`.
    async fn send_app_request(
        &self,
        token: &CancellationToken,
        nodes: &HashSet<NodeId>,
        request_id: u32,
        bytes: Vec<u8>,
    ) -> Result<()>;

    /// Send an application-level response to an `AppRequest` previously received
    /// from `node` with this `request_id`.
    async fn send_app_response(
        &self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        bytes: Vec<u8>,
    ) -> Result<()>;

    /// Send an application-level error in response to an `AppRequest` from
    /// `node` with this `request_id`. `code`/`message` mirror
    /// [`AppError`](crate::app::AppError).
    async fn send_app_error(
        &self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        code: i32,
        message: &str,
    ) -> Result<()>;

    /// Gossip an application-level message to the peers selected by `config`.
    async fn send_app_gossip(
        &self,
        token: &CancellationToken,
        config: SendConfig,
        bytes: Vec<u8>,
    ) -> Result<()>;
}
