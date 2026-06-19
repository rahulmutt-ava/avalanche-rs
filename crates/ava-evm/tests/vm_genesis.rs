// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `EvmVm::from_genesis` — the C-Chain dispatch genesis-wiring seam (M6.8
//! completion / M9.15 C-Chain dispatch).
//!
//! Builds a fully-initialized [`EvmVm`] straight from the production C-Chain
//! genesis JSON (parse → materialize alloc → commit → header), and asserts its
//! committed state root and last-accepted (genesis) id match the Go-authoritative
//! coreth values — the same oracle as `golden::cchain_genesis_root`, now driven
//! through the construction seam the C-Chain boot path (`run_queued_chains`) uses
//! so a solo live node can flip `is_bootstrapped(C)`.

use std::str::FromStr;

use ava_evm::vm::EvmVm;
use ava_evm_reth::B256;
use ava_types::id::Id;
use ava_vm::block::ChainVm;
use tokio_util::sync::CancellationToken;

/// The Go-authoritative C-Chain genesis state root + block ID (coreth
/// `core.Genesis.ToBlock()`), shared by Mainnet and Fuji
/// (`tests/vectors/cchain/genesis/expected.json`).
const GENESIS_STATE_ROOT: &str =
    "0xd65eb1b8604a7aa497d41cd6372663785a5f809a17bd192edb86658ef24e29cc";
const GENESIS_BLOCK_ID: &str = "0x31ced5b9beb7f8782b014660da0cb18cc409f121f408186886e1ca3e8eeca96b";

#[tokio::test]
async fn from_genesis_builds_vm_at_coreth_genesis_root() {
    let genesis_json = include_str!("vectors/cchain/genesis/mainnet.json");
    let expected_state_root = B256::from_str(GENESIS_STATE_ROOT).expect("state root b256");
    let expected_block_id = B256::from_str(GENESIS_BLOCK_ID).expect("block id b256");
    let want_id = Id::from(<[u8; 32]>::from(expected_block_id));

    let dir = tempfile::tempdir().expect("tempdir");
    // network_id = 1 (mainnet); the C-Chain genesis carries chainId 43114.
    let (vm, genesis_id) =
        EvmVm::from_genesis(1, dir.path(), genesis_json.as_bytes()).expect("EvmVm::from_genesis");

    assert_eq!(
        vm.state_root(),
        expected_state_root,
        "from_genesis commits the coreth genesis state root"
    );
    assert_eq!(
        genesis_id, want_id,
        "from_genesis returns the genesis id (keccak(genesis header))"
    );

    let token = CancellationToken::new();
    let last = vm
        .last_accepted(&token)
        .await
        .expect("last_accepted after from_genesis");
    assert_eq!(
        last, want_id,
        "last_accepted seeds the freshly-materialized genesis tip"
    );

    // The engine's bootstrap fetches `get_block(last_accepted)` and reads its
    // height (ava-engine `snowman::bootstrap::start`); the genesis tip must
    // resolve to a height-0 block or the C-Chain stalls before NormalOp.
    let genesis_block = vm
        .get_block(&token, last)
        .await
        .expect("get_block resolves the genesis tip");
    assert_eq!(genesis_block.id(), want_id, "genesis block id round-trips");
    assert_eq!(genesis_block.height(), 0, "genesis block is height 0");
}
