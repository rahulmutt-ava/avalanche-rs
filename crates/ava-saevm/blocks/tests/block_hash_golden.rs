// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Byte-exact SAE-block hashing golden test (specs/11 §4.1).
//!
//! A SAE block **is** a standard Ethereum block: the wire encoding is RLP of
//! the eth block and the block hash is `keccak256(RLP(header))`. This crate
//! uses the stock reth/alloy `SealedBlock`, whose `.hash()` computes exactly
//! that. The `Root` header field is merely *reinterpreted* under SAE as the
//! settled ancestor's post-exec state root — same layout, no encoding change.
//!
//! This test freezes a self-consistent golden: it constructs a fixed eth block,
//! computes its hash via the standard `SealedBlock::seal_slow` path, and asserts
//! the result equals a committed JSON vector. Exact Go/geth byte parity is a
//! later differential-test concern (M7.29) — this guards that our reth-based
//! hashing path is stable and that nobody silently changes the header layout.

use std::path::PathBuf;

use ava_evm_reth::{B256, Header, RethBlock, SealedBlock, U256, keccak256, rlp_encode};

/// Builds the fixed reference Ethereum block used by the golden vector.
///
/// Field values are arbitrary but fixed; `Root` carries the SAE-reinterpreted
/// settled-ancestor state root (layout-identical to a stock header `Root`).
fn reference_block() -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash: B256::repeat_byte(0x11),
        ommers_hash: ava_evm_reth::EMPTY_OMMER_ROOT_HASH,
        beneficiary: ava_evm_reth::Address::repeat_byte(0x22),
        // SAE: settled ancestor's post-exec state root (same layout as `Root`).
        state_root: B256::repeat_byte(0x33),
        transactions_root: ava_evm_reth::EMPTY_ROOT_HASH,
        receipts_root: B256::repeat_byte(0x44),
        number: 1_234,
        gas_limit: 30_000_000,
        gas_used: 21_000,
        timestamp: 1_700_000_000,
        base_fee_per_gas: Some(1_000_000_000),
        difficulty: U256::ZERO,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

fn vector_path() -> PathBuf {
    // crates/ava-saevm/blocks/ -> repo root is 4 levels up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/vectors/saevm/blocks/block_hash.json")
}

#[test]
fn sae_block_rlp_keccak_matches_geth() {
    let block = reference_block();

    // The standard path: keccak256(RLP(header)).
    let want_hash = block.hash();

    // Independently recompute keccak256(RLP(header)) to prove `.hash()` is the
    // standard eth-header hash (not some reth-private scheme).
    let header_rlp = rlp_encode(block.header());
    let recomputed = keccak256(&header_rlp);
    assert_eq!(
        recomputed, want_hash,
        "SealedBlock::hash() must equal keccak256(RLP(header))"
    );

    // Load and compare against the committed golden vector.
    let raw = std::fs::read_to_string(vector_path()).expect("read golden vector");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("parse golden json");

    let want_hash_hex = v["block_hash"].as_str().expect("block_hash field");
    let want_header_rlp_hex = v["header_rlp"].as_str().expect("header_rlp field");

    let got_hash_hex = format!("0x{}", hex::encode(want_hash.as_slice()));
    let got_header_rlp_hex = format!("0x{}", hex::encode(&header_rlp));

    assert_eq!(
        got_hash_hex, want_hash_hex,
        "block hash drifted from committed golden vector"
    );
    assert_eq!(
        got_header_rlp_hex, want_header_rlp_hex,
        "header RLP drifted from committed golden vector"
    );
}
