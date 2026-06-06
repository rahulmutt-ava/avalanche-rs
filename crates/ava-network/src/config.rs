// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PeerConfig` — the per-peer configuration shared (as `Arc<PeerConfig>`) by
//! every peer actor and the `Network` (`specs/05` §3.1).
//!
//! Mirrors the fields of Go's `peer.Config`. This Wave-C scaffolding task
//! (M2.11) defines the always-present fields — the outbound message creator,
//! the router handoff handle, the node identity, and the version-compatibility
//! rule (`specs/26` §3). The remaining collaborators wire in at the tasks that
//! introduce them, to keep this task self-contained:
//!
//! - the **outbound byte throttler** + outbound `MessageQueue` (M2.12),
//! - the **inbound byte / conn-upgrade throttlers** (M2.13),
//! - the **`IpSigner`** + injected `Clock` (consumed by the peer actor, M2.14),
//! - the **`avalanche_network_*` metrics** registry (M2.20).
//!
//! Each is added to this struct as its task lands; see `plan/M2-networking.md`.

use std::sync::Arc;

use ava_message::builder::Creator;
use ava_types::node_id::NodeId;
use ava_version::compatibility::Compatibility;

use crate::router::ExternalHandler;

/// Per-peer configuration shared across the network and every peer actor.
///
/// Cheap to clone via `Arc`; read-only after construction.
pub struct PeerConfig {
    /// The network this node belongs to (mainnet/fuji/local). Echoed in the
    /// Handshake and validated against each peer's Handshake (`specs/05` §1.4).
    pub network_id: u32,

    /// This node's own NodeID (`RIPEMD160(SHA256(leaf_DER))`).
    pub my_node_id: NodeId,

    /// Builds outbound wire messages (`message.Creator`). Shared, lock-free.
    pub creator: Arc<Creator>,

    /// The `06` ChainRouter handoff handle (`specs/05` §3.6). Held as a trait
    /// object — the network has no knowledge of the concrete consensus router.
    pub router: Arc<dyn ExternalHandler>,

    /// The version-compatibility rule applied to every peer at handshake and
    /// re-checked on each net-messages tick (`specs/26` §3; Go
    /// `version.Compatibility`). A peer on a newer major, or below the
    /// clock-selected floor, is rejected.
    pub version_compatibility: Arc<Compatibility>,
}

impl PeerConfig {
    /// Constructs a `PeerConfig` from its always-present collaborators.
    #[must_use]
    pub fn new(
        network_id: u32,
        my_node_id: NodeId,
        creator: Arc<Creator>,
        router: Arc<dyn ExternalHandler>,
        version_compatibility: Arc<Compatibility>,
    ) -> Self {
        Self {
            network_id,
            my_node_id,
            creator,
            router,
            version_compatibility,
        }
    }
}
