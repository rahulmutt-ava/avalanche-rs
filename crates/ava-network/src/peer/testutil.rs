// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Test-support helpers for driving a [`Peer`] actor without a live TLS stack
//! (`specs/02` testing strategy). Used by the `peer_actor` / `handshake` /
//! `ping_pong` integration tests. Not part of the production API surface.
//!
//! These helpers are deliberately public (the integration tests live in a
//! separate crate) but carry no stability guarantees. As test-only support
//! code, this module opts out of the lib-grade clippy bars (`expect`/indexing/
//! arithmetic) the same way the integration-test files do.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

// `proptest` is a dev-dependency used only by the `tests/prop_handshake.rs`
// integration target (M2.21). The crate's `lib test` build links every
// dev-dependency, and `unused_crate_dependencies` would otherwise flag it as
// unused there; reference it here (test builds only) to satisfy the lint.
#[cfg(test)]
use proptest as _;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ava_message::builder::Creator;
use ava_message::codec::MsgBuilder;
use ava_message::frame::{MAX_MESSAGE_SIZE, read_msg_len};
use ava_types::node_id::NodeId;
use ava_version::Application;
use ava_version::compatibility::Compatibility;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::Identity;
use crate::config::PeerConfig;
use crate::peer::handle::PeerHandle;
use crate::peer::ip_signer::{Clock, IpSigner};
use crate::peer::peer::{Direction, Peer};
use crate::router::{AppVersion, ExternalHandler, InboundHandler};
use crate::throttling::inbound_msg_byte::InboundMsgByteThrottler;
use crate::throttling::outbound_msg::{OutboundMsgThrottler, OutboundMsgThrottlerConfig};

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
    async fn connected(
        &self,
        node_id: NodeId,
        _version: &AppVersion,
        _subnet_id: ava_types::id::Id,
    ) {
        self.connected.lock().push(node_id);
    }

    async fn disconnected(&self, node_id: NodeId) {
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
    min_after: Application,
    min_compatible: Application,
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
            upgrade_time: std::time::UNIX_EPOCH + std::time::Duration::from_secs(4_000_000_000),
            min_after: Application::new("avalanchego", 1, 14, 0),
            min_compatible: Application::new("avalanchego", 1, 14, 0),
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

    /// Override the post-upgrade / pre-upgrade compatibility floors.
    #[must_use]
    pub fn floors(mut self, min_after: Application, min_compatible: Application) -> Self {
        self.min_after = min_after;
        self.min_compatible = min_compatible;
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

        let compat = Arc::new(Compatibility::new(
            self.version.clone(),
            self.min_after.clone(),
            self.min_compatible.clone(),
            self.upgrade_time,
        ));

        let outbound = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig::default());
        let inbound = Arc::new(InboundMsgByteThrottler::new(
            32 * 1024 * 1024,
            6 * 1024 * 1024,
            2 * 1024 * 1024,
        ));

        let my_ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9651);
        let ip_tracker = Arc::new(crate::network::ip_tracker::IpTracker::new());

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
            ip_tracker,
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

/// Overridable handshake fields used to construct the §1.4 disconnect-reason
/// cases. `Default` builds a fully-valid handshake (matched to the harness).
#[derive(Default)]
pub struct HandshakeOverrides {
    /// Override the advertised `network_id`.
    pub network_id: Option<u32>,
    /// Override the advertised `my_time` (Unix seconds).
    pub my_time: Option<u64>,
    /// Override the advertised version triple.
    pub version: Option<Application>,
    /// Override the advertised port.
    pub port: Option<u16>,
    /// Advertise this many (dummy) tracked subnets.
    pub num_tracked_subnets: Option<usize>,
    /// Override the supported-ACP set.
    pub supported_acps: Option<Vec<u32>>,
    /// Override the objected-ACP set.
    pub objected_acps: Option<Vec<u32>>,
    /// Advertise a bloom salt of this many bytes.
    pub bloom_salt_len: Option<usize>,
    /// Corrupt the TLS IP signature so verification fails.
    pub corrupt_ip_sig: bool,
}

/// A test harness that owns a peer's signing identity so it can build *valid*
/// (or deliberately invalid) handshakes the peer-under-test will verify.
pub struct PeerHarness {
    builder: TestPeerBuilder,
    cfg: Arc<PeerConfig>,
    /// The peer's identity (its cert is presented to `Peer::spawn`, and its key
    /// signs the handshake IP).
    peer_identity: Identity,
    peer_id: NodeId,
    /// The peer's BLS signer (for the IP proof-of-possession).
    peer_bls: Arc<ava_crypto::bls::LocalSigner>,
}

impl Default for PeerHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerHarness {
    /// Build a harness with default interop fields.
    #[must_use]
    pub fn new() -> Self {
        let builder = TestPeerBuilder::new();
        let cfg = builder.build_config();
        let peer_identity = Identity::generate().expect("peer identity");
        let peer_cert = ava_crypto::staking::parse_certificate(peer_identity.cert_der())
            .expect("parse peer cert");
        let peer_id = crate::peer::upgrader::node_id_from_cert(&peer_cert);
        let peer_bls = Arc::new(ava_crypto::bls::LocalSigner::generate().expect("peer bls"));
        Self {
            builder,
            cfg,
            peer_identity,
            peer_id,
            peer_bls,
        }
    }

