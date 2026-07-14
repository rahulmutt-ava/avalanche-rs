// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 rung 2 — the live chain-boot path registers each booted chain's HTTP
//! handlers with the node's [`ava_api::server::Server`], so a live
//! `avalanchers` node serves `/ext/bc/P`, `/ext/bc/X`, and `/ext/bc/C/rpc`
//! instead of 404 (mirror Go `chains/manager.go` calling
//! `server.RegisterChain` after chain creation).
//!
//! The mount machinery itself (paths, `bc/P`-style aliases, the
//! not-bootstrapped 503 layer) is the M8.22 `ava-api` register seam, already
//! tested at that level — this test pins ONLY the wiring: after
//! `run_queued_chains_with_db` boots the queued P/X/C chains with an API
//! server supplied, the server's composed router resolves the chain mounts and
//! a real `platform.getHeight` round-trips through the mounted handler.

use std::sync::Arc;
use std::time::Duration;

use ava_api::server::Server;
use ava_config::node::{ApiConfig, HttpConfig};
use ava_database::{DynDatabase, MemDb};
use ava_node::init::chain_manager::{AssemblyChainManager, PLATFORM_CHAIN_ID, init_chains};
use ava_types::node_id::NodeId;
use ava_validators::{DefaultManager, ValidatorManager};
use avalanchers::wiring::chains::run_queued_chains_with_db;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// A minimal API server (never bound to a socket — the test drives its
/// composed router in-process), mirroring the `ava-api` register-seam test
/// fixture.
fn test_server() -> Arc<Server> {
    let config = HttpConfig {
        read_timeout: Duration::from_secs(30),
        read_header_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(120),
        api_config: ApiConfig {
            index_api_enabled: false,
            index_allow_incomplete: false,
            admin_api_enabled: false,
            info_api_enabled: true,
            metrics_api_enabled: true,
            health_api_enabled: true,
        },
        http_host: "127.0.0.1".to_string(),
        http_port: 0,
        https_enabled: false,
        https_key: Vec::new(),
        https_cert: Vec::new(),
        http_allowed_origins: vec!["*".to_string()],
        http_allowed_hosts: vec!["*".to_string()],
        shutdown_timeout: Duration::from_secs(10),
        shutdown_wait: Duration::ZERO,
    };
    Arc::new(Server::new(
        config,
        NodeId::from_slice(&[7u8; 20]).expect("node id"),
    ))
}

/// POST a JSON-RPC body at `uri` through the composed router; returns
/// `(status, body)`.
async fn post_json(router: &axum::Router, uri: &str, body: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The chain creator registers each booted chain's `create_handlers` with the
/// supplied API server: the P-Chain mounts at `/ext/bc/<chainID>` with the
/// `bc/P` alias resolving, a live `platform.getHeight` round-trips through the
/// mounted handler, the X mount answers, and the EVM RPC lands at
/// `/ext/bc/C/rpc` (extension `"rpc"`).
#[tokio::test]
async fn run_queued_chains_registers_chain_api_routes() {
    let network_id = 1u32; // mainnet embedded genesis (M8-complete source).
    let (genesis_bytes, _avax_asset_id) =
        ava_genesis::genesis_bytes(network_id, None).expect("build genesis bytes");

    // Assemble + queue exactly as `init_chain_manager`/`init_chains` do.
    let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
    let critical = std::iter::once(PLATFORM_CHAIN_ID).collect();
    let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));
    init_chains(&manager, &genesis_bytes).expect("queue the P-, X- and C-Chains");
    let queued = manager.queued_chains();
    let x_chain_id = queued
        .iter()
        .find(|p| p.id != PLATFORM_CHAIN_ID && p.vm_id == ava_node::init::chain_manager::avm_id())
        .expect("X chain queued")
        .id;
    let c_chain_id = queued
        .iter()
        .find(|p| p.vm_id == ava_node::init::chain_manager::evm_id())
        .expect("C chain queued")
        .id;

    // The node's API server, threaded into the chain creator (the seam under
    // test — before the wiring the server never learns about the chains and
    // every /ext/bc/* probe 404s, exactly like the live node).
    let server = test_server();

    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let handles = run_queued_chains_with_db(&manager, network_id, base, Some(&server))
        .await
        .expect("boot the queued P-, X- and C-Chains with the api server supplied");
    assert_eq!(handles.len(), 3, "P, X and C all boot");

    // Wait for all three engines to reach NormalOp so the per-chain
    // not-bootstrapped 503 layer admits the round-trips.
    let mut ready = false;
    for _ in 0..400_000 {
        if manager.is_bootstrapped(PLATFORM_CHAIN_ID)
            && manager.is_bootstrapped(x_chain_id)
            && manager.is_bootstrapped(c_chain_id)
        {
            ready = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(ready, "P, X and C all reach NormalOp");

    // The composed router now carries the chain mounts (the live node builds
    // this exact router when `serve()` starts, after chain boot).
    let router = server.build_router().expect("build_router()");

    // P: the canonical /ext/bc/<chainID> mount AND the bc/P alias both resolve
    // a live platform.getHeight round-trip through the mounted handler.
    let get_height = r#"{"jsonrpc":"2.0","id":1,"method":"platform.getHeight","params":{}}"#;
    let canonical = format!("/ext/bc/{PLATFORM_CHAIN_ID}");
    let (status, body) = post_json(&router, &canonical, get_height).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "platform.getHeight on the canonical P mount ({canonical}): {body}"
    );
    let (status, body) = post_json(&router, "/ext/bc/P", get_height).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "platform.getHeight on the /ext/bc/P alias: {body}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&body).expect("platform.getHeight reply is JSON");
    assert!(
        json.get("result").and_then(|r| r.get("height")).is_some(),
        "platform.getHeight returns a result.height: {body}"
    );

    // X: the /ext/bc/X alias resolves the X-Chain mount (a JSON-RPC reply, not
    // a 404 — a method-level reply shape is the avm service's own contract).
    let (status, body) = post_json(
        &router,
        "/ext/bc/X",
        r#"{"jsonrpc":"2.0","id":1,"method":"avm.getAssetDescription","params":{"assetID":"AVAX"}}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the /ext/bc/X alias resolves: {body}"
    );

    // C: the EVM RPC lands at extension "rpc" ⇒ /ext/bc/C/rpc serves
    // eth_blockNumber.
    let (status, body) = post_json(
        &router,
        "/ext/bc/C/rpc",
        r#"{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "eth_blockNumber on /ext/bc/C/rpc: {body}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&body).expect("eth_blockNumber reply is JSON");
    assert!(
        json.get("result").is_some(),
        "eth_blockNumber returns a result: {body}"
    );

    manager.shutdown(Duration::from_secs(5)).await;
    for handle in handles {
        handle.join.await.expect("handler task joined cleanly");
    }
}
