// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden `golden::cchain_block_wire` (M6.7 exit-gate, spec 10 §9.3 / §6.2).
//!
//! Decodes Go-produced (coreth) C-Chain block bytes through
//! `ava_evm::block::decode_ava_evm_block`, asserts the recovered block **ID**
//! equals the coreth block hash (consensus-critical), and asserts
//! `assemble_ava_block(...)` re-encodes **byte-identically**. Covers one plain
//! block (no atomic txs) and one block carrying an atomic Import tx in `ExtData`.
//!
//! Vectors + provenance: `tests/vectors/cchain/block_wire/`.

use ava_evm::block::{AvaBlockParts, assemble_ava_block, decode_ava_evm_block};
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm_reth::{B256, Chain};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    plain_block: Vector,
    atomic_block: Vector,
}

#[derive(Deserialize)]
struct Vector {
    block_rlp: String,
    block_hash: String,
    number: u64,
    base_fee: Option<String>,
    ext_data_hash: String,
    ext_data_hex: String,
    num_txs: usize,
    num_atomic_txs: usize,
}

fn b256(hex_str: &str) -> B256 {
    let bytes = hex::decode(hex_str.trim_start_matches("0x")).expect("hex");
    B256::from_slice(&bytes)
}

fn check(v: &Vector, spec: &AvaChainSpec) {
    let raw = hex::decode(v.block_rlp.trim_start_matches("0x")).expect("block rlp hex");

    // Decode round-trips and recovers all parts.
    let block = decode_ava_evm_block(&raw, spec).expect("decode_ava_evm_block");

    // Consensus-critical: recovered block ID == coreth block hash.
    assert_eq!(block.hash(), b256(&v.block_hash), "block ID parity");
    assert_eq!(block.number(), v.number, "block number");

    // ExtDataHash + atomic-tx extraction.
    assert_eq!(
        block.header().ext_data_hash,
        b256(&v.ext_data_hash),
        "ext data hash"
    );
    assert_eq!(
        block.atomic_txs().len(),
        v.num_atomic_txs,
        "atomic tx count"
    );
    assert_eq!(block.transactions().len(), v.num_txs, "evm tx count");
    assert_eq!(
        hex::encode(block.ext_data()),
        v.ext_data_hex,
        "ext data bytes"
    );

    // base fee
    match &v.base_fee {
        Some(bf) => assert_eq!(
            block.header().base_fee.map(|f| f.to_string()),
            Some(bf.clone()),
            "base fee"
        ),
        None => assert_eq!(block.header().base_fee, None, "base fee absent"),
    }

    // Senders recoverable for every EVM tx.
    let recovered = block.recover_senders().expect("recover senders");
    assert_eq!(recovered.len(), v.num_txs, "recovered senders");

    // assemble_ava_block re-encodes byte-identically.
    let parts = AvaBlockParts {
        header: block.header().clone(),
        transactions: block.transactions().to_vec(),
        atomic_txs: block.atomic_txs().to_vec(),
        ext_data: block.ext_data().to_vec(),
        version: block.version(),
    };
    let reencoded = assemble_ava_block(parts, spec).expect("assemble_ava_block");
    assert_eq!(
        hex::encode(reencoded.encoded_bytes()),
        v.block_rlp.trim_start_matches("0x"),
        "re-encode byte identity"
    );
    // ...and the assembled block hashes identically.
    assert_eq!(reencoded.hash(), b256(&v.block_hash), "assembled block ID");
}

#[test]
fn cchain_block_wire() {
    let raw = include_str!("vectors/cchain/block_wire/block_wire.json");
    let vectors: Vectors = serde_json::from_str(raw).expect("parse vectors");

    // A spec with every Avalanche phase active from genesis (all activations at
    // t=0): the synthetic vectors carry timestamp 10, and the atomic_block uses
    // the AP5+ batch ExtData encoding, so AP5 must be active at decode. (The
    // wire *layout* is fork-independent; fork gating only selects the in-block
    // atomic-tx encoding — single pre-AP5 vs. batch AP5+ — §6.2.)
    let all_active = NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: 0,
        apricot_phase_5: 0,
        apricot_phase_pre_6: 0,
        apricot_phase_6: 0,
        apricot_phase_post_6: 0,
        banff: 0,
        cortina: 0,
        durango: 0,
        etna: 0,
        fortuna: 0,
        granite: 0,
        helicon: u64::MAX,
    };
    let spec = AvaChainSpec::from_parts(all_active, Chain::from_id(43114), false);

    check(&vectors.plain_block, &spec);
    check(&vectors.atomic_block, &spec);
}
