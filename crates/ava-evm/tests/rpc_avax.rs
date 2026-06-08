// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `rpc_avax` — golden request→response tests for the `avax.*` RPC namespace +
//! the `admin.*` / health handlers (G8, spec 10 §9.2/§17.9, M6.24).
//!
//! Like the `eth_*` handlers (M6.23, `tests/rpc_eth.rs`), these are plain Rust
//! handlers that return `serde_json::Value` — NOT a `jsonrpsee`/`reth-rpc` server
//! (the jsonrpsee-vs-axum mount topology is deferred to the 12-node milestone,
//! spec §9.2). Each method's JSON shape mirrors coreth's `avax` service
//! (`plugin/evm/atomic/vm/api.go`) + `admin.go`/`health.go`. Golden request /
//! response vectors + provenance live under `tests/vectors/cchain/rpc/`.

use std::sync::Arc;

use ava_avm::txs::components::{Output as FxOutput, TransferableOutput};
use ava_database::MemDb;
use ava_evm::atomic::mempool::AtomicMempool;
use ava_evm::atomic::tx::{AtomicTx, EvmInput, Tx, UnsignedExportTx};
use ava_evm::canonical::CanonicalStore;
use ava_evm::rpc::admin::AdminRpc;
use ava_evm::rpc::avax::{AcceptedAtomicTxIndex, AvaxRpc, IssueTxArgs};
use ava_evm_reth::B256;
use ava_secp256k1fx::{OutputOwners, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use parking_lot::Mutex;
use serde_json::Value;

// ─── Fixtures ────────────────────────────────────────────────────────────────

/// 32-byte id with every byte = `b`.
fn id32(b: u8) -> Id {
    Id::from([b; 32])
}

/// The deterministic AVAX asset id (0xAA × 32).
fn avax_asset() -> Id {
    id32(0xAA)
}

/// A signed (initialized) golden export atomic tx (the same shape used in the
/// atomic-tx codec golden vectors). Deterministic id.
fn golden_tx() -> Tx {
    let unsigned = UnsignedExportTx {
        network_id: 1,
        blockchain_id: id32(0x11),
        destination_chain: id32(0x33),
        ins: vec![EvmInput {
            address: [0x02; 20],
            amount: 3000,
            asset_id: avax_asset(),
            nonce: 7,
        }],
        exported_outputs: vec![TransferableOutput {
            asset_id: avax_asset(),
            out: FxOutput::SecpTransfer(TransferOutput {
                amt: 3000,
                owners: OutputOwners {
                    locktime: 0,
                    threshold: 1,
                    addrs: vec![ShortId::from([0x05; 20])],
                },
            }),
        }],
    };
    let mut tx = Tx::new(AtomicTx::Export(unsigned));
    tx.initialize().expect("initialize");
    tx
}

/// Builds an `AvaxRpc` with a fresh mempool (capacity 100, AVAX asset),
/// a canonical store advanced to height 5, and an empty accepted-tx index.
fn setup() -> (
    AvaxRpc,
    Arc<Mutex<AtomicMempool>>,
    Arc<AcceptedAtomicTxIndex>,
) {
    let mempool = Arc::new(Mutex::new(AtomicMempool::new(100, avax_asset())));

    let canon_db: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let canonical = Arc::new(CanonicalStore::new(canon_db));
    for n in 1..=5u64 {
        canonical
            .append_canonical(
                n,
                B256::repeat_byte(n as u8),
                B256::repeat_byte(0xa0 + n as u8),
                format!("body-{n}").as_bytes(),
                &[],
            )
            .expect("append");
    }

    let accepted = Arc::new(AcceptedAtomicTxIndex::new());
    let rpc = AvaxRpc::new(
        Arc::clone(&mempool),
        Arc::clone(&canonical),
        Arc::clone(&accepted),
    );
    (rpc, mempool, accepted)
}

fn vector(name: &str) -> Value {
    let raw = std::fs::read_to_string(format!(
        "{}/tests/vectors/cchain/rpc/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("read vector");
    serde_json::from_str(&raw).expect("parse vector")
}

/// The checksummed-hex (`formatting.Hex`) encoding of `bytes`:
/// `0x<hex(bytes ++ sha256(bytes)[28..32])>`.
fn hex_cs(bytes: &[u8]) -> String {
    let cs = ava_crypto::hashing::checksum(bytes, 4);
    let mut combined = bytes.to_vec();
    combined.extend_from_slice(&cs);
    format!("0x{}", hex::encode(&combined))
}

// ─── avax.issueTx ─────────────────────────────────────────────────────────────

#[test]
fn issue_tx_admits_to_mempool_and_returns_txid() {
    let (rpc, mempool, _accepted) = setup();
    let tx = golden_tx();
    let want = vector("avax_issueTx");

    let args = IssueTxArgs {
        tx: hex_cs(tx.bytes()),
        encoding: "hex".to_string(),
    };
    let got = rpc.issue_tx(args).expect("issue_tx");

    // The returned txID is the CB58 of the atomic tx id.
    assert_eq!(got["txID"], want["result"]["txID"]);
    assert_eq!(got["txID"], Value::String(tx.id().to_string()));
    // The tx is now Processing in the mempool.
    assert!(mempool.lock().has(&tx.id()));
}

// ─── avax.getAtomicTxStatus ───────────────────────────────────────────────────

#[test]
fn get_atomic_tx_status_processing_dropped_accepted_unknown() {
    let (rpc, mempool, accepted) = setup();
    let tx = golden_tx();

    // Unknown — never seen.
    let st = rpc.get_atomic_tx_status(tx.id()).expect("status");
    assert_eq!(st["status"], Value::String("Unknown".to_string()));
    assert!(st.get("blockHeight").is_none());

    // Processing — in the mempool.
    mempool.lock().add_local(tx.clone()).expect("add");
    let st = rpc.get_atomic_tx_status(tx.id()).expect("status");
    assert_eq!(st["status"], Value::String("Processing".to_string()));

    // Accepted — recorded in the accepted index at height 3 (takes precedence).
    accepted.put(tx.id(), tx.bytes().to_vec(), 3);
    let want = vector("avax_getAtomicTxStatus");
    let st = rpc.get_atomic_tx_status(tx.id()).expect("status");
    assert_eq!(st["status"], want["result"]["status"]);
    assert_eq!(st["blockHeight"], want["result"]["blockHeight"]);

    // An unseen id is Unknown. (The Dropped branch — a discarded mempool tx — is
    // exercised by the focused unit test in `avax.rs`,
    // `get_atomic_tx_status_reports_dropped_for_a_discarded_tx`, since driving a
    // tx Current→Discarded needs the mempool's batch lifecycle.)
    let other = id32(0x77);
    let st = rpc.get_atomic_tx_status(other).expect("status");
    assert_eq!(st["status"], Value::String("Unknown".to_string()));
}

// ─── avax.getAtomicTx ─────────────────────────────────────────────────────────

#[test]
fn get_atomic_tx_returns_signed_bytes_and_height() {
    let (rpc, _mempool, accepted) = setup();
    let tx = golden_tx();
    accepted.put(tx.id(), tx.bytes().to_vec(), 3);

    let want = vector("avax_getAtomicTx");
    let got = rpc
        .get_atomic_tx(tx.id(), "hex".to_string())
        .expect("get_atomic_tx");

    assert_eq!(got["tx"], Value::String(hex_cs(tx.bytes())));
    assert_eq!(got["tx"], want["result"]["tx"]);
    assert_eq!(got["encoding"], Value::String("hex".to_string()));
    assert_eq!(got["blockHeight"], want["result"]["blockHeight"]);
}

#[test]
fn get_atomic_tx_unknown_is_error() {
    let (rpc, _mempool, _accepted) = setup();
    let err = rpc
        .get_atomic_tx(id32(0xEE), "hex".to_string())
        .unwrap_err();
    assert!(err.to_string().contains("could not find tx"));
}

// ─── avax.getBlockByHeight ────────────────────────────────────────────────────

#[test]
fn get_block_by_height_returns_body_bytes() {
    let (rpc, _mempool, _accepted) = setup();
    let want = vector("avax_getBlockByHeight");
    let got = rpc
        .get_block_by_height(3, "hex".to_string())
        .expect("get_block_by_height");
    // The canonical body bytes for height 3 are "body-3".
    assert_eq!(got["block"], Value::String(hex_cs(b"body-3")));
    assert_eq!(got["block"], want["result"]["block"]);
    assert_eq!(got["encoding"], Value::String("hex".to_string()));
}

#[test]
fn get_block_by_height_missing_is_error() {
    let (rpc, _mempool, _accepted) = setup();
    let err = rpc.get_block_by_height(99, "hex".to_string()).unwrap_err();
    assert!(err.to_string().contains("not found") || err.to_string().contains("could not"));
}

// ─── avax.getUTXOs (deferred shape) ───────────────────────────────────────────

#[test]
fn get_utxos_returns_empty_paginated_reply() {
    let (rpc, _mempool, _accepted) = setup();
    let want = vector("avax_getUTXOs");
    // No shared-memory UTXOs are indexed in this test harness, so the reply is
    // the empty paginated shape (numFetched 0, no utxos). The address parse +
    // reply envelope match coreth's GetUTXOsReply.
    let got = rpc
        .get_utxos(
            &["C-avax1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqkr6ykw".to_string()],
            "X",
            0,
        )
        .expect("get_utxos");
    assert_eq!(got["numFetched"], want["result"]["numFetched"]);
    assert_eq!(got["utxos"], want["result"]["utxos"]);
    assert_eq!(got["encoding"], Value::String("hex".to_string()));
}

// ─── admin.* + health ─────────────────────────────────────────────────────────

#[test]
fn admin_methods_respond_with_empty_reply() {
    let admin = AdminRpc::new();
    // SetLogLevel returns the empty reply ({}).
    let reply = admin.set_log_level("info").expect("set_log_level");
    assert_eq!(reply, serde_json::json!({}));
    // Profiler endpoints return the empty reply too (no-op in this build).
    assert_eq!(
        admin.start_cpu_profiler().expect("start"),
        serde_json::json!({})
    );
    assert_eq!(
        admin.stop_cpu_profiler().expect("stop"),
        serde_json::json!({})
    );
}

#[test]
fn health_check_reports_healthy() {
    let (rpc, _mempool, _accepted) = setup();
    // coreth's HealthCheck returns (nil, nil) — healthy with no details.
    let health = rpc.health_check();
    assert_eq!(health["healthy"], Value::Bool(true));
}
