// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Test-support helpers for driving a [`Peer`] actor without a live TLS stack
//! (`specs/02` testing strategy). Used by the `peer_actor` / `handshake` /
//! `ping_pong` integration tests. Not part of the production API surface.
//!
//! These helpers are deliberately public (the integration tests live in a
//! separate crate) but carry no stability guarantees.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ava_message::builder::Creator;
use ava_message::codec::MsgBuilder;
use ava_message::frame::{read_msg_len, MAX_MESSAGE_SIZE};
use ava_types::node_id::NodeId;
use ava_version::compatibility::Compatibility;
use ava_version::Application;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::PeerConfig;
use crate::peer::handle::PeerHandle;
use crate::peer::ip_signer::{Clock, IpSigner};
use crate::peer::peer::{Direction, Peer};
use crate::router::{AppVersion, ExternalHandler, InboundHandler};
use crate::throttling::inbound_msg_byte::InboundMsgByteThrottler;
use crate::throttling::outbound_msg::{OutboundMsgThrottler, OutboundMsgThrottlerConfig};
use crate::Identity;

/// A controllable Unix-seconds clock for deterministic tests.
#[derive(Debug, Default)]
pub struct TestClock {
    secs: AtomicU64,
}

impl TestClock {
    /// A clock fixed at `secs` Unix-seconds.
    #[must_use]
    pub fn new(secs: u64) -> Self {
        Self {
            secs: AtomicU64::new(secs),
        }
    }

    /// Set the current time to `secs`.
    pub fn set(&self, secs: u64) {
        self.secs.store(secs, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn unix(&self) -> u64 {
        self.secs.load(Ordering::SeqCst)
    }
}

/// A router that records `connected` / `disconnected` / inbound calls.
#[derive(Default)]
pub struct RecordingRouter {
    /// NodeIDs `connected` was called for.
    pub connected: parking_lot::Mutex<Vec<NodeId>>,
    /// NodeIDs `disconnected` was called for.
    pub disconnected: parking_lot::Mutex<Vec<NodeId>>,
}

impl RecordingRouter {
    /// How many times `connected` fired.
    #[must_use]
    pub fn connected_count(&self) -> usize {
        self.connected.lock().len()
    }
}

#[async_trait::async_trait]
impl InboundHandler for RecordingRouter {
    async fn handle_inbound(
        &self,
        _ctx: &CancellationToken,
        _msg: ava_message::codec::InboundMessage,
    ) {
    }
}

#[async_trait::async_trait]
impl ExternalHandler for RecordingRouter {
    fn connected(&self, node_id: NodeId, _version: &AppVersion, _subnet_id: ava_types::id::Id) {
        self.connected.lock().push(node_id);
    }

    fn disconnected(&self, node_id: NodeId) {
        self.disconnected.lock().push(node_id);
    }
}

/// Builds a self-contained [`PeerConfig`] + spawns a [`Peer`] over an in-process
/// duplex stream, for tests.
pub struct TestPeerBuilder {
    network_id: u32,
    clock: Arc<TestClock>,
    version: Application,
    upgrade_time: std::time::SystemTime,
    router: Arc<RecordingRouter>,
}

impl Default for TestPeerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestPeerBuilder {
    /// A builder with sensible interop-default fields.
    #[must_use]
    pub fn new() -> Self {
        Self {
            network_id: 1,
            clock: Arc::new(TestClock::new(1_700_000_000)),
            version: Application::new("avalanchego", 1, 14, 2),
            // Upgrade far in the future: the pre-upgrade floor applies.
            upgrade_time: std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(4_000_000_000),
            router: Arc::new(RecordingRouter::default()),
        }
    }

    /// Override the network id (for the network-mismatch disconnect test).
    #[must_use]
    pub fn network_id(mut self, id: u32) -> Self {
        self.network_id = id;
        self
    }

    /// Override the reported version.
    #[must_use]
    pub fn version(mut self, v: Application) -> Self {
        self.version = v;
        self
    }

    /// Override the compatibility upgrade time.
    #[must_use]
    pub fn upgrade_time(mut self, t: std::time::SystemTime) -> Self {
        self.upgrade_time = t;
        self
    }

    /// The shared clock (so tests can advance time).
    #[must_use]
    pub fn clock(&self) -> Arc<TestClock> {
        Arc::clone(&self.clock)
    }

    /// The recording router (so tests can assert `connected`).
    #[must_use]
    pub fn router(&self) -> Arc<RecordingRouter> {
        Arc::clone(&self.router)
    }

    /// Build the shared [`PeerConfig`].
    #[must_use]
    pub fn build_config(&self) -> Arc<PeerConfig> {
        let identity = Identity::generate().expect("generate identity");
        let bls = Arc::new(ava_crypto::bls::LocalSigner::generate().expect("bls signer"));
        let clock: Arc<dyn Clock> = self.clock.clone();
        let ip_signer = Arc::new(IpSigner::new(identity.clone(), bls, clock.clone()));
        let creator = Arc::new(Creator::new(MsgBuilder::default()));

        let min_compatible = Application::new("avalanchego", 1, 14, 0);
        let min_after = Application::new("avalanchego", 1, 14, 0);
        let compat = Arc::new(Compatibility::new(
            self.version.clone(),
            min_after,
            min_compatible,
            self.upgrade_time,
        ));

        let outbound = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig::default());
        let inbound = Arc::new(InboundMsgByteThrottler::new(
            32 * 1024 * 1024,
            6 * 1024 * 1024,
            2 * 1024 * 1024,
        ));

        let my_ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9651);

        Arc::new(PeerConfig::new(
            self.network_id,
            NodeId::default(),
            identity,
            my_ip,
            self.version.clone(),
            creator,
            self.router.clone(),
            compat,
            ip_signer,
            outbound,
            inbound,
            clock,
        ))
    }

    /// Spawn a peer over a fresh duplex; returns the *remote* end of the duplex
    /// (so the test can act as the peer) and the [`PeerHandle`].
    #[must_use]
    pub fn spawn_over_duplex(self) -> (DuplexStream, PeerHandle) {
        let cfg = self.build_config();
        let (local, remote) = tokio::io::duplex(1 << 20);

        // A real peer cert is needed for the signed-IP verification path; use a
        // fresh staking identity's cert as the (placeholder) peer cert.
        let peer_cert = ava_crypto::staking::parse_certificate(
            Identity::generate().expect("peer identity").cert_der(),
        )
        .expect("parse peer cert");
        let peer_id = NodeId::from_slice(&[7u8; 20]).expect("peer id");

        let net_token = CancellationToken::new();
        let tracker = TaskTracker::new();
        let handle = Peer::spawn(
            cfg,
            peer_id,
            peer_cert,
            Direction::Inbound,
            local,
            &net_token,
            &tracker,
        );
        tracker.close();
        (remote, handle)
    }
}

/// Read exactly one length-prefixed frame from `stream` and return its payload.
///
/// # Errors
/// Returns an [`std::io::Error`] on EOF / a malformed prefix / an oversized
/// length.
pub async fn read_one_frame<R>(stream: &mut R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = read_msg_len(len_buf, MAX_MESSAGE_SIZE)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Write a length-prefixed frame (`len_be || payload`) to `stream`.
///
/// # Errors
/// Returns an [`std::io::Error`] on a write failure or oversized payload.
pub async fn write_one_frame<W>(stream: &mut W, payload: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let len = u32::try_from(payload.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "frame too large"))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}
