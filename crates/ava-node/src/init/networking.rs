// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 16 (specs/12 §2.2): the P2P networking layer (mirror Go
//! `initNetworking`).
//!
//! Includes the Rust ports of the two node-local `ExternalHandler` wrappers
//! from Go (`node/insecure_validator_manager.go`, `node/beacon_manager.go`)
//! and the [`RouterBridge`] seam that will hand decoded wire messages to the
//! `06` ChainRouter once the wire→engine op conversion lands (M8.30,
//! `tests/PORTING.md`).

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};

use parking_lot::{Mutex, RwLock};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use ava_config::node::Config;
use ava_crypto::bls::Signer;
use ava_engine::networking::router::Router as EngineRouter;
use ava_genesis::Bootstrapper;
use ava_message::builder::Creator;
use ava_message::codec::InboundMessage;
use ava_network::config::PeerConfig;
use ava_network::identity::Identity;
use ava_network::metrics::Metrics as NetworkMetrics;
use ava_network::network::ip_tracker::IpTracker;
use ava_network::network::{Network, NetworkImpl};
use ava_network::peer::ip_signer::{Clock as PeerClock, IpSigner, SystemClock};
use ava_network::peer::metrics::PeerMetrics;
use ava_network::router::{AppVersion, ExternalHandler, InboundHandler};
use ava_network::throttling::inbound_msg_byte::InboundMsgByteThrottler;
use ava_network::throttling::outbound_msg::{OutboundMsgThrottler, OutboundMsgThrottlerConfig};
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::ValidatorManager;

use crate::error::{Error, Result};
use crate::init::nat::Nat;
use crate::nat::NatRouter;

/// The name Go maps the staking port under (`constants.AppName + "-staking"`).
const STAKING_PORT_NAME: &str = "avalanchego-staking";

/// The network→consensus bridge (the base `ExternalHandler` the peer actors
/// call). **Narrow seam (M8.29, `tests/PORTING.md`):** the decoded
/// wire-message → engine `InboundOp` conversion does not exist yet, so
/// `handle_inbound` drops messages (debug-logged) until M8.30 wires the slot
/// set by [`RouterBridge::set_engine_router`]. Peer lifecycle events are
/// forwarded to nothing — the engine `ChainRouter` has no peer surface yet.
#[derive(Default)]
pub struct RouterBridge {
    engine_router: OnceLock<Arc<dyn EngineRouter>>,
}

impl RouterBridge {
    /// A bridge with an empty engine-router slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fill the engine-router slot (init step 20). Idempotent: the first call
    /// wins, matching the single chain-router of the node.
    pub fn set_engine_router(&self, router: Arc<dyn EngineRouter>) {
        let _ = self.engine_router.set(router);
    }

    /// The engine router, once step 20 has filled the slot.
    #[must_use]
    pub fn engine_router(&self) -> Option<Arc<dyn EngineRouter>> {
        self.engine_router.get().cloned()
    }
}

#[async_trait::async_trait]
impl InboundHandler for RouterBridge {
    async fn handle_inbound(&self, _ctx: &CancellationToken, msg: InboundMessage) {
        let Some(router) = self.engine_router() else {
            tracing::debug!(op = ?msg.op, "no engine router yet; dropping inbound");
            return;
        };
        match crate::init::inbound_decode::decode_inbound(msg.sender, &msg) {
            Some(engine_msg) => router.handle_inbound(engine_msg).await,
            None => tracing::trace!(op = ?msg.op, "non-consensus inbound; ignored"),
        }
    }
}

#[async_trait::async_trait]
impl ExternalHandler for RouterBridge {
    fn connected(&self, node_id: NodeId, version: &AppVersion, subnet_id: Id) {
        tracing::info!(%node_id, %version, %subnet_id, "peer connected");
    }

    fn disconnected(&self, node_id: NodeId) {
        tracing::info!(%node_id, "peer disconnected");
    }
}

/// Port of Go `node/insecure_validator_manager.go`: with sybil protection off,
/// every connecting peer is registered as a primary-network validator with the
/// configured disabled weight, and deregistered on disconnect.
pub struct InsecureValidatorManager {
    inner: Arc<dyn ExternalHandler>,
    vdrs: Arc<dyn ValidatorManager>,
    weight: u64,
}

