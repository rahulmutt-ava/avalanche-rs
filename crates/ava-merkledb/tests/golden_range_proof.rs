// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::range_proof` — a `RangeProof` byte-exact vs Go `x/merkledb`
//! (`proof.go` / `proto/sync`).
//!
//! Builds the basic-batch trie ({i}->{i} for i in [0,4]) at BranchFactor16,
//! produces a RangeProof for [{1}, {3}] (max 10), asserts the proto-encoded
//! `sync.RangeProof` bytes equal the committed deterministic Go vector, and that
//! verification accepts valid / rejects tampered.

use ava_merkledb::hashing::DefaultHasher;
use ava_merkledb::key::BranchFactor;
use ava_merkledb::proof::RangeProof;

use ava_types::id::Id;

#[derive(serde::Deserialize)]
struct Pair {
    key_hex: String,
    value_hex: String,
}

#[derive(serde::Deserialize)]
struct Vectors {
    root_hex: String,
    kvs: Vec<Pair>,
    start_hex: String,
    end_hex: String,
    max_length: usize,
    key_values: Vec<Pair>,
    proto_hex: String,
}

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("hex")
}

fn load() -> Vectors {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/merkledb/range-proofs/range_proof.json"
    ))
    .expect("read range proof vectors");
    serde_json::from_str(&raw).expect("parse range proof vectors")
}

#[test]
fn range_proof() {
    let v = load();
    let hasher = DefaultHasher;
    let bf = BranchFactor::Sixteen;

    let pairs: Vec<(Vec<u8>, Vec<u8>)> = v
        .kvs
        .iter()
        .map(|p| (hx(&p.key_hex), hx(&p.value_hex)))
        .collect();
    let kvs: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .map(|(k, val)| (k.as_slice(), val.as_slice()))
        .collect();

    let expected_root = Id::from_slice(&hx(&v.root_hex)).expect("root id");
    let start = hx(&v.start_hex);
    let end = hx(&v.end_hex);

    let proof = RangeProof::prove(bf, &hasher, &kvs, Some(&start), Some(&end), v.max_length)
        .expect("range proof");

    // Structural: key_values match the Go vector.
    let got_kvs: Vec<(Vec<u8>, Vec<u8>)> = proof
        .key_values
        .iter()
        .map(|kv| (kv.key.clone(), kv.value.clone()))
        .collect();
    let want_kvs: Vec<(Vec<u8>, Vec<u8>)> = v
        .key_values
        .iter()
        .map(|p| (hx(&p.key_hex), hx(&p.value_hex)))
        .collect();
    assert_eq!(got_kvs, want_kvs, "range key_values");

    // Proto bytes byte-exact vs the deterministic Go vector.
    assert_eq!(
        hex::encode(proof.encode_proto()),
        v.proto_hex,
        "range proof proto bytes"
    );

    // Valid proof verifies.
    proof
        .verify(Some(&start), Some(&end), expected_root, bf, &hasher)
        .expect("range proof verifies");

    // Tampered proof (corrupt a value) is rejected.
    let mut tampered = proof.clone();
    if let Some(kv) = tampered.key_values.first_mut() {
        kv.value = vec![0xff];
    }
    assert!(
        tampered
            .verify(Some(&start), Some(&end), expected_root, bf, &hasher)
            .is_err(),
        "tampered range proof rejected"
    );

    // Verifying against a wrong root is rejected.
    assert!(
        proof
            .verify(Some(&start), Some(&end), Id::EMPTY, bf, &hasher)
            .is_err(),
        "range proof vs wrong root rejected"
    );
}
