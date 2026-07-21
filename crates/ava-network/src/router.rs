// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The network→consensus handoff traits (`specs/05` §3.6).
//!
//! This is the **cross-spec contract** between `ava-network` and
//! `ava-engine`/`ava-snow` (`specs/06`): every `Peer` holds an
//! `Arc<dyn InboundHandler>` (the `06` ChainRouter) and calls
//! [`InboundHandler::handle_inbound`] for each fully-parsed, non-handshake op
//! once the handshake has finished. The `Network` invokes
//! [`ExternalHandler::connected`]/[`ExternalHandler::disconnected`] from the
//! peer-set bookkeeping (Go `snow/networking/router/chain_router.go`).
//!
//! Mirrors Go `snow/networking/router/inbound_handler.go` + the
//! `ExternalHandler` surface. Keeping the two traits object-safe is a hard
//! requirement: the network stores them as trait objects with no knowledge of
//! the concrete `06` router.

use ava_message::codec::InboundMessage;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use tokio_util::sync::CancellationToken;

/// The peer's reported application version (Go `version.Application`). Re-exported
/// from `ava-version` so `06`/`05` share one type on the handoff boundary.
pub type AppVersion = ava_version::Application;

/// Handles a fully-parsed inbound consensus/app message.
///
/// Implemented by `06`'s ChainRouter; held by every [`crate::peer`] actor. THE
/// source of truth for the network→consensus boundary — `06` MUST implement
/// exactly this signature. The Go `context.Background()` handed to the router
/// becomes the peer/network [`CancellationToken`].
#[async_trait::async_trait]
pub trait InboundHandler: Send + Sync {
    /// Process one inbound message. The handler takes ownership of `msg`; its
    /// `OnFinished` drop-guard releases the inbound throttler permit when the
    /// router is done with it (`specs/05` §3.6).
    async fn handle_inbound(&self, ctx: &CancellationToken, msg: InboundMessage);
}

/// Adds peer lifecycle to [`InboundHandler`]. The `Network` calls these; `06`'s
/// ChainRouter implements it.
///
/// **Ordering note (review follow-up on Task 8):** both methods are `async`
/// and MUST be awaited by the caller before it proceeds — Go's
/// `chain_router.Connected`/`Disconnected` push into every chain handler
/// synchronously before returning, and callers here (`Peer::finish_handshake`,
/// `NetworkImpl::spawn_watcher`) rely on that same per-call
/// completion-before-return: a peer's `Connected` notification must be fully
/// delivered before any of that peer's later inbound ops are forwarded, or a
/// consensus engine could observe an inbound op from a node it hasn't been
/// told is `Connected` yet. A detached (`tokio::spawn`-and-forget) impl breaks
/// this guarantee even though the trait signature can't see it — see
/// `ava_engine::networking::router::ChainRouter::connected`/`disconnected` for
/// the concrete fix.
#[async_trait::async_trait]
pub trait ExternalHandler: InboundHandler {
    /// A peer finished its handshake and shares `subnet_id` with us. Invoked once
    /// per tracked subnet the peer shares (Go iterates subnets).
    async fn connected(&self, node_id: NodeId, version: &AppVersion, subnet_id: Id);

    /// A peer disconnected (its last actor task dropped).
    async fn disconnected(&self, node_id: NodeId);
}
