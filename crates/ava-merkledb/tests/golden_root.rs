// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::merkledb_root` — trie root IDs byte-exact vs Go `x/merkledb`.
//!
//! Cases ascend in size: EMPTY trie (-> `ids::EMPTY`), single-key, then small
//! multi-key sets, across BranchFactor256/16/2. Vectors extracted from Go.

use ava_merkledb::hashing::{DefaultHasher, merkle_root};
use ava_merkledb::key::BranchFactor;

#[derive(serde::Deserialize)]
struct Pair {
    key_hex: String,
    value_hex: String,
}

#[derive(serde::Deserialize)]
struct RootCase {
    name: String,
    branch_factor: String,
    #[serde(default)]
    pairs: Option<Vec<Pair>>,
    root_hex: String,
}

#[derive(serde::Deserialize)]
struct Vectors {
    cases: Vec<RootCase>,
}

fn bf(name: &str) -> BranchFactor {
    match name {
        "BranchFactor2" => BranchFactor::Two,
        "BranchFactor4" => BranchFactor::Four,
        "BranchFactor16" => BranchFactor::Sixteen,
        "BranchFactor256" => BranchFactor::TwoFiftySix,
        other => panic!("unknown branch factor {other}"),
    }
}

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("hex")
}

#[test]
fn merkledb_root() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/merkledb/roots/merkledb_root.json"
    ))
    .expect("read root vectors");
    let v: Vectors = serde_json::from_str(&raw).expect("parse root vectors");

    let hasher = DefaultHasher;
    for c in &v.cases {
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = c
            .pairs
            .iter()
            .flatten()
            .map(|p| (hx(&p.key_hex), hx(&p.value_hex)))
            .collect();
        let kvs: Vec<(&[u8], &[u8])> = pairs
            .iter()
            .map(|(k, val)| (k.as_slice(), val.as_slice()))
            .collect();
        let root = merkle_root(bf(&c.branch_factor), &hasher, &kvs);
        assert_eq!(root.hex(), c.root_hex, "{}: root", c.name);
    }
}
