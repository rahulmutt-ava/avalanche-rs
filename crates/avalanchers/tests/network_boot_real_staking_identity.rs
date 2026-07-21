// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! T16 regression test: a chain booted over the **production** network-boot
//! seam ([`boot_chain_over_network_core_for_test`]) must carry the node's
//! REAL staking identity into `ChainContext.node_id` — not a fresh throwaway
//! cert generated on every boot.
//!
//! Live-proven bug: `boot_chain_with_sender`'s `staking_identity()` call
//! generated a brand-new ECDSA cert on every invocation, so a networked
//! chain's `ChainContext.node_id` never matched the identity the P2P layer
//! actually handshaked with (`--staking-tls-cert-file`). The post-Durango
//! windower schedules the REAL id, so `wait_for_slot_and_decide`'s
//! `expected == self.ctx.node_id` could never be true and the node could
//! never propose a signed block (live: 40+ heights, zero proposals).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use ava_database::MemDb;
use ava_message::codec::OutboundMessage;
use ava_network::network::{
    Allower, GossipConfig, Network, PeerInfo, SendConfig as NetSendConfig, UptimeResult,
};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, RealClock};
use ava_vm::testutil::TestVm;
use tokio_util::sync::CancellationToken;

use avalanchers::wiring::chains::{
    boot_chain_over_network_core_for_test, staking_identity_from_tls,
};

struct AllowAll;
impl Allower for AllowAll {
    fn is_allowed(&self, _node_id: &NodeId) -> bool {
        true
    }
}

/// A `Network` that drops everything: this test only needs the chain to
/// assemble and expose its `ChainContext`, not to actually exchange wire
/// messages.
#[derive(Default)]
struct NoopNetwork;

#[async_trait::async_trait]
impl Network for NoopNetwork {
    async fn dispatch(self: Arc<Self>) -> ava_network::Result<()> {
        Ok(())
    }
    fn start_close(&self) {}
    fn manually_track(&self, _node_id: NodeId, _ip: SocketAddr) {}
    fn peer_info(&self, _node_ids: &[NodeId]) -> Vec<PeerInfo> {
        Vec::new()
    }
    fn node_uptime(&self) -> ava_network::Result<UptimeResult> {
        Ok(UptimeResult::default())
    }
    fn send(
        &self,
        _msg: OutboundMessage,
        _cfg: NetSendConfig,
        _subnet: Id,
        _allower: &dyn Allower,
    ) -> HashSet<NodeId> {
        HashSet::new()
    }
    fn gossip(
        &self,
        _msg: OutboundMessage,
        _subnet: Id,
        _cfg: GossipConfig,
        _allower: &dyn Allower,
    ) -> HashSet<NodeId> {
        HashSet::new()
    }
}

/// Boots a chain over [`boot_chain_over_network_core_for_test`] with a KNOWN,
/// pre-resolved `ava_network::identity::Identity` (the stand-in for the
/// node's real `--staking-tls-cert-file` identity), then asserts the
/// assembled chain's `ChainContext.node_id` is the id DERIVED FROM THAT
/// IDENTITY — never a randomly generated one.
///
/// This must FAIL against the unfixed `boot_chain_with_sender` (which called
/// `staking_identity()` unconditionally, ignoring any supplied identity).
#[tokio::test]
async fn network_boot_carries_the_real_staking_identity_into_chain_context() {
    let identity = ava_network::identity::Identity::generate().expect("Identity::generate");
    let expected_node_id = ava_crypto::staking::node_id_from_cert(identity.cert_der());
    let staking = staking_identity_from_tls(&identity).expect("staking_identity_from_tls");

    let chain_id = Id::EMPTY;
    let subnet_id = ava_types::constants::PRIMARY_NETWORK_ID;
    let network: Arc<dyn Network> = Arc::new(NoopNetwork);
    let allower: Arc<dyn Allower> = Arc::new(AllowAll);
    let token = CancellationToken::new();
    let clock: Arc<dyn Clock> = Arc::new(RealClock);
    let timeouts = Arc::new(
        ava_engine::networking::timeout::AdaptiveTimeoutManager::new(
            &ava_engine::networking::timeout::AdaptiveTimeoutConfig {
                initial_timeout: std::time::Duration::from_secs(2),
                minimum_timeout: std::time::Duration::from_secs(2),
                maximum_timeout: std::time::Duration::from_secs(10),
                timeout_coefficient: 1.0,
                timeout_halflife: std::time::Duration::from_secs(5),
            },
            Arc::clone(&clock),
        )
        .expect("AdaptiveTimeoutManager::new"),
    );
    let router = ava_engine::networking::router::ChainRouter::new(timeouts);

    // Beaconless (`Some(empty)`) so the boot short-circuits straight to
    // NormalOp without needing a live peer to answer a frontier broadcast.
    let handle = boot_chain_over_network_core_for_test(
        chain_id,
        subnet_id,
        router,
        clock,
        network,
        allower,
        TestVm::new(),
        b"genesis",
        Arc::new(MemDb::new()),
        token.clone(),
        Some(BTreeMap::new()),
        Some(staking),
    )
    .await
    .expect("boot_chain_over_network_core_for_test");

    assert_eq!(
        handle.ctx.chain.node_id, expected_node_id,
        "ChainContext.node_id must be derived from the supplied REAL staking \
         identity, not a fresh throwaway generated cert"
    );

    token.cancel();
    let _ = handle.join.await;
}
