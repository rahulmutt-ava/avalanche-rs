// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::cchain_genesis_root` — the M6.8 milestone exit-gate test (spec 10
//! §11.1 / §9.3 / §8.3 / §5, 02 §6).
//!
//! Parse the embedded Mainnet and Fuji C-Chain genesis JSON, materialize the
//! `alloc` into a fresh Firewood-ethhash db via the 5-field `rlp_account` path
//! (M6.30), and assert BOTH the computed genesis **state root** AND the genesis
//! **block ID** (`keccak256(RLP(header))` over the coreth header layout) equal
//! the Go-authoritative values for BOTH networks. The genesis state root + block
//! ID prove the genesis parse + alloc materialization + coreth header layout all
//! agree with coreth byte-for-byte (full parity is achievable: genesis is pure
//! alloc, no fee model — see `tests/vectors/cchain/genesis/_provenance.md`).

use std::str::FromStr;
use std::sync::Arc;

use ava_database::MemDb;
use ava_evm::chainspec::CChainGenesis;
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::B256;

#[derive(serde::Deserialize)]
struct Expected {
    chain_id: u64,
    genesis_state_root: String,
    genesis_block_id: String,
}

fn b256(s: &str) -> B256 {
    B256::from_str(s).expect("b256")
}

/// Materializes the genesis `alloc` into a fresh Firewood-ethhash db and returns
/// the committed genesis state root.
fn materialize_genesis_root(genesis: &CChainGenesis) -> B256 {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");

    // Seed contract bytecode into the side store (the state root only commits the
    // code_hash; the bytecode lives in the side KV — spec 10 §5.1).
    for (code_hash, code) in genesis.bytecode() {
        provider
            .bytecode_store()
            .put(code_hash.as_slice(), code)
            .expect("seed bytecode");
    }

    // Commit the alloc through the provider's propose -> stash -> commit
    // lifecycle (the same path accept() uses).
    let root = provider
        .propose_from_bundle(genesis.alloc_bundle())
        .expect("propose genesis alloc");
    provider.commit(root).expect("commit genesis alloc");
    provider.root()
}

/// Asserts genesis state-root + block-ID parity for one network.
fn assert_genesis_parity(net: &str, genesis_json: &str, expected: &Expected) {
    let genesis = CChainGenesis::parse(genesis_json).expect("parse genesis");
    assert_eq!(genesis.chain_id(), expected.chain_id, "{net} chain id");

    let state_root = materialize_genesis_root(&genesis);
    assert_eq!(
        state_root,
        b256(&expected.genesis_state_root),
        "{net} genesis state root parity vs coreth"
    );

    let header = genesis.genesis_header(state_root);
    assert_eq!(
        header.hash(),
        b256(&expected.genesis_block_id),
        "{net} genesis block ID parity vs coreth"
    );
}

#[test]
fn cchain_genesis_root() {
    let expected_raw = include_str!("vectors/cchain/genesis/expected.json");
    let expected: serde_json::Value =
        serde_json::from_str(expected_raw).expect("parse expected vectors");

    for net in ["mainnet", "fuji"] {
        let genesis_json = match net {
            "mainnet" => include_str!("vectors/cchain/genesis/mainnet.json"),
            "fuji" => include_str!("vectors/cchain/genesis/fuji.json"),
            _ => unreachable!(),
        };
        let exp: Expected =
            serde_json::from_value(expected[net].clone()).expect("expected for network");
        assert_genesis_parity(net, genesis_json, &exp);
    }
}
