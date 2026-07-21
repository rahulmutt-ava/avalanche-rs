// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! cchain-tx-gossip task 14 — offline two-node C-Chain tx gossip e2e.
//!
//! Two real [`ava_node::node::Node`] assemblies on localhost TLS (the exact
//! bring-up `two_node_convergence.rs` uses: a beaconless beacon node and a
//! follower whose sole bootstrapper is the beacon), driven through the
//! production [`drive_startup_chains_over_network`] path. That path dispatches
//! **all** of the `local` network's queued chains — P, X, **and C** (cchain-tx-
//! gossip task 12's real `EvmVm::initialize` gossip wiring runs over the node's
//! real [`ava_engine::networking::vm_app_sender::VmAppSender`], itself backed by
//! the real [`ava_engine::networking::sender::OutboundSender`] over the node's
//! real [`ava_network::network::Network`] — never a no-op/in-process stand-in).
//! So once the follower's TLS peer connection to the beacon is up, both nodes'
//! C-Chain tx-gossip systems (push + pull gossipers, `GossipHandler`) are wired
//! over that same real network path, and a tx admitted on one node's C-Chain
//! pool is observable on the other's — the load-bearing proof this file exists
//! to make.
//!
//! The C-Chain RPC surface is reached the same way `live_block_follow.rs` and
//! `chain_api_routes.rs` reach it: `Vm::create_handlers` → the in-process `/rpc`
//! [`ava_vm::vm::VmHttpService`] → `serve_http`. This fixture boots with no
//! `ava_api::server::Server` (mirroring `two_node_convergence.rs`'s `boot()`,
//! which passes `api_server: None`), so there is no real HTTP listener to hit;
//! the in-process service is the same handler a real HTTP mount would forward
//! to, so this is a faithful "C-Chain RPC" call, not a bypass of it — only the
//! JSON-RPC transport hop is skipped, not the RPC handler itself.
//!
//! # Why these asserts prove GOSSIP (non-vacuity)
//!
//! A tx submitted to node A could in principle reach node B two ways: tx
//! gossip (the system under test) or consensus block propagation (A builds a
//! block containing the tx; B accepts it). This doc originally argued the
//! second path was structurally impossible because `boot_chain_with_sender`
//! builds each chain's validator manager as SELF + `extra_beacons` only, and
//! node A boots with an EMPTY beacon map — so A's C-Chain Snowman engine
//! samples from {A} alone and never *initiates* a consensus message to B.
//!
//! **That argument is INCOMPLETE (cchain-tx-gossip task 16 finding).** It
//! only rules out A pushing a consensus message to B; it says nothing about
//! B *pulling* one from A. `boot_chain_with_sender` (`crates/avalanchers/src/
//! wiring/chains.rs`, the `extra_beacons` loop) registers every explicit
//! bootstrap beacon as a primary-network **validator**, so B's own validator
//! manager contains {A, B} — meaning B's OWN Snowman/SAE consensus engine
//! treats A as an ordinary validator peer it can fetch newly-proposed blocks
//! from via the normal Get/Ancestors block-sync path, entirely independent
//! of (and not gated by) the tx-gossip system. A throwaway diagnostic
//! confirmed this empirically: with BOTH nodes' gossip cadences disabled
//! (traced per-node — each disabled loop's one tick fires once at boot,
//! before the tx exists, and never again), B still eventually observes the
//! submitted tx **already mined** into its own block 1 (surfaced from the
//! accepted-tx index, never having passed through B's own mempool) purely
//! via consensus, while A's own tip stayed at genesis throughout.
//!
//! So: a bare "B's RPC eventually returns this tx, in ANY shape" is
//! vacuous — it can be satisfied by consensus alone, with gossip completely
//! dead, given enough wall-clock time. Only [`push_only_gossip_carries_tx`]
//! currently closes this gap, by additionally requiring a genuine **pending**
//! sighting (`blockHash: null`, every tx-body field populated straight from
//! the pooled tx — see that test's doc comment for the full argument);
//! consensus block-sync can never produce that shape, since it only ever
//! delivers a tx already embedded in an accepted block. The other two tests
//! in this file ([`push_gossip_carries_tx_between_two_real_nodes`],
//! [`pull_gossip_reconciles_when_push_missed`]) still only assert "any
//! sighting" and so are NOT airtight against this consensus-sync fallback —
//! treat them as best-effort until hardened the same way. If a future
//! fixture change makes the validator sets symmetric in the OTHER direction
//! too (A also lists B as a beacon), re-derive this argument again before
//! trusting any test in this file that does not check for a pending sighting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ava_config::flags::{FLAG_SPECS, build_command};
use ava_config::node::Config;
use ava_config::parse::get_node_config;
use ava_config::precedence::Layered;
use ava_crypto::secp256k1::PrivateKey;
use ava_evm_reth::{
    Address, Encodable2718, EvmSignature, SignableTransaction, TransactionSigned, TxKind, TxLegacy,
    U256,
};
use ava_network::network::Network;
use ava_node::node::Node;
use ava_p2p::gossip::GossipParams;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::vm::{VmHttpService, VmRequest};
use avalanchers::wiring::chains::{
    NetworkChainBootHandle, drive_startup_chains_over_network,
    drive_startup_chains_over_network_for_test,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// The well-known "ewoq" pre-funded private key on `local` networks (matches
/// `tests/differential/src/livenet.rs` / `crates/avalanchers/tests/proposal_forwarder.rs`).
/// Address: `0x8db97C7cEcE249c2b98bDC0226Cc4C2A57BF52FC`.
const EWOQ_KEY_HEX: &str = "56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027";

/// The `local` network's C-Chain id (the embedded `genesis_local.json`
/// `cChainGenesis.config.chainId`; matches `proposal_forwarder.rs`).
const CHAIN_ID: u64 = 43_112;

/// A gas price comfortably above the AP3 genesis base fee (225 gwei; matches
/// `proposal_forwarder.rs`).
const GAS_PRICE_WEI: u128 = 300_000_000_000;

/// The ewoq signing key.
fn ewoq_key() -> PrivateKey {
    PrivateKey::from_bytes(&hex::decode(EWOQ_KEY_HEX).expect("ewoq key hex")).expect("ewoq key")
}

/// Builds and signs a funded ewoq self-transfer at `nonce`, returning its
/// EIP-2718-encoded raw bytes (the `eth_sendRawTransaction` wire form) plus its
/// own tx hash (an independent oracle for the "the tx that arrived on B is the
/// SAME tx submitted on A" assertion).
fn signed_ewoq_transfer(nonce: u64) -> (Vec<u8>, String) {
    let key = ewoq_key();
    let ewoq_addr = Address::from(key.public_key().eth_address());
    let tx = TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce,
        gas_price: GAS_PRICE_WEI,
        gas_limit: 21_000,
        to: TxKind::Call(ewoq_addr),
        value: U256::from(1u64),
        input: Default::default(),
    };
    let sig_hash = tx.signature_hash();
    let rsv = key.sign_hash(&sig_hash.0).expect("sign_hash");
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    let signed = TransactionSigned::Legacy(tx.into_signed(sig));
    let hash = format!("0x{}", hex::encode(signed.tx_hash().as_slice()));
    (signed.encoded_2718(), hash)
}

