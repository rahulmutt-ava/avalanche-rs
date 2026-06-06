// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.11 — object-safety of the network→consensus handoff traits and the
//! `PeerConfig` version-compatibility wiring (`specs/05` §3.1/§3.6, `26` §3).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ava_message::builder::Creator;
use ava_message::codec::{InboundMessage, MsgBuilder};
use ava_network::config::PeerConfig;
use ava_network::router::{AppVersion, ExternalHandler, InboundHandler};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::Application;
use ava_version::compatibility::Compatibility;
use tokio_util::sync::CancellationToken;

/// A no-op router used to prove the traits are object-safe and have the exact
/// `specs/05` §3.6 method signatures.
struct TestHandler;

#[async_trait::async_trait]
impl InboundHandler for TestHandler {
    async fn handle_inbound(&self, _ctx: &CancellationToken, _msg: InboundMessage) {}
}

#[async_trait::async_trait]
impl ExternalHandler for TestHandler {
    fn connected(&self, _node_id: NodeId, _version: &AppVersion, _subnet_id: Id) {}
    fn disconnected(&self, _node_id: NodeId) {}
}

#[test]
fn inbound_handler_object_safe() {
    // The contract: `06`'s ChainRouter is held as a trait object by every Peer.
    let handler: Arc<dyn ExternalHandler> = Arc::new(TestHandler);
    // `ExternalHandler: InboundHandler`, so it coerces to the narrower object too.
    let _inbound: Arc<dyn InboundHandler> = handler.clone();
}

/// Builds a `PeerConfig` whose floor switch has not yet fired (upgrade far in
/// the future), so the floor is `min_compatible`.
fn test_peer_config() -> PeerConfig {
    let creator = Arc::new(Creator::new(MsgBuilder::default()));
    let current = Application::new("avalanchego", 1, 14, 2);
    let min_compatible = Application::new("avalanchego", 1, 14, 0);
    let min_after = Application::new("avalanchego", 1, 14, 0);
    let upgrade_time = SystemTime::now()
        .checked_add(Duration::from_secs(365 * 24 * 60 * 60))
        .expect("upgrade_time in range");
    let compat = Arc::new(Compatibility::new(
        current,
        min_after,
        min_compatible,
        upgrade_time,
    ));
    PeerConfig::new(1, NodeId::default(), creator, Arc::new(TestHandler), compat)
}

#[test]
fn compatibility_floor_rule() {
    let cfg = test_peer_config();

    // Equal-version peer is accepted.
    let equal = Application::new("avalanchego", 1, 14, 2);
    assert!(cfg.version_compatibility.compatible(&equal));

    // A peer at exactly the floor is accepted.
    let at_floor = Application::new("avalanchego", 1, 14, 0);
    assert!(cfg.version_compatibility.compatible(&at_floor));

    // A peer below the floor is rejected.
    let below = Application::new("avalanchego", 1, 13, 9);
    assert!(!cfg.version_compatibility.compatible(&below));

    // A peer on a newer major is rejected (clause 1, clock-independent).
    let newer_major = Application::new("avalanchego", 2, 0, 0);
    assert!(!cfg.version_compatibility.compatible(&newer_major));
}
