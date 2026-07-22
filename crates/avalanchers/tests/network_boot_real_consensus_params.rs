// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! T16 regression test: a chain booted over the **production** network-boot
//! seam ([`boot_chain_over_network_core_for_test`]) must run the passed-in
//! REAL consensus parameters (Go `snowball.DefaultParameters`: k=20,
//! alpha=15, beta=20), NOT the single-node `k=1`/`alpha=1`/`beta=1` set — while
//! a loopback boot ([`boot_chain_with_loopback`]) correctly keeps k=1.
//!
//! Live-proven bug: `boot_chain_with_sender` hard-coded `single_node_params()`
//! for EVERY boot, so a networked chain on a 5-validator net finalized whichever
//! block it preferred after ONE self-poll (k=1 instant finality), unilaterally
//! ignoring the other 4 validators — a post-fork race then wedged the whole
//! network (~5 kHz repoll storm, gossip starvation, test stall).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use ava_database::MemDb;
use ava_message::codec::OutboundMessage;
use ava_network::network::{
    Allower, GossipConfig, Network, PeerInfo, SendConfig as NetSendConfig, UptimeResult,
};
use ava_snow::snowball::DEFAULT_PARAMETERS;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::{Clock, RealClock};
use ava_vm::testutil::TestVm;
use tokio_util::sync::CancellationToken;

use avalanchers::wiring::chains::{
    boot_chain_over_network_core_for_test, boot_chain_with_loopback,
};

struct AllowAll;
impl Allower for AllowAll {
    fn is_allowed(&self, _node_id: &NodeId) -> bool {
        true
    }
}

/// A `Network` that drops everything: this test only needs the chain to
/// assemble and expose the parameters its engine was built with.
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

/// The production network-boot seam threads the passed-in REAL consensus
/// parameters into the assembled chain's engine — proved by the params the boot
/// handle exposes. This must FAIL against the unfixed `boot_chain_with_sender`
/// (which ignored any supplied params and always ran `single_node_params()`).
#[tokio::test]
async fn network_boot_runs_real_consensus_params() {
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
        None,
        // The REAL parameters the production `main.rs` path passes.
        Some(DEFAULT_PARAMETERS),
    )
    .await
    .expect("boot_chain_over_network_core_for_test");

    assert_eq!(
        handle.params, DEFAULT_PARAMETERS,
        "the networked chain must run the passed-in real consensus parameters"
    );
    assert_eq!(
        handle.params.k, 20,
        "Go DefaultParameters: k = 20 (not k=1)"
    );
    assert_eq!(handle.params.alpha_preference, 15, "alpha_preference = 15");
    assert_eq!(handle.params.alpha_confidence, 15, "alpha_confidence = 15");
    assert_eq!(handle.params.beta, 20, "beta = 20 (not beta=1)");

    token.cancel();
    let _ = handle.join.await;
}

/// The loopback boot KEEPS k=1 — correct for a self-only chain (the node polls
/// only itself and its own vote is definitive). The `single_node_params()`
/// choice must NOT leak onto the networked path (asserted above) and, conversely,
/// the real parameters must NOT leak onto the loopback path.
#[tokio::test]
async fn loopback_boot_keeps_k1() {
    let handle = boot_chain_with_loopback(
        ava_types::constants::LOCAL_ID,
        Id::EMPTY,
        ava_types::constants::PRIMARY_NETWORK_ID,
        "test",
        Id::EMPTY,
        Id::EMPTY,
        TestVm::new(),
        b"genesis".to_vec(),
        Arc::new(MemDb::new()),
    )
    .await
    .expect("boot_chain_with_loopback");

    assert_eq!(
        handle.params.k, 1,
        "a loopback / self-only chain must keep k=1"
    );
    assert_eq!(handle.params.alpha_preference, 1, "loopback alpha = 1");
    assert_eq!(handle.params.beta, 1, "loopback beta = 1");

    // Cancel the HANDLE's own token — a fresh local token cancels nothing and
    // `join` would then never resolve (the hang this line replaces).
    handle.token.cancel();
    let _ = handle.join.await;
}