impl InsecureValidatorManager {
    /// Wrap `inner`, registering peers on `vdrs` at `weight`.
    #[must_use]
    pub fn new(
        inner: Arc<dyn ExternalHandler>,
        vdrs: Arc<dyn ValidatorManager>,
        weight: u64,
    ) -> Self {
        Self {
            inner,
            vdrs,
            weight,
        }
    }
}

#[async_trait::async_trait]
impl InboundHandler for InsecureValidatorManager {
    async fn handle_inbound(&self, ctx: &CancellationToken, msg: InboundMessage) {
        self.inner.handle_inbound(ctx, msg).await;
    }
}

#[async_trait::async_trait]
impl ExternalHandler for InsecureValidatorManager {
    fn connected(&self, node_id: NodeId, version: &AppVersion, subnet_id: Id) {
        if subnet_id == PRIMARY_NETWORK_ID {
            // Sybil protection is disabled: a fake TxID (the padded NodeID,
            // like Go) marks the connection-derived registration.
            let tx_id = padded_node_id(node_id);
            if let Err(e) =
                self.vdrs
                    .add_staker(PRIMARY_NETWORK_ID, node_id, None, tx_id, self.weight)
            {
                tracing::debug!(%node_id, error = %e, "failed to add insecure validator");
            }
        }
        self.inner.connected(node_id, version, subnet_id);
    }

    fn disconnected(&self, node_id: NodeId) {
        if let Err(e) = self
            .vdrs
            .remove_weight(PRIMARY_NETWORK_ID, node_id, self.weight)
        {
            tracing::debug!(%node_id, error = %e, "failed to remove insecure validator");
        }
        self.inner.disconnected(node_id);
    }
}

/// Pad a 20-byte NodeID into a 32-byte Id (Go's dummy TxID for
/// sybil-protection-off registrations).
#[must_use]
pub fn padded_node_id(node_id: NodeId) -> Id {
    let mut bytes = [0u8; 32];
    let src = node_id.as_bytes();
    let len = src.len().min(32);
    if let (Some(dst), Some(src)) = (bytes.get_mut(..len), src.get(..len)) {
        dst.copy_from_slice(src);
    }
    Id::from(bytes)
}

/// Port of Go `node/beacon_manager.go`: counts handshaken connections to the
/// bootstrap-beacon set and fires `on_sufficiently_connected` once ≥ the
/// required count ((3·beacons + 3) / 4).
pub struct BeaconManager {
    inner: Arc<dyn ExternalHandler>,
    beacons: Arc<dyn ValidatorManager>,
    /// The set of connected beacon node-ids (Go `beacon_manager.go` peer-set
    /// semantics): `connected` inserts (idempotent), `disconnected` removes.
    /// `len()` is the live connection count — never double-counts a duplicate
    /// `connected`, never goes negative on a spurious `disconnected`.
    conns: Mutex<HashSet<NodeId>>,
    required_conns: i64,
    on_sufficiently_connected: tokio::sync::watch::Sender<bool>,
}

impl BeaconManager {
    /// Wrap `inner`, tracking connections against `beacons`.
    #[must_use]
    pub fn new(
        inner: Arc<dyn ExternalHandler>,
        beacons: Arc<dyn ValidatorManager>,
        required_conns: i64,
        on_sufficiently_connected: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        Self {
            inner,
            beacons,
            conns: Mutex::new(HashSet::new()),
            required_conns,
            on_sufficiently_connected,
        }
    }
}

#[async_trait::async_trait]
impl InboundHandler for BeaconManager {
    async fn handle_inbound(&self, ctx: &CancellationToken, msg: InboundMessage) {
        self.inner.handle_inbound(ctx, msg).await;
    }
}

#[async_trait::async_trait]
impl ExternalHandler for BeaconManager {
    fn connected(&self, node_id: NodeId, version: &AppVersion, subnet_id: Id) {
        if subnet_id == PRIMARY_NETWORK_ID
            && self.beacons.get_weight(PRIMARY_NETWORK_ID, node_id) != 0
        {
            let conns = {
                let mut set = self.conns.lock();
                set.insert(node_id);
                i64::try_from(set.len()).unwrap_or(i64::MAX)
            };
            tracing::debug!(
                %node_id,
                conns,
                required = self.required_conns,
                "beacon connected (rung 4: connectivity count)"
            );
            if conns >= self.required_conns {
                tracing::debug!(
                    conns,
                    required = self.required_conns,
                    "rung 5: beacon-connectivity gate fired"
                );
                let _ = self.on_sufficiently_connected.send(true);
            }
        }
        self.inner.connected(node_id, version, subnet_id);
    }

