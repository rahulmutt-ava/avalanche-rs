// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use assert_matches::assert_matches;
use ava_utils::cb58::{cb58_decode, cb58_encode};
use ava_utils::error::Error;

#[derive(serde::Deserialize)]
struct Pair {
    bytes_hex: String,
    cb58: String,
}

#[test]
fn cb58_roundtrip() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/ids/cb58_raw.json"
    ))
    .unwrap();
    let pairs: Vec<Pair> = serde_json::from_str(&raw).unwrap();
    assert!(!pairs.is_empty());
    for p in &pairs {
        let bytes = hex::decode(&p.bytes_hex).unwrap();
        assert_eq!(
            cb58_encode(&bytes).unwrap(),
            p.cb58,
            "encode {}",
            p.bytes_hex
        );
        assert_eq!(cb58_decode(&p.cb58).unwrap(), bytes, "decode {}", p.cb58);
    }
}

#[test]
fn cb58_bad_checksum() {
    // valid "deadbeef" cb58 is "eFGDJT5xfjY"; mutate the last char.
    let mut s = cb58_encode(&hex::decode("deadbeef").unwrap()).unwrap();
    s.pop();
    s.push('Z');
    assert_matches!(
        cb58_decode(&s),
        Err(Error::BadChecksum) | Err(Error::Base58Decoding(_))
    );
}

#[test]
fn cb58_too_short() {
    // "1" decodes to a single zero byte (< 4-byte checksum).
    assert_matches!(cb58_decode("1"), Err(Error::MissingChecksum));
}