/// Build a local-network, memdb, ephemeral-identity node config under `dir`,
/// with optional `--bootstrap-ids/-ips` (verbatim copy of
/// `two_node_convergence.rs`'s `build_config`).
fn build_config(
    dir: &std::path::Path,
    bootstrap: Option<(NodeId, std::net::SocketAddr)>,
) -> Config {
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

/// Boot every chain the `local` network genesis queues (P, X, C) over the
/// production network path, with production `GossipParams` throughout
/// (mirrors `two_node_convergence.rs`'s `boot`).
async fn boot(node: &Arc<Node>, beacons: BTreeMap<NodeId, u64>) -> Vec<NetworkChainBootHandle> {
    drive_startup_chains_over_network(
        &node.chain_manager,
        node.config.network_id,
        Arc::clone(&node.db),
        Arc::clone(&node.networking.net) as Arc<dyn ava_network::network::Network>,
        Arc::clone(&node.chain_router),
        node.networking.on_sufficiently_connected.clone(),
        beacons,
        None,
        None,
    )
    .await
    .expect("drive_startup_chains_over_network")
}

/// Like [`boot`], but overriding the queued C-Chain's [`GossipParams`] via the
/// task-14 test seam ([`EvmVm::with_gossip_params_for_test`] threaded through
/// [`drive_startup_chains_over_network_for_test`]). Used to disable a node's
/// push gossip so [`pull_gossip_reconciles_when_push_missed`] exercises the
/// pull-only reconciliation path in isolation.
async fn boot_with_cchain_gossip_params(
    node: &Arc<Node>,
    beacons: BTreeMap<NodeId, u64>,
    cchain_gossip_params: GossipParams,
) -> Vec<NetworkChainBootHandle> {
    drive_startup_chains_over_network_for_test(
        &node.chain_manager,
        node.config.network_id,
        Arc::clone(&node.db),
        Arc::clone(&node.networking.net) as Arc<dyn ava_network::network::Network>,
        Arc::clone(&node.chain_router),
        node.networking.on_sufficiently_connected.clone(),
        beacons,
        None,
        Some(cchain_gossip_params),
    )
    .await
    .expect("drive_startup_chains_over_network_for_test")
}

/// Finds the booted C-Chain's handle by the node's own genesis-derived
/// `c_chain_id` (there is no fixed C-Chain-id constant analogous to
/// `PLATFORM_CHAIN_ID` — the C-Chain id is derived from the queued
/// `CreateChainTx`, and each [`Node`] already resolved + cached it at
/// assembly time).
fn cchain_handle(handles: &[NetworkChainBootHandle], c_chain_id: Id) -> &NetworkChainBootHandle {
    handles
        .iter()
        .find(|h| h.ctx.chain.chain_id == c_chain_id)
        .expect("a C-Chain handle")
}

/// Resolves the booted C-Chain VM's in-process `/rpc` [`VmHttpService`] (the
/// same seam `live_block_follow.rs` / `chain_api_routes.rs` use — `Vm::create_handlers`
/// is idempotent and safe to call after boot; it does not disturb the running
/// engine).
async fn eth_rpc_service(handle: &NetworkChainBootHandle) -> Arc<dyn VmHttpService> {
    let token = CancellationToken::new();
    let mut vm = handle.vm.lock().await;
    let handlers = vm
        .create_handlers(&token)
        .await
        .expect("create_handlers on the booted C VM");
    handlers
        .get("/rpc")
        .expect("the C VM exposes /rpc")
        .service
        .clone()
        .expect("/rpc is an in-process service")
}

/// Calls one JSON-RPC `method` with a raw `params` JSON array against `rpc`,
/// returning the parsed reply. Panics with the full reply on a JSON-RPC
/// `error` object — every caller here expects a `result`.
async fn json_rpc(rpc: &Arc<dyn VmHttpService>, method: &str, params: &str) -> Value {
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    let resp = rpc
        .serve_http(VmRequest {
            method: "POST".to_string(),
            uri: "/rpc".to_string(),
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: body.into_bytes(),
        })
        .await;
    let v: Value = serde_json::from_slice(&resp.body).expect("json-rpc reply parses");
    assert!(
        v.get("error").is_none_or(Value::is_null),
        "{method} returned a JSON-RPC error: {v:?}"
    );
    v["result"].clone()
}

/// Submits `raw_tx` via `eth_sendRawTransaction` on `rpc`, returning the tx
/// hash the RPC reports (asserted equal to `want_hash`, the tx's own
/// independently-derived hash).
async fn send_raw_transaction(rpc: &Arc<dyn VmHttpService>, raw_tx: &[u8], want_hash: &str) {
    let params = format!(r#"["0x{}"]"#, hex::encode(raw_tx));
    let got = json_rpc(rpc, "eth_sendRawTransaction", &params).await;
    assert_eq!(
        got.as_str(),
        Some(want_hash),
        "eth_sendRawTransaction returns the tx's own hash"
    );
}

/// Two real `Node`s: node `a` beaconless (its P/X/C-Chains short-circuit to
/// `NormalOp`), node `b` bootstrapping solely off `a`. Returns
/// `(a, a_handles, b, b_handles, dispatch_a, dispatch_b)`; the caller must pump
/// the network dispatch tasks and await TLS connection + C-Chain `NormalOp`
/// before driving gossip.
async fn spawn_two_nodes(
    a_cchain_gossip_params: Option<GossipParams>,
) -> (
    Arc<Node>,
    Vec<NetworkChainBootHandle>,
    Arc<Node>,
    Vec<NetworkChainBootHandle>,
    tokio::task::JoinHandle<ava_network::Result<()>>,
    tokio::task::JoinHandle<ava_network::Result<()>>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    spawn_two_nodes_with_overrides(a_cchain_gossip_params, None).await
}

/// Like [`spawn_two_nodes`], but also allowing an override of node `b`'s
/// queued C-Chain [`GossipParams`] (used by
/// [`push_only_gossip_carries_tx`] to disable `b`'s pull so only push can
/// carry a tx from `a` to `b`).
async fn spawn_two_nodes_with_overrides(
    a_cchain_gossip_params: Option<GossipParams>,
    b_cchain_gossip_params: Option<GossipParams>,
) -> (
    Arc<Node>,
    Vec<NetworkChainBootHandle>,
    Arc<Node>,
    Vec<NetworkChainBootHandle>,
    tokio::task::JoinHandle<ava_network::Result<()>>,
    tokio::task::JoinHandle<ava_network::Result<()>>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();

    let a_cfg = Arc::new(build_config(a_dir.path(), None));
    let log_factory = ava_node::logging_test_factory(&a_cfg);
    let a = Node::new(
        Arc::clone(&a_cfg),
        Arc::clone(&log_factory),
        tokio::runtime::Handle::current(),
    )
    .await
    .expect("node a Node::new");
    let a_id = a.id;
    let a_addr = a.networking.staking_address;
    let a_handles = match a_cchain_gossip_params {
        Some(params) => boot_with_cchain_gossip_params(&a, BTreeMap::new(), params).await,
        None => boot(&a, BTreeMap::new()).await,
    };

    let b_cfg = Arc::new(build_config(b_dir.path(), Some((a_id, a_addr))));
    let b = Node::new(
        Arc::clone(&b_cfg),
        Arc::clone(&log_factory),
        tokio::runtime::Handle::current(),
    )
    .await
    .expect("node b Node::new");
    let mut beacons = BTreeMap::new();
    beacons.insert(a_id, 1u64);
    let b_handles = match b_cchain_gossip_params {
        Some(params) => boot_with_cchain_gossip_params(&b, beacons, params).await,
        None => boot(&b, beacons).await,
    };

    let a_net = Arc::clone(&a.networking.net);
    let b_net = Arc::clone(&b.networking.net);
    let ad = tokio::spawn(async move { a_net.dispatch().await });
    let bd = tokio::spawn(async move { b_net.dispatch().await });

    (a, a_handles, b, b_handles, ad, bd, a_dir, b_dir)
}

/// Waits (bounded) for `follower` to establish a TLS peer connection to
/// `beacon_id` — the load-bearing precondition every real-network gossip test
/// needs (mirrors `two_node_convergence.rs`'s rung-1 assertion).
async fn await_connected(follower: &Node, beacon_id: NodeId) {
    let connected = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if follower
                .networking
                .net
                .connected_peers()
                .contains(&beacon_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        connected.is_ok(),
        "follower established a TLS peer connection to the beacon"
    );
}

/// Waits (bounded) for a booted chain's engine to reach `NormalOp`.
async fn await_normalop(handle: &NetworkChainBootHandle, within: Duration) {
    let reached = tokio::time::timeout(within, async {
        loop {
            if matches!(**handle.ctx.state.load(), ava_snow::EngineState::NormalOp) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(reached.is_ok(), "chain reached NormalOp");
}

/// Cleanly tears down both nodes' network dispatch loops (mirrors
/// `two_node_convergence.rs`'s teardown).
async fn teardown(
    a: &Node,
    b: &Node,
    ad: tokio::task::JoinHandle<ava_network::Result<()>>,
    bd: tokio::task::JoinHandle<ava_network::Result<()>>,
) {
    a.networking.net.start_close();
    b.networking.net.start_close();
    let _ = tokio::time::timeout(Duration::from_secs(10), ad).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), bd).await;
}

/// **Push gossip carries a tx between two real nodes.** Submits a signed
/// raw transfer to node A's C-Chain RPC and polls node B's C-Chain
/// `eth_getTransactionByHash` (bounded ~30s) until the SAME tx is visible on
/// B. Proves the full push path end-to-end: push emit (A) → wire (real
/// localhost TLS) → decode → adapter → VM → mux → handler → remote admission
/// (B).
///
/// **Not asserted: `blockHash: null` (pending-only).** Once the tx is
/// gossiped into B's own EVM pool, B's own per-chain proposal forwarder (see
/// `proposal_forwarder.rs`) observes the pending work and drives its engine
/// to build + accept a block, same as any locally-submitted tx would — this
/// two-node fixture's single-validator-per-chain consensus resolves fast
/// enough on localhost that B often mines the tx before this test's first
/// poll ever observes it merely pool-pending. Either shape (`blockHash: null`
/// or a real mined block) is equally valid proof gossip carried the tx across
/// the wire — B never received it any other way — so only the hash match is
/// asserted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_gossip_carries_tx_between_two_real_nodes() {
    let (a, a_handles, b, b_handles, ad, bd, _a_dir, _b_dir) = spawn_two_nodes(None).await;

    await_connected(&b, a.id).await;

    let a_c = cchain_handle(&a_handles, a.c_chain_id);
    let b_c = cchain_handle(&b_handles, b.c_chain_id);
    await_normalop(a_c, Duration::from_secs(30)).await;
    await_normalop(b_c, Duration::from_secs(30)).await;

    let a_rpc = eth_rpc_service(a_c).await;
    let b_rpc = eth_rpc_service(b_c).await;

    let (raw_tx, tx_hash) = signed_ewoq_transfer(0);
    send_raw_transaction(&a_rpc, &raw_tx, &tx_hash).await;

    // Poll B's eth_getTransactionByHash (bounded ~30s; production push_period
    // is 100ms, so this should converge in well under a second).
    let params = format!(r#"["{tx_hash}"]"#);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut seen: Option<Value> = None;
    while tokio::time::Instant::now() < deadline {
        let got = json_rpc(&b_rpc, "eth_getTransactionByHash", &params).await;
        if !got.is_null() {
            seen = Some(got);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let got = seen.expect(
        "push gossip must carry the tx from A to B within 30s (eth_getTransactionByHash \
         stayed null on B)",
    );
    assert_eq!(
        got["hash"].as_str(),
        Some(tx_hash.as_str()),
        "the tx B observed is the SAME tx submitted on A: {got:?}"
    );

    teardown(&a, &b, ad, bd).await;
}

/// **Pull gossip reconciles when push is missed.** Node A boots its C-Chain
/// with push gossip effectively disabled (`push_period: 1h`, via the task-14
/// `EvmVm::with_gossip_params_for_test` seam); node B keeps the production
/// 1s pull cadence. A tx admitted on A's pool is never actively pushed, yet
/// B's periodic pull must still fetch it (bounded ~15s) — proving the pull
/// path reconciles independently of push. (See the sibling push test's doc
/// comment for why `blockHash` is not asserted null: B's own consensus can
/// mine the tx before this test's first poll observes it.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_gossip_reconciles_when_push_missed() {
    let a_gossip_params = GossipParams {
        push_period: Duration::from_secs(3600),
        ..GossipParams::default()
    };
    let (a, a_handles, b, b_handles, ad, bd, _a_dir, _b_dir) =
        spawn_two_nodes(Some(a_gossip_params)).await;

    await_connected(&b, a.id).await;

    let a_c = cchain_handle(&a_handles, a.c_chain_id);
    let b_c = cchain_handle(&b_handles, b.c_chain_id);
    await_normalop(a_c, Duration::from_secs(30)).await;
    await_normalop(b_c, Duration::from_secs(30)).await;

    let a_rpc = eth_rpc_service(a_c).await;
    let b_rpc = eth_rpc_service(b_c).await;

    // Seed A's pool. With push_period == 1h, `initialize`'s one immediate
    // `tokio::time::interval` tick (which fires with an empty outbox, before
    // this tx is admitted) is the only push cycle that will run for the rest
    // of the test — any convergence on B must come from B's own 1s pull.
    let (raw_tx, tx_hash) = signed_ewoq_transfer(0);
    send_raw_transaction(&a_rpc, &raw_tx, &tx_hash).await;

    // Poll B (bounded ~15s; production pull_period is 1s, so a few pull
    // cycles must suffice).
    let params = format!(r#"["{tx_hash}"]"#);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut seen: Option<Value> = None;
    while tokio::time::Instant::now() < deadline {
        let got = json_rpc(&b_rpc, "eth_getTransactionByHash", &params).await;
        if !got.is_null() {
            seen = Some(got);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let got = seen.expect(
        "pull gossip must reconcile the tx from A to B within 15s even though A's push \
         is disabled (eth_getTransactionByHash stayed null on B)",
    );
    assert_eq!(
        got["hash"].as_str(),
        Some(tx_hash.as_str()),
        "the tx B observed via pull is the SAME tx submitted on A: {got:?}"
    );

    teardown(&a, &b, ad, bd).await;
}

/// **Push gossip carries a tx between two real nodes, with pull disabled on
/// the observer.** The sibling `push_gossip_carries_tx_between_two_real_nodes`
/// test does not disable B's pull cadence, so it cannot tell push and pull
/// apart — B's periodic 1s pull could carry the tx on its own even if push
/// were completely dead. This test closes that gap: B boots its C-Chain with
/// pull effectively disabled (`pull_period: 1h`, via the task-14
/// `EvmVm::with_gossip_params_for_test` seam threaded through
/// [`spawn_two_nodes_with_overrides`]); A keeps the production 100ms push
/// cadence. A tx admitted on A's pool can therefore ONLY reach B via push.
///
/// # Why this asserts a genuine **pending** sighting, not just "any" sighting
/// (cchain-tx-gossip task 16 finding)
///
/// This file's top-of-file "non-vacuity" argument — that consensus block
/// propagation from A to B is structurally impossible because A boots
/// beaconless — is **incomplete**: `boot_chain_with_sender`
/// (`crates/avalanchers/src/wiring/chains.rs`, the `extra_beacons` loop)
/// registers every explicit bootstrap beacon as a primary-network
/// **validator** too, so B's OWN validator manager contains {A, B}. That
/// makes B, as a matter of ordinary Snowman/SAE consensus (not gossip), a
/// legitimate peer to fetch a newly-proposed block from A via the normal
/// Get/Ancestors block-sync path — entirely independent of, and not gated
/// by, the C-Chain tx-gossip `GossipParams` this test overrides. A
/// throwaway diagnostic during this investigation confirmed the gap
/// empirically: with BOTH A's push and B's pull verifiably disabled (traced
/// per-node — each loop's one disabled tick fires once at boot and never
/// again), B still eventually observes the SAME tx **already mined** into
/// its own block 1 (via the accepted-tx index, never having passed through
/// its own mempool) while A's own tip stayed at genesis — i.e., ordinary
/// consensus block-sync, not gossip, carried it.
///
/// This means a bare "`eth_getTransactionByHash` ever returns non-null" — as
/// this test previously asserted — cannot distinguish genuine gossip
/// delivery from that consensus-sync fallback and would silently keep
/// "passing" even if push were completely dead, given enough wall-clock
/// time. The fix: poll tightly (no inter-poll sleep) and require that B's
/// RPC report the tx in the **pending** shape (`blockHash: null`, every
/// tx-body field populated straight from the pooled `RecoveredTx` — see
/// `rpc/eth.rs`'s `eth_getTransactionByHash` doc) at least once before it is
/// ever mined. Consensus block-sync delivers a tx only already embedded in
/// an accepted block, so it can NEVER produce a pending sighting — only
/// genuine mempool-level admission (i.e., [`ava_p2p::gossip`]'s
/// `Set::add`, called from the inbound `GossipHandler`) can. This was
/// verified empirically too: repeated runs of this exact test reliably
/// observe the pending shape before the mined shape, versus the
/// both-disabled diagnostic never observing it once across 100k+ tight
/// polls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_only_gossip_carries_tx() {
    let b_gossip_params = GossipParams {
        pull_period: Duration::from_secs(3600),
        ..GossipParams::default()
    };
    let (a, a_handles, b, b_handles, ad, bd, _a_dir, _b_dir) =
        spawn_two_nodes_with_overrides(None, Some(b_gossip_params)).await;

    await_connected(&b, a.id).await;

    let a_c = cchain_handle(&a_handles, a.c_chain_id);
    let b_c = cchain_handle(&b_handles, b.c_chain_id);
    await_normalop(a_c, Duration::from_secs(30)).await;
    await_normalop(b_c, Duration::from_secs(30)).await;

    let a_rpc = eth_rpc_service(a_c).await;
    let b_rpc = eth_rpc_service(b_c).await;

    let (raw_tx, tx_hash) = signed_ewoq_transfer(0);
    send_raw_transaction(&a_rpc, &raw_tx, &tx_hash).await;

    // Poll B as tightly as possible (bounded ~30s; production push_period is
    // 100ms, so this should converge in well under a second if push actually
    // reaches the wire) — no inter-poll sleep, so a genuinely-pending sighting
    // (see the doc comment above) isn't missed to a 100ms polling gap.
    let params = format!(r#"["{tx_hash}"]"#);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut seen: Option<Value> = None;
    let mut ever_pending = false;
    while tokio::time::Instant::now() < deadline {
        let got = json_rpc(&b_rpc, "eth_getTransactionByHash", &params).await;
        if !got.is_null() {
            if got["blockHash"].is_null() {
                ever_pending = true;
                continue;
            }
            seen = Some(got);
            break;
        }
    }
    let got = seen.expect(
        "push gossip must carry the tx from A to B within 30s even though B's pull is \
         disabled (eth_getTransactionByHash stayed null on B)",
    );
    assert_eq!(
        got["hash"].as_str(),
        Some(tx_hash.as_str()),
        "the tx B observed via push is the SAME tx submitted on A: {got:?}"
    );
    assert!(
        ever_pending,
        "B must observe the tx in the PENDING shape (blockHash: null) at least once before \
         it is mined — a sighting that is ALWAYS already-mined is consistent with ordinary \
         consensus block-sync (see this test's doc comment), not genuine push-gossip \
         mempool admission, and would pass even with push completely broken"
    );

    // Second sequential tx: the push loop must SURVIVE its first non-empty
    // batch (live debugging found a one-drain-then-silent pattern — a loop
    // that dies after its first send still passes a single-delivery assert).
    let (raw_tx2, tx_hash2) = signed_ewoq_transfer(1);
    send_raw_transaction(&a_rpc, &raw_tx2, &tx_hash2).await;
    let params2 = format!(r#"["{tx_hash2}"]"#);
    let deadline2 = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut second_seen = false;
    while tokio::time::Instant::now() < deadline2 {
        let got = json_rpc(&b_rpc, "eth_getTransactionByHash", &params2).await;
        if !got.is_null() {
            second_seen = true;
            break;
        }
    }
    assert!(
        second_seen,
        "push gossip must ALSO carry a SECOND tx submitted after the first was delivered — \
         a push loop that dies after its first non-empty batch fails this leg \
         (eth_getTransactionByHash stayed null on B for tx2 {tx_hash2})"
    );

    teardown(&a, &b, ad, bd).await;
}

/// **Repro (task 16 live debugging): push loop must survive concurrent
/// inbound App load.** `push_only_gossip_carries_tx` proved the push loop
/// survives two sequential sends with near-zero concurrent App traffic — the
/// live topology (5 Go validators + this Rust node) instead dies after AT
/// MOST one non-empty push batch, and the only structural difference the
/// live topology has that the two-node test doesn't is a *constant* stream of
/// concurrent inbound `AppRequest`/`AppResponse` traffic hitting the source
/// node's C-Chain while its push loop is independently trying to drain and
/// send.
///
/// This test reproduces that shape offline: node `a` is the source (default
/// `GossipParams`, so its push loop runs on the production 100ms cadence);
/// node `b` is the checked observer, with pull disabled
/// (`pull_period: 1h`) so a tx `b` observes can only have arrived via `a`'s
/// push loop — same non-vacuity argument
/// [`push_only_gossip_carries_tx`]'s doc comment makes. `HAMMER_COUNT`
/// additional nodes also bootstrap off `a` with an aggressive pull cadence
/// (`pull_period: 2ms`) and are never asked what they observed — they exist
/// purely to keep `a`'s C-Chain fielding a continuous stream of concurrent
/// inbound pull `AppRequest`s (and the `AppResponse`s `a` sends back),
/// exactly the "every inbound App op takes the chain's VM mutex, inline,
/// while `a`'s push loop separately needs the mempool lock the SAME
/// `AppRequest` answers hold for their whole `Set::iterate` duration" shape
/// task 16's fact-sheet flags as the leading suspect. `TX_COUNT` sequential
/// txs are submitted to `a`; every one must reach `b` — a push loop that
/// dies (or is merely starved forever) after its first non-empty batch under
/// this load fails on tx #1 already.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_survives_concurrent_inbound_pull_load() {
    const HAMMER_COUNT: usize = 4;
    const TX_COUNT: u64 = 4;

    let a_dir = tempfile::tempdir().unwrap();
    let a_cfg = Arc::new(build_config(a_dir.path(), None));
    let log_factory = ava_node::logging_test_factory(&a_cfg);
    let a = Node::new(
        Arc::clone(&a_cfg),
        Arc::clone(&log_factory),
        tokio::runtime::Handle::current(),
    )
    .await
    .expect("node a Node::new");
    let a_id = a.id;
    let a_addr = a.networking.staking_address;
    let a_handles = boot(&a, BTreeMap::new()).await;

    let b_dir = tempfile::tempdir().unwrap();
    let b_cfg = Arc::new(build_config(b_dir.path(), Some((a_id, a_addr))));
    let b = Node::new(
        Arc::clone(&b_cfg),
        Arc::clone(&log_factory),
        tokio::runtime::Handle::current(),
    )
    .await
    .expect("node b Node::new");
    let mut b_beacons = BTreeMap::new();
    b_beacons.insert(a_id, 1u64);
    let b_gossip_params = GossipParams {
        pull_period: Duration::from_secs(3600),
        ..GossipParams::default()
    };
    let b_handles = boot_with_cchain_gossip_params(&b, b_beacons, b_gossip_params).await;

    // The hammer nodes: aggressive pull cadence against `a`, never checked
    // for content — see the doc comment above.
    let mut hammer_dirs = Vec::with_capacity(HAMMER_COUNT);
    let mut hammer_nodes = Vec::with_capacity(HAMMER_COUNT);
    let mut hammer_dispatch = Vec::with_capacity(HAMMER_COUNT);
    for _ in 0..HAMMER_COUNT {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Arc::new(build_config(dir.path(), Some((a_id, a_addr))));
        let node = Node::new(
            Arc::clone(&cfg),
            Arc::clone(&log_factory),
            tokio::runtime::Handle::current(),
        )
        .await
        .expect("hammer Node::new");
        let mut beacons = BTreeMap::new();
        beacons.insert(a_id, 1u64);
        let hammer_params = GossipParams {
            pull_period: Duration::from_millis(2),
            ..GossipParams::default()
        };
        let handles = boot_with_cchain_gossip_params(&node, beacons, hammer_params).await;
        let net = Arc::clone(&node.networking.net);
        let dispatch = tokio::spawn(async move { net.dispatch().await });
        hammer_nodes.push((node, handles));
        hammer_dispatch.push(dispatch);
        hammer_dirs.push(dir);
    }

    let a_net = Arc::clone(&a.networking.net);
    let b_net = Arc::clone(&b.networking.net);
    let ad = tokio::spawn(async move { a_net.dispatch().await });
    let bd = tokio::spawn(async move { b_net.dispatch().await });

    await_connected(&b, a_id).await;
    for (node, _) in &hammer_nodes {
        await_connected(node, a_id).await;
    }

    let a_c = cchain_handle(&a_handles, a.c_chain_id);
    let b_c = cchain_handle(&b_handles, b.c_chain_id);
    await_normalop(a_c, Duration::from_secs(30)).await;
    await_normalop(b_c, Duration::from_secs(30)).await;
    for (node, handles) in &hammer_nodes {
        let c = cchain_handle(handles, node.c_chain_id);
        await_normalop(c, Duration::from_secs(30)).await;
    }

    let a_rpc = eth_rpc_service(a_c).await;
    let b_rpc = eth_rpc_service(b_c).await;

    for nonce in 0..TX_COUNT {
        let (raw_tx, tx_hash) = signed_ewoq_transfer(nonce);
        send_raw_transaction(&a_rpc, &raw_tx, &tx_hash).await;

        let params = format!(r#"["{tx_hash}"]"#);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut seen = false;
        while tokio::time::Instant::now() < deadline {
            let got = json_rpc(&b_rpc, "eth_getTransactionByHash", &params).await;
            if !got.is_null() {
                seen = true;
                break;
            }
        }
        assert!(
            seen,
            "push gossip must carry tx #{nonce} ({tx_hash}) from A to B within 20s while A \
             concurrently fields inbound pull-gossip AppRequests from {HAMMER_COUNT} hammer \
             nodes — a push loop that dies (or is starved forever) under concurrent App load \
             fails this leg (eth_getTransactionByHash stayed null on B)"
        );
    }

    teardown(&a, &b, ad, bd).await;
    for ((node, _), dispatch) in hammer_nodes.into_iter().zip(hammer_dispatch) {
        node.networking.net.start_close();
        let _ = tokio::time::timeout(Duration::from_secs(10), dispatch).await;
    }
}