    fn disconnected(&self, node_id: NodeId) {
        // Remove by id (a spurious disconnect for an un-counted node is a
        // no-op — the set never goes "negative"). The gate is one-shot (Go
        // `onSufficientlyConnected` parity): once fired it never downgrades, so
        // a disconnect after firing does not re-close it.
        if self.beacons.get_weight(PRIMARY_NETWORK_ID, node_id) != 0 {
            self.conns.lock().remove(&node_id);
        }
        self.inner.disconnected(node_id);
    }
}

/// Everything step 16 hands back to `Node::new`.
pub struct Networking {
    /// The P2P runtime.
    pub net: Arc<NetworkImpl>,
    /// The bound staking listener address (process.json `stakingAddress`).
    pub staking_address: SocketAddr,
    /// The advertised public IP:staking-port, updated by the dynamic-IP
    /// updater (shared with the info API).
    pub my_ip: Arc<RwLock<SocketAddr>>,
    /// The network→consensus bridge (its engine-router slot is filled by init
    /// step 20).
    pub router_bridge: Arc<RouterBridge>,
    /// Receives `true` once sufficiently many beacons are connected (Go
    /// `onSufficientlyConnected`; consumed by the M8.30 dispatch warn task).
    pub on_sufficiently_connected: tokio::sync::watch::Receiver<bool>,
    /// The dynamic-IP updater task, when a resolution service is configured.
    pub ip_updater: Option<JoinHandle<()>>,
    /// The staking-port keep-alive mapping task.
    pub port_mapping: JoinHandle<()>,
}

/// Whether `ip` is publicly routable (shared with the API-server step).
fn is_public(ip: IpAddr) -> bool {
    super::api_server::ip_is_public(ip)
}

/// Narrow seam over the one `Network` method the beacon-tracking loop needs,
/// so the loop is unit-testable without assembling a full [`NetworkImpl`].
trait BeaconTracker {
    fn track_beacon(&self, node_id: NodeId, ip: SocketAddr);
}

impl BeaconTracker for NetworkImpl {
    fn track_beacon(&self, node_id: NodeId, ip: SocketAddr) {
        self.manually_track(node_id, ip);
    }
}

/// Pin each configured bootstrap beacon so the backoff dialer keeps
/// reconnecting to it (Go `initNetworking` tracks the beacon IPs). Without
/// this a beaconed node never dials its beacons and cannot bootstrap.
fn track_bootstrappers(tracker: &dyn BeaconTracker, bootstrappers: &[Bootstrapper]) {
    for b in bootstrappers {
        tracker.track_beacon(b.id, b.ip);
    }
}

