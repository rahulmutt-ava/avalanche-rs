// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PeerConfig` — the per-peer configuration shared (as `Arc<PeerConfig>`) by
//! every peer actor and the `Network` (`specs/05` §3.1).
//!
//! Mirrors the fields of Go's `peer.Config`. The Wave-C scaffolding task (M2.11)
//! defined the always-present fields; M2.14+ extends the struct with the
//! collaborators the peer actor needs (the throttlers, the `IpSigner`, the
//! injected `Clock`, our own advertised handshake fields, and the timing
//! constants), added here rather than forward-declared with placeholder types.
//!
//! Still deferred: the `avalanche_network_*` metrics registry (M2.20) and the
//! validator-set / uptime-calculator handles (wired when their sources land).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ava_message::builder::Creator;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::compatibility::Compatibility;

use crate::identity::Identity;
use crate::network::ip_tracker::IpTracker;
use crate::peer::ip_signer::{Clock, IpSigner};
use crate::router::ExternalHandler;
use crate::throttling::inbound_msg_byte::InboundMsgByteThrottler;
use crate::throttling::outbound_msg::OutboundMsgThrottler;

/// Maximum tolerated difference between our clock and a peer's `my_time`
/// (Go `maxClockDifference = time.Minute`).
pub const MAX_CLOCK_DIFFERENCE: Duration = Duration::from_secs(60);

/// How often the net-messages task ticks: send a `Ping` and re-check
/// compatibility. `PingFrequency = 3/4 * PingPongTimeout (30s) = 22.5s`
/// (Go `constants.DefaultPingFrequency`).
pub const PING_FREQUENCY: Duration = Duration::from_millis(22_500);

/// Read deadline a peer must respond within (Go `PingPongTimeout = 30s`). The
/// read task wraps each frame read in this timeout; a peer that goes silent is
/// dropped.
pub const PONG_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of subnets a peer may advertise in its Handshake
/// (Go `maxNumTrackedSubnets = 16`).
pub const MAX_NUM_TRACKED_SUBNETS: usize = 16;

/// Maximum bloom-filter salt length accepted in a Handshake / GetPeerList
/// (Go `maxBloomSaltLen = 32`).
pub const MAX_BLOOM_SALT_LEN: usize = 32;

/// Per-peer configuration shared across the network and every peer actor.
///
/// Cheap to clone via `Arc`; read-only after construction.
pub struct PeerConfig {
    /// The network this node belongs to (mainnet/fuji/local). Echoed in the
    /// Handshake and validated against each peer's Handshake (`specs/05` §1.4).
    pub network_id: u32,

    /// This node's own NodeID (`RIPEMD160(SHA256(leaf_DER))`).
    pub my_node_id: NodeId,

    /// This node's local staking identity (cert + key), the TLS signer for the
    /// signed IP.
    pub identity: Identity,

    /// Our advertised public IP:port (the address carried in our Handshake and
    /// signed by the `IpSigner`).
    pub my_ip: SocketAddr,

    /// Our reported client version triple + name (`specs/26` §2).
    pub my_version: ava_version::Application,

    /// The subnets this node tracks (advertised in our Handshake, ≤16).
    pub my_tracked_subnets: Vec<Id>,

    /// ACPs this node supports (advertised in our Handshake).
    pub my_supported_acps: Vec<u32>,
    /// ACPs this node objects to.
    pub my_objected_acps: Vec<u32>,

    /// Builds outbound wire messages (`message.Creator`). Shared, lock-free.
    pub creator: Arc<Creator>,

    /// The `06` ChainRouter handoff handle (`specs/05` §3.6). Held as a trait
    /// object — the network has no knowledge of the concrete consensus router.
    pub router: Arc<dyn ExternalHandler>,

    /// The version-compatibility rule applied to every peer at handshake and
    /// re-checked on each net-messages tick (`specs/26` §3; Go
    /// `version.Compatibility`). A peer on a newer major, or below the
    /// clock-selected floor, is rejected. The floor is selected with this
    /// config's injected [`PeerConfig::clock`] rather than the wall clock so
    /// the fork-boundary cut-over is testable (`specs/26` §3.1).
    pub version_compatibility: Arc<Compatibility>,

    /// Caches our current `SignedIp`, re-signing on IP change (`specs/05` §3.5).
    pub ip_signer: Arc<IpSigner>,

    /// Outbound message byte throttler (`specs/05` §5; M2.12).
    pub outbound_msg_throttler: OutboundMsgThrottler,

    /// Inbound message byte throttler (`specs/05` §5; M2.13).
    pub inbound_msg_throttler: Arc<InboundMsgByteThrottler>,

    /// Shared IP-tracker / peer-list-gossip state (`specs/05` §3.5; M2.17).
    pub ip_tracker: Arc<IpTracker>,

    /// Injected clock: source of `my_time`, the clock-skew bound, and the
    /// compatibility floor selection. Tests inject a controllable clock.
    pub clock: Arc<dyn Clock>,

    /// Net-messages tick / ping interval. Defaults to [`PING_FREQUENCY`].
    pub ping_frequency: Duration,

    /// Read deadline per frame. Defaults to [`PONG_TIMEOUT`].
    pub pong_timeout: Duration,
}

impl PeerConfig {
    /// Constructs a `PeerConfig` from its full collaborator set.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network_id: u32,
        my_node_id: NodeId,
        identity: Identity,
        my_ip: SocketAddr,
        my_version: ava_version::Application,
        creator: Arc<Creator>,
        router: Arc<dyn ExternalHandler>,
        version_compatibility: Arc<Compatibility>,
        ip_signer: Arc<IpSigner>,
        outbound_msg_throttler: OutboundMsgThrottler,
        inbound_msg_throttler: Arc<InboundMsgByteThrottler>,
        ip_tracker: Arc<IpTracker>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            network_id,
            my_node_id,
            identity,
            my_ip,
            my_version,
            my_tracked_subnets: Vec::new(),
            my_supported_acps: Vec::new(),
            my_objected_acps: Vec::new(),
            creator,
            router,
            version_compatibility,
            ip_signer,
            outbound_msg_throttler,
            inbound_msg_throttler,
            ip_tracker,
            clock,
            ping_frequency: PING_FREQUENCY,
            pong_timeout: PONG_TIMEOUT,
        }
    }
}
