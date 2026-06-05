// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M0.6 — Golden CB58 string-form round-trip tests for `Id`, `ShortId`, `NodeId`.
//!
//! Owning spec: `specs/03-core-primitives.md` §1.1, §3.2, §3.6.
//! Vectors from: `tests/vectors/ids/cb58.json`.

use std::str::FromStr;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;

/// Reads the golden vector file and asserts round-trip for every entry.
///
/// For each vector:
///  - `from_str(string)` succeeds and produces the right bytes.
///  - `to_string()` round-trips back to the exact same string.
///  - `NodeId` entries require the `NodeID-` prefix; bare CB58 is rejected.
#[test]
fn id_nodeid_cb58_strings() {
    // Load the golden vectors.
    let json = include_str!("../../../tests/vectors/ids/cb58.json");
    let entries: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();

    for entry in &entries {
        let kind = entry["kind"].as_str().unwrap();
        let bytes_hex = entry["bytes_hex"].as_str().unwrap();
        let string = entry["string"].as_str().unwrap();
        let raw = hex::decode(bytes_hex).unwrap();

        match kind {
            "id" => {
                let id = Id::from_str(string).unwrap();
                assert_eq!(
                    id.as_bytes().as_slice(),
                    raw.as_slice(),
                    "id bytes mismatch for {string}"
                );
                assert_eq!(id.to_string(), string, "id round-trip failed for {string}");
            }
            "short_id" => {
                let sid = ShortId::from_str(string).unwrap();
                assert_eq!(
                    sid.as_bytes().as_slice(),
                    raw.as_slice(),
                    "short_id bytes mismatch for {string}"
                );
                assert_eq!(
                    sid.to_string(),
                    string,
                    "short_id round-trip failed for {string}"
                );
            }
            "node_id" => {
                let nid = NodeId::from_str(string).unwrap();
                assert_eq!(
                    nid.as_bytes().as_slice(),
                    raw.as_slice(),
                    "node_id bytes mismatch for {string}"
                );
                assert_eq!(
                    nid.to_string(),
                    string,
                    "node_id round-trip failed for {string}"
                );

                // Bare CB58 (without NodeID- prefix) must be rejected.
                let bare = string.strip_prefix("NodeID-").unwrap();
                let err = NodeId::from_str(bare);
                assert!(
                    err.is_err(),
                    "expected error for bare CB58 '{bare}' but got Ok"
                );
            }
            other => panic!("unknown kind: {other}"),
        }
    }
}

/// JSON null deserializes to `Default` for `Id` (Go null no-op behavior, spec §1.1).
#[test]
fn id_json_null_is_default() {
    let id: Id = serde_json::from_str("null").unwrap();
    assert_eq!(id, Id::default());
}

/// JSON null deserializes to `Default` for `ShortId`.
#[test]
fn short_id_json_null_is_default() {
    let sid: ShortId = serde_json::from_str("null").unwrap();
    assert_eq!(sid, ShortId::default());
}

/// JSON null deserializes to `Default` for `NodeId`.
#[test]
fn node_id_json_null_is_default() {
    let nid: NodeId = serde_json::from_str("null").unwrap();
    assert_eq!(nid, NodeId::default());
}

/// `Id` serializes to its CB58 string and deserializes back.
#[test]
fn id_json_string_round_trip() {
    let id = Id::from_str("11111111111111111111111111111111LpoYY").unwrap();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""11111111111111111111111111111111LpoYY""#);
    let id2: Id = serde_json::from_str(&json).unwrap();
    assert_eq!(id, id2);
}

/// `NodeId` serializes to its `NodeID-`-prefixed CB58 string and deserializes back.
#[test]
fn node_id_json_string_round_trip() {
    let nid = NodeId::from_str("NodeID-111111111111111111116DBWJs").unwrap();
    let json = serde_json::to_string(&nid).unwrap();
    assert_eq!(json, r#""NodeID-111111111111111111116DBWJs""#);
    let nid2: NodeId = serde_json::from_str(&json).unwrap();
    assert_eq!(nid, nid2);
}
