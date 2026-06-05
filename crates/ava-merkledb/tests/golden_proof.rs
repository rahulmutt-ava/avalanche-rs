// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::merkledb_proof` — single inclusion/exclusion proofs byte-exact vs
//! Go `x/merkledb` (`proof.go`).
//!
//! Builds the basic-batch trie ({i}->{i} for i in [0,4]) at BranchFactor16,
//! generates an inclusion proof (key 0x02) and an exclusion proof (key 0x06),
//! asserts the proto-encoded proof path equals the committed deterministic Go
//! vector, and that `verify` accepts valid / rejects tampered proofs.

use ava_merkledb::hashing::DefaultHasher;
use ava_merkledb::key::{BranchFactor, Key};
use ava_merkledb::maybe::Maybe;
use ava_merkledb::proof::Proof;

use ava_types::id::Id;

#[derive(serde::Deserialize)]
struct Pair {
    key_hex: String,
    value_hex: String,
}

#[derive(serde::Deserialize)]
struct InclusionCase {
    key_hex: String,
    value_hex: String,
    path_proto_hex: String,
}

#[derive(serde::Deserialize)]
struct ExclusionCase {
    key_hex: String,
    path_proto_hex: String,
}

#[derive(serde::Deserialize)]
struct Vectors {
    root_hex: String,
    kvs: Vec<Pair>,
    inclusion: InclusionCase,
    exclusion: ExclusionCase,
}

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("hex")
}

fn load() -> Vectors {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/merkledb/proofs/merkledb_proof.json"
    ))
    .expect("read proof vectors");
    serde_json::from_str(&raw).expect("parse proof vectors")
}

#[test]
fn merkledb_proof() {
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

    // Inclusion proof for key 0x02.
    let incl_key = hx(&v.inclusion.key_hex);
    let incl = Proof::prove(bf, &hasher, &kvs, &incl_key).expect("inclusion proof");
    assert_eq!(
        incl.value(),
        Maybe::Some(bytes::Bytes::from(hx(&v.inclusion.value_hex))),
        "inclusion value"
    );
    // proto encoding of the path matches the deterministic Go vector.
    assert_eq!(
        hex::encode(incl.encode_path_proto()),
        v.inclusion.path_proto_hex,
        "inclusion proto bytes"
    );
    // Valid proof verifies.
    incl.verify(expected_root, bf, &hasher)
        .expect("inclusion verifies");

    // Tampered proof (corrupt the proven value) is rejected.
    let mut tampered = incl.clone();
    tampered.set_value(Maybe::Some(bytes::Bytes::from_static(b"\xff")));
    assert!(
        tampered.verify(expected_root, bf, &hasher).is_err(),
        "tampered inclusion rejected"
    );

    // Exclusion proof for key 0x06.
    let excl_key = hx(&v.exclusion.key_hex);
    let excl = Proof::prove(bf, &hasher, &kvs, &excl_key).expect("exclusion proof");
    assert!(excl.value().is_nothing(), "exclusion has no value");
    assert_eq!(
        hex::encode(excl.encode_path_proto()),
        v.exclusion.path_proto_hex,
        "exclusion proto bytes"
    );
    excl.verify(expected_root, bf, &hasher)
        .expect("exclusion verifies");

    // Exclusion proof against a wrong root is rejected.
    assert!(
        excl.verify(Id::EMPTY, bf, &hasher).is_err(),
        "exclusion vs wrong root rejected"
    );

    // Sanity: the proven key round-trips through Key.
    assert_eq!(incl.key(), &Key::from_bytes(&incl_key));
}