/// Step 16: bind the staking listener, resolve the advertised public IP,
/// assemble the peer config, and build the network (mirror Go
/// `initNetworking`). Assumes validators, CPU/disk targeters, the message
/// creator, and NAT are initialized (the Go comment).
///
/// # Errors
/// - Listener bind / address-parse failures.
/// - [`Error::UnsupportedResolver`] when `--public-ip-resolution-service`
///   names a service with no Rust resolver yet (deferral).
/// - NAT external-IP resolution failure when neither a public IP nor a
///   resolution service is configured and the router cannot answer.
#[allow(clippy::too_many_arguments)]
pub async fn init_networking(
    config: &Config,
    node_id: NodeId,
    identity: &Identity,
    staking_signer: &Arc<dyn Signer>,
    creator: &Arc<Creator>,
    network_registry: &prometheus::Registry,
    validators: &Arc<dyn ValidatorManager>,
    bootstrappers: &Arc<dyn ValidatorManager>,
    nat: &Nat,
    net_token: &CancellationToken,
) -> Result<Networking> {
    let listen_addr = format!(
        "{}:{}",
        if config.ip_config.listen_host.is_empty() {
            "0.0.0.0"
        } else {
            config.ip_config.listen_host.as_str()
        },
        config.ip_config.listen_port
    );
    let listener = TcpListener::bind(&listen_addr).await?;
    let staking_address = listener.local_addr()?;
    let staking_port = staking_address.port();

    // The three-way public-IP switch (Go initNetworking).
    let (public_addr, ip_updater): (IpAddr, Option<JoinHandle<()>>) =
        if !config.ip_config.public_ip.is_empty() {
            let addr: IpAddr = config.ip_config.public_ip.parse().map_err(|e| {
                Error::Networking(format!(
                    "invalid public IP address {:?}: {e}",
                    config.ip_config.public_ip
                ))
            })?;
            (addr, None)
        } else if !config.ip_config.public_ip_resolution_service.is_empty() {
            // Concrete opendns/http resolvers are a documented deferral
            // (M8.28 left the `Resolver` trait seam; an HTTP client dependency
            // decision is needed first — `tests/PORTING.md`).
            return Err(Error::UnsupportedResolver(
                config.ip_config.public_ip_resolution_service.clone(),
            ));
        } else {
            let router = Arc::clone(&nat.router);
            let resolved = tokio::task::spawn_blocking(move || router.external_ip()).await?;
            let addr = resolved.map_err(|e| {
            Error::Networking(format!(
                "public IP / IP resolution service not given and failed to resolve IP with NAT: {e}"
            ))
        })?;
            (addr, None)
        };

    if !is_public(public_addr) {
        tracing::warn!(ip = %public_addr, "P2P IP is private, you will not be publicly discoverable");
    }

    let my_ip = Arc::new(RwLock::new(SocketAddr::new(public_addr, staking_port)));

    // Keep the staking port mapped (no-op on a NAT-less router).
    let port_mapping = nat.mapper.start(
        staking_port,
        staking_port,
        STAKING_PORT_NAME,
        net_token.child_token(),
    );

    tracing::info!(ip = %*my_ip.read(), "initializing networking");

    if !config.network_config.tls_key_log_file.is_empty() {
        tracing::warn!(
            filename = %config.network_config.tls_key_log_file,
            "TLS key logging is configured but not supported yet (tests/PORTING.md)"
        );
    }

    // The consensus router chain: bridge → (insecure validators) → (beacons).
    let router_bridge = Arc::new(RouterBridge::new());
    let mut consensus_router: Arc<dyn ExternalHandler> =
        Arc::clone(&router_bridge) as Arc<dyn ExternalHandler>;

    if !config.staking_config.sybil_protection_enabled {
        // Register ourselves with a dummy TxID (the padded NodeID, Go parity).
        validators
            .add_staker(
                PRIMARY_NETWORK_ID,
                node_id,
                Some(staking_signer.public_key().clone()),
                padded_node_id(node_id),
                config.staking_config.sybil_protection_disabled_weight,
            )
            .map_err(|e| Error::Networking(e.to_string()))?;
        consensus_router = Arc::new(InsecureValidatorManager::new(
            consensus_router,
            Arc::clone(validators),
            config.staking_config.sybil_protection_disabled_weight,
        ));
    }

    let (connected_tx, connected_rx) = tokio::sync::watch::channel(false);
    let num_beacons = bootstrappers.num_validators(PRIMARY_NETWORK_ID);
    let required_conns = i64::try_from(num_beacons)
        .unwrap_or(i64::MAX)
        .saturating_mul(3)
        .saturating_add(3)
        / 4;
    if required_conns > 0 {
        consensus_router = Arc::new(BeaconManager::new(
            consensus_router,
            Arc::clone(bootstrappers),
            required_conns,
            connected_tx,
        ));
    } else {
        let _ = connected_tx.send(true);
    }

    let clock: Arc<dyn PeerClock> = Arc::new(SystemClock);
    let compatibility = Arc::new(ava_version::compatibility::get_compatibility(
        chrono_to_system_time(config.upgrade_config.granite_time),
    ));
    let ip_signer = Arc::new(IpSigner::new(
        identity.clone(),
        Arc::clone(staking_signer),
        Arc::clone(&clock),
    ));

    let outbound = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig {
        vdr_alloc_size: config.network_config.outbound_throttler_vdr_alloc_size,
        at_large_alloc_size: config.network_config.outbound_throttler_at_large_alloc_size,
        node_max_at_large_bytes: config
            .network_config
            .outbound_throttler_node_max_at_large_bytes,
    });
    let inbound = Arc::new(InboundMsgByteThrottler::new(
        config.network_config.inbound_throttler_vdr_alloc_size,
        config.network_config.inbound_throttler_at_large_alloc_size,
        config
            .network_config
            .inbound_throttler_node_max_at_large_bytes,
    ));

    let net_metrics =
        NetworkMetrics::new(network_registry).map_err(|e| Error::Networking(e.to_string()))?;
    inbound.set_metrics(&net_metrics);
    let peer_metrics =
        PeerMetrics::new(network_registry).map_err(|e| Error::Networking(e.to_string()))?;

    let mut peer_config = PeerConfig::new(
        config.network_id,
        node_id,
        identity.clone(),
        *my_ip.read(),
        ava_version::CURRENT.clone(),
        Arc::clone(creator),
        consensus_router,
        compatibility,
        ip_signer,
        outbound,
        inbound,
        Arc::new(IpTracker::new()),
        clock,
    )
    .with_peer_metrics(peer_metrics);
    peer_config.my_tracked_subnets = config.tracked_subnets.iter().copied().collect();
    peer_config.my_supported_acps = config
        .network_config
        .supported_acps
        .iter()
        .copied()
        .collect();
    peer_config.my_objected_acps = config
        .network_config
        .objected_acps
        .iter()
        .copied()
        .collect();
    peer_config.ping_frequency = config.network_config.ping_frequency;
    peer_config.pong_timeout = config.network_config.ping_pong_timeout;

    let net = NetworkImpl::new_with_metrics(Arc::new(peer_config), listener, net_metrics)
        .map_err(|e| Error::Networking(e.to_string()))?;

    // Pin each configured bootstrap beacon so the backoff dialer keeps
    // reconnecting to it (Go `initNetworking` tracks the beacon IPs). Without
    // this a beaconed node never dials its beacons and cannot bootstrap.
    track_bootstrappers(net.as_ref(), &config.bootstrap_config.bootstrappers);

    Ok(Networking {
        net,
        staking_address,
        my_ip,
        router_bridge,
        on_sufficiently_connected: connected_rx,
        ip_updater,
        port_mapping,
    })
}