    /// Rebuild with a different compatibility upgrade time (preserves the shared
    /// clock + router). Use before [`PeerHarness::spawn`].
    #[must_use]
    pub fn with_upgrade_time(mut self, t: std::time::SystemTime) -> Self {
        self.builder = std::mem::take(&mut self.builder).upgrade_time(t);
        self.cfg = self.builder.build_config();
        self
    }

    /// Rebuild with explicit compatibility floors (preserves the shared clock +
    /// router). Use before [`PeerHarness::spawn`].
    #[must_use]
    pub fn with_floors(mut self, min_after: Application, min_compatible: Application) -> Self {
        self.builder = std::mem::take(&mut self.builder).floors(min_after, min_compatible);
        self.cfg = self.builder.build_config();
        self
    }

    /// The recording router (so tests can assert `connected`/`disconnected`).
    #[must_use]
    pub fn router(&self) -> Arc<RecordingRouter> {
        self.builder.router()
    }

    /// The controllable clock.
    #[must_use]
    pub fn clock(&self) -> Arc<TestClock> {
        self.builder.clock()
    }

    /// Spawn the peer-under-test over a duplex; returns the remote end (the test
    /// acts as the peer) and the handle.
    pub fn spawn(&mut self) -> (DuplexStream, PeerHandle) {
        let (local, remote) = tokio::io::duplex(1 << 20);
        let peer_cert = ava_crypto::staking::parse_certificate(self.peer_identity.cert_der())
            .expect("parse peer cert");
        let net_token = CancellationToken::new();
        let tracker = TaskTracker::new();
        let handle = Peer::spawn(
            Arc::clone(&self.cfg),
            self.peer_id,
            peer_cert,
            Direction::Inbound,
            local,
            &net_token,
            &tracker,
        );
        tracker.close();
        (remote, handle)
    }

    /// Build a framed `Handshake` payload from the harness peer identity, with
    /// the given overrides applied.
    #[must_use]
    pub fn build_handshake(&self, o: HandshakeOverrides) -> Vec<u8> {
        use ava_message::builder::OutboundMsgBuilder;

        let network_id = o.network_id.unwrap_or(self.cfg.network_id);
        let my_time = o.my_time.unwrap_or_else(|| self.clock().unix());
        let port = o.port.unwrap_or(9651);
        let version = o
            .version
            .unwrap_or_else(|| Application::new("avalanchego", 1, 14, 2));

        // Sign the peer IP with the peer identity's TLS + BLS keys.
        let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1));
        let unsigned = crate::peer::ip::UnsignedIp::new(ip, port, my_time);
        let tls_key = self.peer_identity.tls_signing_key().expect("peer tls key");
        let mut signed = unsigned
            .sign(&tls_key, self.peer_bls.as_ref())
            .expect("sign peer ip");
        if o.corrupt_ip_sig {
            signed.corrupt_tls_signature_for_test();
        }

        let subnets: Vec<ava_types::id::Id> = (0..o.num_tracked_subnets.unwrap_or(0))
            .map(|i| {
                let mut b = [0u8; 32];
                b[0] = u8::try_from(i % 251).unwrap_or(0);
                b[1] = u8::try_from(i / 251).unwrap_or(0);
                ava_types::id::Id::from_slice(&b).expect("id")
            })
            .collect();

        let salt = vec![0u8; o.bloom_salt_len.unwrap_or(0)];

        let msg = self
            .cfg
            .creator
            .handshake(
                network_id,
                my_time,
                std::net::SocketAddr::new(ip, port),
                &version.name,
                version.major,
                version.minor,
                version.patch,
                0,
                signed.unsigned.timestamp,
                signed.tls_signature(),
                signed.bls_signature_bytes(),
                &subnets,
                &o.supported_acps.unwrap_or_default(),
                &o.objected_acps.unwrap_or_default(),
                &[],
                &salt,
                true,
            )
            .expect("build handshake");
        msg.bytes.to_vec()
    }

    /// Build a framed (empty) `PeerList` payload.
    #[must_use]
    pub fn build_peer_list(&self) -> Vec<u8> {
        use ava_message::builder::OutboundMsgBuilder;
        let msg = self.cfg.creator.peer_list(&[], true).expect("peerlist");
        msg.bytes.to_vec()
    }

    /// Build a framed `Ping{uptime}` payload.
    #[must_use]
    pub fn build_ping(&self, uptime: u32) -> Vec<u8> {
        use ava_message::builder::OutboundMsgBuilder;
        let msg = self.cfg.creator.ping(uptime).expect("ping");
        msg.bytes.to_vec()
    }

    /// Build a framed `Pong` payload.
    #[must_use]
    pub fn build_pong(&self) -> Vec<u8> {
        use ava_message::builder::OutboundMsgBuilder;
        let msg = self.cfg.creator.pong().expect("pong");
        msg.bytes.to_vec()
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
