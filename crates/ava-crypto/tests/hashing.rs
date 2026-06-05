// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden tests for `ava_crypto::hashing` (M0.13).
//!
//! Mirrors avalanchego `utils/hashing/hashing_test.go`. The vectors are
//! Go-generated `(pubkey, ripemd160(sha256(pubkey)), checksum4)` triples.

use ava_crypto::hashing::{
    ADDR_LEN, HASH_LEN, checksum, pubkey_bytes_to_address, ripemd160, sha256,
};

#[derive(serde::Deserialize)]
struct AddrCase {
    pubkey_hex: String,
    address_hex: String,
    checksum4_hex: String,
}

fn cases() -> Vec<AddrCase> {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/crypto/addr.json"
    ))
    .expect("read addr.json");
    serde_json::from_str(&raw).expect("parse addr.json")
}

#[test]
fn address_from_pubkey() {
    assert_eq!(HASH_LEN, 32);
    assert_eq!(ADDR_LEN, 20);
    for c in cases() {
        let pubkey = hex::decode(&c.pubkey_hex).expect("decode pubkey");
        let want_addr = hex::decode(&c.address_hex).expect("decode addr");
        let want_ck = hex::decode(&c.checksum4_hex).expect("decode ck");

        // pubkey_bytes_to_address == ripemd160(sha256(key))
        let addr = pubkey_bytes_to_address(&pubkey);
        assert_eq!(addr.as_slice(), want_addr.as_slice());
        assert_eq!(addr, ripemd160(&sha256(&pubkey)));

        // checksum(b, 4) == last 4 bytes of sha256(b)
        let ck = checksum(&pubkey, 4);
        assert_eq!(ck.as_slice(), want_ck.as_slice());
        assert_eq!(ck.as_slice(), &sha256(&pubkey)[HASH_LEN - 4..]);
    }
}

#[test]
fn keccak256_known_empty() {
    // keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
    let got = ava_crypto::hashing::keccak256(b"");
    assert_eq!(
        hex::encode(got),
        "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
    );
}
