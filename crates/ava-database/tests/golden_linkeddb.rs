// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! linkeddb node-codec golden (04 §2.7, §10.6).
//!
//! The bytes physically written inside a linkeddb are linearcodec-encoded
//! `node{ value, has_next, next, has_previous, previous }` records (plus a head
//! pointer at key `0x01`), so they must reproduce avalanchego's
//! `database/linkeddb` byte-for-byte for a migrated list to iterate identically.
//! Asserts `encode_node`/`decode_node` round-trip the committed Go vector
//! (`tests/vectors/linkeddb/node_codec.json`), that the head key is `0x01`, and
//! that the per-key node key is `0x00 ‖ key`.
//!
//! The `unused_crate_dependencies` allow is unconditional (a known
//! false-positive of that lint for integration-test binaries).

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    unused_crate_dependencies
)]

use ava_database::linkeddb::{HEAD_KEY, Node, decode_node, encode_node, node_key};

/// The committed Go vector.
const VECTOR: &str = include_str!("vectors/linkeddb/node_codec.json");

mod golden {
    use super::*;

    /// Every node case encodes to (and decodes from) the Go bytes exactly, and
    /// the head/node key layout matches.
    #[test]
    fn linkeddb_node_codec() {
        let v: serde_json::Value = serde_json::from_str(VECTOR).unwrap();

        // head_key = 0x01.
        assert_eq!(hex::encode(HEAD_KEY), v["head_key"].as_str().unwrap());

        // node_key("logical") = 0x00 ‖ "logical".
        let nk = node_key(b"logical");
        assert_eq!(
            hex::encode(&nk),
            v["nodes"][0]["node_key_for_logical"].as_str().unwrap()
        );

        for case in v["nodes"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let node = Node {
                value: hex::decode(case["value"].as_str().unwrap()).unwrap(),
                has_next: case["has_next"].as_bool().unwrap(),
                next: hex::decode(case["next"].as_str().unwrap()).unwrap(),
                has_previous: case["has_previous"].as_bool().unwrap(),
                previous: hex::decode(case["previous"].as_str().unwrap()).unwrap(),
            };
            let want = case["encoded"].as_str().unwrap();
            let got = encode_node(&node).unwrap();
            assert_eq!(hex::encode(&got), want, "encode mismatch for {name}");

            // Decode round-trips back to the node, including the version prefix.
            let decoded = decode_node(&got).unwrap();
            assert_eq!(decoded, node, "decode mismatch for {name}");
        }
    }
}
