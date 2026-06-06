// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Test-support for spinning up a real [`NetworkImpl`] on loopback (`specs/02`).
//! Not part of the production API surface; carries no stability guarantees.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::net::SocketAddr;
use std::sync::Arc;

use ava_message::builder::Creator;
use ava_message::codec::MsgBuilder;
use ava_types::node_id::NodeId;
use ava_version::compatibility::Compatibility;
use ava_version::Application;
use tokio::net::TcpListener;

use crate::config::PeerConfig;
use crate::network::ip_tracker::IpTracker;
use crate::network::net_impl::NetworkImpl;
use crate::peer::ip_signer::{Clock, IpSigner, SystemClock};
use crate::peer::testutil::RecordingRouter;
use crate::throttling::inbound_msg_byte::InboundMsgByteThrottler;
use crate::throttling::outbound_msg::{OutboundMsgThrottler, OutboundMsgThrottlerConfig};
use crate::Identity;

/// A live `NetworkImpl` bound to a loopback TCP port, for integration tests.
pub struct TestNetwork {
    network: Arc<NetworkImpl>,
    node_id: NodeId,
    listen_addr: SocketAddr,
    router: Arc<RecordingRouter>,
}

impl TestNetwork {
    /// Start a network on an ephemeral loopback port.
    pub async fn start() -> TestNetwork {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let listen_addr = listener.local_addr().expect("local addr");

        let identity = Identity::generate().expect("identity");
        let cert = ava_crypto::staking::parse_certificate(identity.cert_der()).expect("cert");
        let node_id = ava_crypto::staking::node_id_from_cert(&cert.raw);

        let bls = Arc::new(ava_crypto::bls::LocalSigner::generate().expect("bls"));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ip_signer = Arc::new(IpSigner::new(identity.clone(), bls, clock.clone()));
        let creator = Arc::new(Creator::new(MsgBuilder::default()));
        let router = Arc::new(RecordingRouter::default());

        // Upgrade far in the future: the pre-upgrade floor applies.
        let upgrade_time =
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(4_000_000_000);
        let compat = Arc::new(Compatibility::new(
            Application::new("avalanchego", 1, 14, 2),
            Application::new("avalanchego", 1, 14, 0),
            Application::new("avalanchego", 1, 13, 0),
            upgrade_time,
        ));

        let outbound = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig::default());
        let inbound = Arc::new(InboundMsgByteThrottler::new(
            32 * 1024 * 1024,
            6 * 1024 * 1024,
            2 * 1024 * 1024,
        ));
        let ip_tracker = Arc::new(IpTracker::new());

        let peer_config = Arc::new(PeerConfig::new(
            1,
            node_id,
            identity,
            listen_addr,
            Application::new("avalanchego", 1, 14, 2),
            creator,
            router.clone(),
            compat,
            ip_signer,
            outbound,
            inbound,
            ip_tracker,
            clock,
        ));

        let network = NetworkImpl::new(peer_config, listener).expect("network");

        TestNetwork {
            network,
            node_id,
            listen_addr,
            router,
        }
    }

    /// The shared network handle.
    #[must_use]
    pub fn network(&self) -> &Arc<NetworkImpl> {
        &self.network
    }

    /// This network's NodeID.
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// The loopback address the listener is bound to.
    #[must_use]
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    /// The recording router (so tests can assert `connected`/`disconnected`).
    #[must_use]
    pub fn router(&self) -> Arc<RecordingRouter> {
        Arc::clone(&self.router)
    }
}
