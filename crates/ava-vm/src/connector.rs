// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Peer connect/disconnect notifications (`snow/validators.Connector`,
//! specs 07 §2.2).

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::node_id::NodeId;
use ava_version::application::Application;

use crate::error::Result;

/// `snow/validators.Connector` — a handler called when a peer connection is
/// marked connected or disconnected.
///
/// The `version.Application` Go passes on `Connected` becomes
/// [`ava_version::application::Application`]; `context.Context` becomes a
/// `&CancellationToken`.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Called when `node` connects, carrying the peer's advertised application
    /// version.
    async fn connected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> Result<()>;

    /// Called when `node` disconnects.
    async fn disconnected(&mut self, token: &CancellationToken, node: NodeId) -> Result<()>;
}