/// Convert a `chrono` UTC time into a `SystemTime` (saturating at the epoch
/// for pre-1970 values, which the upgrade schedule never contains).
fn chrono_to_system_time(t: chrono::DateTime<chrono::Utc>) -> std::time::SystemTime {
    let secs = t.timestamp();
    if secs >= 0 {
        std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(secs.unsigned_abs()))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    } else {
        std::time::SystemTime::UNIX_EPOCH
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use ava_engine::networking::router::{
        InboundMessage as EngineInboundMessage, InboundOp, Router as EngineRouter,
    };
    use ava_genesis::Bootstrapper;
    use ava_message::codec::{Compression, MsgBuilder};
    use ava_message::proto::p2p;
    use ava_types::constants::PRIMARY_NETWORK_ID;
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_validators::validator::Validator;
    use ava_validators::{ManagerCallbackListener, ValidatorManager};
    use bytes::Bytes;
    use tokio_util::sync::CancellationToken;

    use super::{
        AppVersion, BeaconManager, BeaconTracker, ExternalHandler, InboundHandler, RouterBridge,
        track_bootstrappers,
    };

    // ---------------------------------------------------------------------------
    // Minimal stubs for the BeaconManager gate test
    // ---------------------------------------------------------------------------

    /// A no-op `ExternalHandler` (both traits) used as the inner handler.
    struct NoopHandler;

    #[async_trait]
    impl InboundHandler for NoopHandler {
        async fn handle_inbound(
            &self,
            _ctx: &CancellationToken,
            _msg: ava_message::codec::InboundMessage,
        ) {
        }
    }

    #[async_trait]
    impl ExternalHandler for NoopHandler {
        fn connected(&self, _n: NodeId, _v: &AppVersion, _s: Id) {}
        fn disconnected(&self, _n: NodeId) {}
    }

    /// A stub `ValidatorManager` that returns weight=1 only for node_ids in
    /// `members`; all other methods are unimplemented (not called by this test).
    struct StubBeacons {
        members: HashSet<NodeId>,
    }

    impl ValidatorManager for StubBeacons {
        fn add_staker(
            &self,
            _subnet: Id,
            _node: NodeId,
            _pk: Option<ava_crypto::bls::PublicKey>,
            _tx: Id,
            _weight: u64,
        ) -> ava_validators::error::Result<()> {
            unimplemented!("not needed for gate test")
        }

        fn add_weight(
            &self,
            _subnet: Id,
            _node: NodeId,
            _weight: u64,
        ) -> ava_validators::error::Result<()> {
            unimplemented!("not needed for gate test")
        }

        fn remove_weight(
            &self,
            _subnet: Id,
            _node: NodeId,
            _weight: u64,
        ) -> ava_validators::error::Result<()> {
            unimplemented!("not needed for gate test")
        }

        fn get_weight(&self, _subnet: Id, node_id: NodeId) -> u64 {
            u64::from(self.members.contains(&node_id))
        }

        fn get_validator(&self, _subnet: Id, _node: NodeId) -> Option<Validator> {
            unimplemented!("not needed for gate test")
        }

        fn get_validator_ids(&self, _subnet: Id) -> Vec<NodeId> {
            unimplemented!("not needed for gate test")
        }

        fn subset_weight(
            &self,
            _subnet: Id,
            _ids: &HashSet<NodeId>,
        ) -> ava_validators::error::Result<u64> {
            unimplemented!("not needed for gate test")
        }

        fn total_weight(&self, _subnet: Id) -> ava_validators::error::Result<u64> {
            unimplemented!("not needed for gate test")
        }

        fn num_validators(&self, _subnet: Id) -> usize {
            self.members.len()
        }

        fn num_subnets(&self) -> usize {
            unimplemented!("not needed for gate test")
        }

        fn sample(&self, _subnet: Id, _size: usize) -> ava_validators::error::Result<Vec<NodeId>> {
            unimplemented!("not needed for gate test")
        }

        fn register_callback_listener(&self, _subnet: Id, _l: Arc<dyn ManagerCallbackListener>) {
            // no-op: not needed for gate test
        }
    }

    /// `BeaconManager` fires the watch gate at exactly `required_conns` beacon
    /// connections, not before, and a non-beacon connection must not count.
    #[tokio::test]
    async fn beacon_manager_fires_gate_at_required_conns() {
        let beacon_ids = [
            NodeId::from([1u8; 20]),
            NodeId::from([2u8; 20]),
            NodeId::from([3u8; 20]),
        ];
        let non_beacon = NodeId::from([9u8; 20]);

        let beacons: Arc<dyn ValidatorManager> = Arc::new(StubBeacons {
            members: beacon_ids.iter().copied().collect(),
        });
        let inner: Arc<dyn ExternalHandler> = Arc::new(NoopHandler);
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let bm = BeaconManager::new(inner, beacons, 2, tx);

        let v = ava_version::CURRENT.clone();
        bm.connected(non_beacon, &v, PRIMARY_NETWORK_ID); // ignored: not a beacon
        assert!(
            !*rx.borrow_and_update(),
            "non-beacon must not fire the gate"
        );
        bm.connected(beacon_ids[0], &v, PRIMARY_NETWORK_ID); // 1/2
        assert!(!*rx.borrow_and_update(), "one beacon < required_conns");
        bm.connected(beacon_ids[1], &v, PRIMARY_NETWORK_ID); // 2/2
        assert!(*rx.borrow_and_update(), "gate fires at required_conns");
    }

    /// Five beacons, `required_conns = 4`. A duplicate `connected()` for the same
    /// beacon must NOT inflate the count: 4 raw calls spanning only 3 *distinct*
    /// beacons must leave the gate closed (Go peer-set dedup semantics; M9.15).
    #[tokio::test]
    async fn duplicate_connected_does_not_double_count() {
        let beacon_ids: Vec<NodeId> = (1u8..=5).map(|b| NodeId::from([b; 20])).collect();
        let beacons: Arc<dyn ValidatorManager> = Arc::new(StubBeacons {
            members: beacon_ids.iter().copied().collect(),
        });
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let bm = BeaconManager::new(Arc::new(NoopHandler), beacons, 4, tx);
        let v = ava_version::CURRENT.clone();

        bm.connected(beacon_ids[0], &v, PRIMARY_NETWORK_ID);
        bm.connected(beacon_ids[0], &v, PRIMARY_NETWORK_ID); // duplicate — must not count twice
        bm.connected(beacon_ids[1], &v, PRIMARY_NETWORK_ID);
        bm.connected(beacon_ids[2], &v, PRIMARY_NETWORK_ID);
        assert!(
            !*rx.borrow_and_update(),
            "3 distinct beacons < required 4 despite 4 raw connects"
        );

        bm.connected(beacon_ids[3], &v, PRIMARY_NETWORK_ID); // 4th DISTINCT beacon
        assert!(
            *rx.borrow_and_update(),
            "gate fires at 4 distinct beacons"
        );
    }

    /// A `disconnected()` for a beacon that never connected must not drive the count
    /// negative and wedge the gate below threshold (M9.15 hypothesis 3).
    #[tokio::test]
    async fn disconnect_before_connect_does_not_wedge_gate() {
        let beacon_ids: Vec<NodeId> = (1u8..=5).map(|b| NodeId::from([b; 20])).collect();
        let beacons: Arc<dyn ValidatorManager> = Arc::new(StubBeacons {
            members: beacon_ids.iter().copied().collect(),
        });
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let bm = BeaconManager::new(Arc::new(NoopHandler), beacons, 4, tx);
        let v = ava_version::CURRENT.clone();

        bm.disconnected(beacon_ids[4]); // spurious: never connected
        for id in &beacon_ids[0..4] {
            bm.connected(*id, &v, PRIMARY_NETWORK_ID);
        }
        assert!(
            *rx.borrow_and_update(),
            "4 beacons connected ⇒ gate fires even after a spurious disconnect"
        );
    }

    /// Concurrent `connected()` bursts (the real `finish_handshake` pattern: N peers
    /// complete on N runtime threads) fire the gate exactly once with the full set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_connects_fire_gate() {
        let beacon_ids: Vec<NodeId> = (1u8..=5).map(|b| NodeId::from([b; 20])).collect();
        let beacons: Arc<dyn ValidatorManager> = Arc::new(StubBeacons {
            members: beacon_ids.iter().copied().collect(),
        });
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let bm = Arc::new(BeaconManager::new(Arc::new(NoopHandler), beacons, 4, tx));

        let mut handles = Vec::new();
        for id in beacon_ids {
            let bm = Arc::clone(&bm);
            handles.push(tokio::spawn(async move {
                bm.connected(id, &ava_version::CURRENT.clone(), PRIMARY_NETWORK_ID);
            }));
        }
        for h in handles {
            h.await.expect("connected task joins");
        }
        assert!(
            *rx.borrow_and_update(),
            "gate fires once all 5 beacons connect concurrently"
        );
    }

    /// A recording stub that captures every [`EngineInboundMessage`] it receives.
    struct RecordingRouter {
        received: Arc<Mutex<Vec<EngineInboundMessage>>>,
    }

    impl RecordingRouter {
        fn new() -> (Self, Arc<Mutex<Vec<EngineInboundMessage>>>) {
            let store = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    received: Arc::clone(&store),
                },
                store,
            )
        }
    }

    #[async_trait]
    impl EngineRouter for RecordingRouter {
        fn add_chain(
            &self,
            _chain: ava_types::id::Id,
            _handler: Arc<dyn ava_engine::networking::router::ChainMessageSink>,
        ) {
        }

        async fn handle_inbound(&self, msg: EngineInboundMessage) {
            self.received.lock().unwrap().push(msg);
        }

        fn register_request(&self, _node: NodeId, _chain: Id, _request_id: u32, _op_tag: u8) {}

        fn health_check(&self) -> bool {
            true
        }
    }

    fn make_get_accepted_frontier_msg(
        chain: Id,
        request_id: u32,
        sender: NodeId,
    ) -> ava_message::codec::InboundMessage {
        let inner = p2p::message::Message::GetAcceptedFrontier(p2p::GetAcceptedFrontier {
            chain_id: Bytes::copy_from_slice(chain.as_bytes()),
            request_id,
            deadline: 1_000_000_000,
        });
        let m = p2p::Message {
            message: Some(inner),
        };
        let mb = MsgBuilder::default();
        let (bytes, _, _) = mb.marshal(&m, Compression::None).expect("marshal");
        let mut msg = mb.parse_inbound(&bytes).expect("parse_inbound");
        msg.sender = sender;
        msg
    }

    /// Verify that `RouterBridge::handle_inbound` decodes a decodable inbound
    /// message and forwards the resulting [`EngineInboundMessage`] to the engine
    /// router.
    #[tokio::test]
    async fn router_bridge_routes_decoded_message_to_engine_router() {
        let chain = Id::from([0xABu8; 32]);
        let sender = NodeId::from([0x01u8; 20]);

        let bridge = Arc::new(RouterBridge::new());
        let (recording, store) = RecordingRouter::new();
        bridge.set_engine_router(Arc::new(recording));

        let msg = make_get_accepted_frontier_msg(chain, 42, sender);
        let ctx = CancellationToken::new();
        bridge.handle_inbound(&ctx, msg).await;

        let received = store.lock().unwrap();
        assert_eq!(
            received.len(),
            1,
            "engine router should receive exactly one message"
        );
        let got = received.first().expect("one message");
        assert_eq!(got.chain, chain);
        assert_eq!(got.node, sender);
        assert_eq!(got.op, InboundOp::GetAcceptedFrontier { request_id: 42 });
    }

    /// Verify that `track_bootstrappers` calls `track_beacon` for every
    /// configured beacon, in config order, with the exact ip — exercising the
    /// production helper that `init_networking` calls on the real
    /// [`NetworkImpl`].
    #[test]
    fn track_bootstrappers_records_each_beacon_in_order() {
        // A recording double that implements the narrow seam.
        #[derive(Default)]
        struct TrackRecorder {
            tracked: Mutex<Vec<(NodeId, SocketAddr)>>,
        }
        impl BeaconTracker for TrackRecorder {
            fn track_beacon(&self, node_id: NodeId, ip: SocketAddr) {
                self.tracked.lock().unwrap().push((node_id, ip));
            }
        }

        let beacons = vec![
            Bootstrapper {
                id: NodeId::from([1u8; 20]),
                ip: "127.0.0.1:9651".parse::<SocketAddr>().unwrap(),
            },
            Bootstrapper {
                id: NodeId::from([2u8; 20]),
                ip: "127.0.0.1:9652".parse::<SocketAddr>().unwrap(),
            },
        ];

        let recorder = TrackRecorder::default();
        track_bootstrappers(&recorder, &beacons);

        let tracked = recorder.tracked.lock().unwrap().clone();
        assert_eq!(tracked.len(), 2, "both configured beacons tracked");
        let (b0, b1) = (
            beacons.first().expect("beacon 0 exists"),
            beacons.get(1).expect("beacon 1 exists"),
        );
        assert_eq!(
            tracked.first().copied(),
            Some((b0.id, b0.ip)),
            "first beacon tracked in order with exact ip"
        );
        assert_eq!(
            tracked.get(1).copied(),
            Some((b1.id, b1.ip)),
            "second beacon tracked in order with exact ip"
        );
    }

    /// Verify that a non-consensus message (Ping) is silently dropped and
    /// the engine router receives nothing.
    #[tokio::test]
    async fn router_bridge_drops_non_consensus_message() {
        let bridge = Arc::new(RouterBridge::new());
        let (recording, store) = RecordingRouter::new();
        bridge.set_engine_router(Arc::new(recording));

        let inner = p2p::message::Message::Ping(p2p::Ping { uptime: 0 });
        let m = p2p::Message {
            message: Some(inner),
        };
        let mb = MsgBuilder::default();
        let (bytes, _, _) = mb.marshal(&m, Compression::None).expect("marshal");
        let msg = mb.parse_inbound(&bytes).expect("parse_inbound");
        let ctx = CancellationToken::new();
        bridge.handle_inbound(&ctx, msg).await;

        let received = store.lock().unwrap();
        assert!(
            received.is_empty(),
            "engine router should receive nothing for a Ping"
        );
    }
}
