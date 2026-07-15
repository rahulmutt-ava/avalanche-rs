// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Live-captured Go block adoption (M9.15 rung 5).
//!
//! Drives the **live-captured** local-network C-Chain block 1 (the exact
//! 791-byte proposervm container a 5-validator Go network served the Rust
//! follower during the 2026-07-15 `mixed_network` run — see
//! `vectors/cchain/block_wire/live_local_block1.json` / `_provenance.md`)
//! through the follower's `EvmVm` stack: `from_genesis` over the local genesis,
//! `parse_block` of the inner coreth block, `verify` (execute to the header
//! state root), and `accept` (advance the canonical tip).
//!
//! Regression: `EvmBlock::eth_env_header` used to drop the Cancun tail
//! (`parentBeaconRoot`/blob fields) when building the execution env, so
//! alloy-evm's EIP-4788 system call rejected every Cancun-active Go block with
//! `MissingParentBeaconBlockRoot` — the follower could never adopt the live
//! network's blocks (its C height stayed 0 while chits named block 1).

use ava_evm::vm::EvmVm;
use ava_types::constants::LOCAL_ID;
use ava_vm::block::ChainVm;
use tokio_util::sync::CancellationToken;

/// Extracts the inner coreth block bytes from a proposervm **unsigned
/// post-fork** container: `codecVersion(2) || typeID(4) || parentID(32) ||
/// timestamp(8) || pChainHeight(8) || certLen(4)=0 || blockLen(4) || block ||
/// signatureLen(4)=0`.
fn inner_block_of(container: &[u8]) -> &[u8] {
    let cert_len = u32::from_be_bytes(container[54..58].try_into().expect("cert len"));
    assert_eq!(cert_len, 0, "unsigned post-fork block carries no cert");
    let block_len = u32::from_be_bytes(container[58..62].try_into().expect("block len")) as usize;
    &container[62..62 + block_len]
}

#[tokio::test]
async fn follower_adopts_live_local_block1() {
    let vector: serde_json::Value = serde_json::from_str(include_str!(
        "vectors/cchain/block_wire/live_local_block1.json"
    ))
    .expect("live_local_block1.json parses");
    let container = hex::decode(vector["container_hex"].as_str().expect("container_hex"))
        .expect("container hex decodes");
    assert_eq!(container.len(), 791, "captured container length");
    let inner = inner_block_of(&container);
    assert_eq!(inner.len(), 725, "inner coreth block length");

    // The follower's local C-Chain genesis (byte-identical to the embedded
    // cChainGenesis the live network runs on; genesis parity pinned by
    // `genesis_root::cchain_genesis_root`).
    let genesis_json = include_str!("vectors/cchain/genesis/local.json");
    let dir = tempfile::tempdir().expect("tempdir");
    let (vm, genesis_id) = EvmVm::from_genesis(LOCAL_ID, dir.path(), genesis_json.as_bytes())
        .expect("EvmVm::from_genesis over the local genesis");

    let token = CancellationToken::new();
    let blk = vm
        .parse_block(&token, inner)
        .await
        .expect("parse_block(live inner block 1)");
    assert_eq!(blk.height(), 1, "live block 1 height");
    assert_eq!(
        blk.parent(),
        genesis_id,
        "live block 1 parents at the local genesis (genesis parity)"
    );

    // The load-bearing assert: execute the Cancun-active live block to its
    // header state root. RED (pre-fix): "EIP-4788 parent beacon block root
    // missing for active Cancun block".
    blk.verify(&token)
        .await
        .expect("verify(live block 1) executes to the header state root");

    blk.accept(&token).await.expect("accept(live block 1)");
    let last = vm.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(last, blk.id(), "accept advanced the tip to live block 1");

    let tip = vm.get_block(&token, last).await.expect("get_block(tip)");
    assert_eq!(tip.height(), 1, "the accepted tip is height 1");
}
