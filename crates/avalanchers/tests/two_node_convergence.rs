// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Phase 2 two-`Node` convergence (M9.15 production network→consensus wiring).
//!
//! Two real [`ava_node::node::Node`] assemblies on localhost TLS, driven through
//! the production [`drive_startup_chains_over_network`] path: a beaconless beacon
//! node (its P-Chain short-circuits to NormalOp, its Getter answers) and a
//! follower node configured with the beacon as its sole bootstrapper. The
//! follower dials the beacon (Task 6), the connectivity gate fires, and the
//! follower bootstraps its P-Chain to NormalOp, converging on the beacon's tip.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ava_config::flags::{FLAG_SPECS, build_command};
use ava_config::node::Config;
use ava_config::parse::get_node_config;
use ava_config::precedence::Layered;
use ava_network::network::Network;
use ava_node::node::Node;
use ava_snow::EngineState;
use ava_types::node_id::NodeId;

use avalanchers::wiring::chains::{NetworkChainBootHandle, drive_startup_chains_over_network};

/// Build a local-network, memdb, ephemeral-identity node config under `dir`,
/// with optional `--bootstrap-ids/-ips` (mirror of `ava_node::testutil::test_config`).
fn build_config(dir: &std::path::Path, bootstrap: Option<(NodeId, std::net::SocketAddr)>) -> Config {
    let mut args: Vec<String> = [
        "avalanchers",
        "--network-id=local",
        "--db-type=memdb",
        "--staking-ephemeral-cert-enabled",
        "--staking-ephemeral-signer-enabled",
        "--http-host=127.0.0.1",
        "--http-port=0",
        "--staking-port=0",
        "--public-ip=127.0.0.1",
        "--http-shutdown-wait=0s",
    ]
    .into_iter()
    .map(String::from)
    .chain([format!("--data-dir={}", dir.display())])
    .collect();
    if let Some((id, ip)) = bootstrap {
        args.push(format!("--bootstrap-ids={id}"));
        args.push(format!("--bootstrap-ips={ip}"));
    }
    let layered = Layered::build_with_env(
        build_command(FLAG_SPECS),
        args,
        FLAG_SPECS,
        std::iter::empty(),
    )
    .expect("Layered::build_with_env");
    get_node_config(&layered).expect("get_node_config")
}

async fn boot(node: &Arc<Node>, beacons: BTreeMap<NodeId, u64>) -> Vec<NetworkChainBootHandle> {
    drive_startup_chains_over_network(
        &node.chain_manager,
        node.config.network_id,
        Arc::clone(&node.db),
        Arc::clone(&node.networking.net) as Arc<dyn ava_network::network::Network>,
        Arc::clone(&node.chain_router),
        node.networking.on_sufficiently_connected.clone(),
        beacons,
    )
    .await
    .expect("drive_startup_chains_over_network")
}

fn pchain_handle(handles: &[NetworkChainBootHandle]) -> &NetworkChainBootHandle {
    handles
        .iter()
        .find(|h| {
            // The P-Chain is the only chain on PLATFORM_CHAIN_ID; match by ctx.
            h.ctx.chain.chain_id == ava_node::init::chain_manager::PLATFORM_CHAIN_ID
        })
        .expect("a P-Chain handle")
}

/// Two real [`Node`] assemblies on localhost TLS converge over the production
/// `drive_startup_chains_over_network` path.
///
/// Beacon node: beaconless — its P-Chain short-circuits to `NormalOp` and its
/// `Getter` answers frontier/block requests.  Follower node: the beacon is its
/// sole bootstrapper.  The follower dials the beacon (Task 6), the connectivity
/// gate fires, the follower bootstraps its P-Chain, and both nodes converge on
/// the same P-Chain tip (genesis height 0 for a fresh `local` network).
#[tokio::test]
async fn follower_node_bootstraps_from_beacon_node_to_normalop() {
    let beacon_dir = tempfile::tempdir().unwrap();
    let follower_dir = tempfile::tempdir().unwrap();

    // ---- Beacon node: no bootstrappers ⇒ beaconless. ----
    let beacon_cfg = Arc::new(build_config(beacon_dir.path(), None));
    let log_factory = ava_node::logging_test_factory(&beacon_cfg);
    let beacon = Arc::new(
        Node::new(
            Arc::clone(&beacon_cfg),
            Arc::clone(&log_factory),
            tokio::runtime::Handle::current(),
        )
        .await
        .expect("beacon Node::new"),
    );
    let beacon_id = beacon.id;
    let beacon_addr = beacon.networking.staking_address;
    let beacon_handles = boot(&beacon, BTreeMap::new()).await;

    // ---- Follower node: beacon is its sole bootstrapper. ----
    let follower_cfg = Arc::new(build_config(
        follower_dir.path(),
        Some((beacon_id, beacon_addr)),
    ));
    let follower = Arc::new(
        Node::new(
            Arc::clone(&follower_cfg),
            Arc::clone(&log_factory),
            tokio::runtime::Handle::current(),
        )
        .await
        .expect("follower Node::new"),
    );
    let mut beacons = BTreeMap::new();
    beacons.insert(beacon_id, 1u64);
    let follower_handles = boot(&follower, beacons).await;

    // ---- Pump both network loops. ----
    let beacon_net = Arc::clone(&beacon.networking.net);
    let follower_net = Arc::clone(&follower.networking.net);
    let bd = tokio::spawn(async move { beacon_net.dispatch().await });
    let fd = tokio::spawn(async move { follower_net.dispatch().await });

    // ---- Rung 1: TLS peer connection. ----
    // The follower must actually establish a TLS peer connection to the beacon —
    // the connectivity gate (on_sufficiently_connected) fires only on this
    // handshake, so this is the load-bearing precondition for the bootstrapper
    // to send GetAcceptedFrontier and ultimately reach NormalOp.
    let connected = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if follower
                .networking
                .net
                .connected_peers()
                .contains(&beacon_id)
            {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        connected,
        "follower established a TLS peer connection to the beacon"
    );

    // ---- Follower P-Chain reaches NormalOp (bootstrapped). ----
    let fp = pchain_handle(&follower_handles);
    let bp = pchain_handle(&beacon_handles);
    let finished = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if matches!(**fp.ctx.state.load(), EngineState::NormalOp) {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(finished, "follower P-Chain reached NormalOp via the beacon");

    // ---- Convergence: same last-accepted height. ----
    // For a fresh `local` genesis both P-Chains are at height 0 — the height
    // equality is structural. The load-bearing proof is the gated NormalOp
    // transition (the connectivity gate fires only on a real TLS handshake)
    // and the connection assertion above.
    assert_eq!(
        fp.last_accepted_height, bp.last_accepted_height,
        "follower converged on the beacon's P-Chain tip height"
    );

    // ---- Teardown. ----
    beacon.networking.net.start_close();
    follower.networking.net.start_close();
    let _ = tokio::time::timeout(Duration::from_secs(10), bd).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), fd).await;
}
