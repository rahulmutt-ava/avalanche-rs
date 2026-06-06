// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::firewood_ethhash_root` (M1.21, spec 04 §4.1 ethhash, 15 §6).
//!
//! Feeds a fixed batch of RLP-encoded accounts (at account depth) + storage
//! slots into a firewood instance in ethhash (Keccak-256 / Eth-MPT) mode and
//! asserts the resulting EVM state root equals the **REAL** Go-extracted vector
//! from `firewood-go-ethhash/ffi v0.5.0` (see
//! `tests/vectors/firewood/ethhash/_provenance.md`). Also asserts the empty
//! trie hashes to the well-known Ethereum empty-trie root
//! (`0x56e81f17…` == `types.EmptyRootHash`).

#![cfg(feature = "firewood-ethhash")]

use ava_merkledb::firewood::BatchOp;
use ava_merkledb::firewood::ethhash::EthHashDb;
use ava_types::id::Id;

const VECTOR_JSON: &str = include_str!("vectors/firewood/ethhash/accounts_root.json");

/// The well-known Ethereum empty-trie root (`types.EmptyRootHash`).
const EMPTY_ROOT_HEX: &str = "56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421";

#[derive(serde::Deserialize)]
struct Kv {
    key: String,
    value: String,
}

#[derive(serde::Deserialize)]
struct Vector {
    empty_root: String,
    batch: Vec<Kv>,
    root: String,
}

fn id_from_hex(s: &str) -> Id {
    let bytes = hex::decode(s).expect("hex decode");
    Id::from_slice(&bytes).expect("id from slice")
}

#[test]
fn firewood_ethhash_empty_root_is_eth_empty_trie() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = EthHashDb::open(dir.path()).expect("open ethhash db");

    let expected = id_from_hex(EMPTY_ROOT_HEX);
    assert_eq!(
        db.root(),
        expected,
        "fresh ethhash db must report the Ethereum empty-trie root"
    );
    assert_eq!(
        EthHashDb::empty_root(),
        expected,
        "EthHashDb::empty_root() must equal types.EmptyRootHash"
    );
}

#[test]
fn firewood_ethhash_root() {
    let vector: Vector = serde_json::from_str(VECTOR_JSON).expect("parse vector");

    // Sanity: the vector's empty-trie root is the well-known constant.
    assert_eq!(vector.empty_root, EMPTY_ROOT_HEX);

    let dir = tempfile::tempdir().expect("tempdir");
    let db = EthHashDb::open(dir.path()).expect("open ethhash db");

    let ops: Vec<BatchOp> = vector
        .batch
        .iter()
        .map(|kv| BatchOp::Put {
            key: hex::decode(&kv.key).expect("decode key"),
            value: hex::decode(&kv.value).expect("decode value"),
        })
        .collect();

    // The root is available pre-commit (consensus votes on it).
    let proposed_root = db.state_root(ops.clone()).expect("state root");
    let expected_root = id_from_hex(&vector.root);
    assert_eq!(
        proposed_root, expected_root,
        "ethhash state root must match the REAL Go-extracted firewood vector"
    );
    assert_ne!(
        proposed_root,
        EthHashDb::empty_root(),
        "populated state root must differ from the empty-trie root"
    );

    // Committing yields the same root and advances the tip.
    let committed_root = db.commit(ops).expect("commit");
    assert_eq!(committed_root, expected_root);
    assert_eq!(
        db.root(),
        expected_root,
        "tip advanced to the committed root"
    );
}
