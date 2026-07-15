// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 rung 5 — the C-Chain follower adopts a **live-captured Go block**
//! through the full engine path.
//!
//! Boots the follower's real C-Chain stack (`EvmVm::from_genesis` over the
//! local genesis → proposervm → `SnowmanEngine`, via `boot_chain_with_loopback`)
//! and delivers the exact 791-byte proposervm container a 5-validator Go
//! avalanchego local network served the Rust follower during the 2026-07-15
//! live `mixed_network` run (oracle `96897293a2`; vector + provenance:
//! `crates/ava-evm/tests/vectors/cchain/block_wire/live_local_block1.json`)
//! as an unsolicited `Put` — the same inbound op the live follower processes.
//!
//! The engine must parse the container (post-fork proposervm block wrapping the
//! inner coreth block), issue it (its parent IS the local genesis — genesis
//! parity), verify it (execute to the header state root; the RED failure was
//! `eth_env_header` dropping the Cancun tail ⇒ "EIP-4788 parent beacon block
//! root missing" / "excess_blob_gas not set"), and — through the self-loopback
//! closing the k=1/β=1 poll — ACCEPT it, advancing `eth_blockNumber` to `0x1`
//! exactly as the live harness polls it.

use std::sync::Arc;

use ava_database::{DynDatabase, MemDb};
use ava_engine::networking::router::{ChainMessageSink, InboundOp};
use ava_snow::EngineState;
use ava_types::constants::LOCAL_ID;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::vm::VmRequest;
use avalanchers::wiring::chains::boot_chain_with_loopback;
use tokio_util::sync::CancellationToken;

/// Lowercase-hex decode (avoids a `hex` dev-dependency).
fn unhex(s: &str) -> Vec<u8> {
    let s = s.trim();
    assert!(s.len().is_multiple_of(2), "even-length hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn c_chain_follower_adopts_live_go_block() {
    // The live-captured container + the follower's local C genesis (both shared
    // with the ava-evm unit regression `live_block_adopt.rs`).
    let vector: serde_json::Value = serde_json::from_str(include_str!(
        "../../ava-evm/tests/vectors/cchain/block_wire/live_local_block1.json"
    ))
    .expect("live_local_block1.json parses");
    let container = unhex(vector["container_hex"].as_str().expect("container_hex"));
    assert_eq!(container.len(), 791, "captured container length");
    let genesis_json = include_str!("../../ava-evm/tests/vectors/cchain/genesis/local.json");

    // The follower's C-Chain VM, exactly as `run_queued_chains*` builds it. The
    // Firewood scratch dir must outlive the running VM.
    let data_dir = tempfile::tempdir().expect("tempdir");
    let (vm, genesis_id) =
        ava_evm::vm::EvmVm::from_genesis(LOCAL_ID, data_dir.path(), genesis_json.as_bytes())
            .expect("EvmVm::from_genesis over the local genesis");

    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let handle = boot_chain_with_loopback(
        LOCAL_ID,
        Id::from([7u8; 32]), // chain id — synthetic, as the other boot tests use
        ava_types::constants::PRIMARY_NETWORK_ID,
        "C",
        Id::EMPTY, // the C-Chain EVM genesis carries no AVAX asset id
        genesis_id,
        vm,
        genesis_json.as_bytes().to_vec(),
        Arc::clone(&base),
    )
    .await
    .expect("boot the C chain with the self-loopback installed");

    // Solo node (empty beacons) short-circuits Bootstrapping → NormalOp.
    let mut reached = false;
    for _ in 0..200_000 {
        if matches!(**handle.ctx.state.load(), EngineState::NormalOp) {
            reached = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(reached, "the solo C chain reached NormalOp");

    // The RPC-visibility surface the live harness polls: eth_blockNumber on the
    // VM's "/rpc" handler.
    let token = CancellationToken::new();
    let rpc = {
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
    };
    let block_number = |rpc: Arc<dyn ava_vm::vm::VmHttpService>| async move {
        let resp = rpc
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: "/rpc".to_string(),
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: br#"{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}"#
                    .to_vec(),
            })
            .await;
        let v: serde_json::Value = serde_json::from_slice(&resp.body).expect("json-rpc reply");
        v["result"].as_str().map(str::to_owned)
    };

    assert_eq!(
        block_number(Arc::clone(&rpc)).await.as_deref(),
        Some("0x0"),
        "eth_blockNumber starts at genesis"
    );

    // Deliver the live Go network's block 1 exactly as the wire does: an
    // unsolicited `Put`. The engine parses + issues + verifies it; the loopback
    // closes the k=1/β=1 poll (push_query → self-chits) and consensus ACCEPTS.
    handle
        ._sink
        .push(
            NodeId::from([9u8; 20]),
            InboundOp::Put {
                request_id: u32::MAX,
                container,
            },
        )
        .await;

    // eth_blockNumber must reach 0x1 — the exact observable the live
    // mixed_network harness polls (`await_same_c_height`).
    let mut adopted = false;
    for _ in 0..20_000 {
        if block_number(Arc::clone(&rpc)).await.as_deref() == Some("0x1") {
            adopted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert!(
        adopted,
        "the follower engine adopted the live Go block: put → issue → verify → \
         poll → accept → eth_blockNumber 0x1 (still {:?})",
        block_number(rpc).await
    );

    handle.token.cancel();
    handle.join.await.expect("handler task joined cleanly");
}
