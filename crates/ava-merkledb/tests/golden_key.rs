// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::key_pack` — `Key`/`Path` byte-exact vs Go `x/merkledb/key.go`.
//!
//! Vectors extracted from the Go tree (see `tests/vectors/merkledb/keys/`).

use ava_merkledb::key::{BranchFactor, Key, longest_common_prefix};

#[derive(serde::Deserialize)]
struct ToTokenCase {
    branch_factor: String,
    token_size: usize,
    val: u8,
    value_hex: String,
    length: usize,
}

#[derive(serde::Deserialize)]
struct TokenProbe {
    bit_index: usize,
    token: u8,
}

#[derive(serde::Deserialize)]
struct KeyCase {
    name: String,
    token_size: usize,
    input_hex: String,
    bit_length: usize,
    value_hex: String,
    #[serde(default)]
    tokens: Option<Vec<TokenProbe>>,
    skip_bits: usize,
    skip_value_hex: String,
    skip_length: usize,
    take_bits: usize,
    take_value_hex: String,
    take_length: usize,
}

#[derive(serde::Deserialize)]
struct PrefixCase {
    name: String,
    token_size: usize,
    key_hex: String,
    key_len: usize,
    prefix_hex: String,
    prefix_len: usize,
    has_prefix: bool,
    iter_offset: usize,
    iter_has_prefix: bool,
}

#[derive(serde::Deserialize)]
struct LcpCase {
    name: String,
    token_size: usize,
    first_hex: String,
    second_hex: String,
    second_offset: usize,
    common_len: usize,
}

#[derive(serde::Deserialize)]
struct Vectors {
    to_token: Vec<ToTokenCase>,
    keys: Vec<KeyCase>,
    prefixes: Vec<PrefixCase>,
    longest_common_prefix: Vec<LcpCase>,
}

fn load() -> Vectors {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/merkledb/keys/key_pack.json"
    ))
    .expect("read key_pack.json");
    serde_json::from_str(&raw).expect("parse key_pack.json")
}

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("hex")
}

#[test]
fn key_pack() {
    let v = load();

    for c in &v.to_token {
        // sanity: token_size matches a branch factor.
        assert!(
            BranchFactor::from_token_size(c.token_size).is_some(),
            "{}: bad token size",
            c.branch_factor
        );
        let t = Key::to_token(c.val, c.token_size);
        assert_eq!(t.bytes(), hx(&c.value_hex).as_slice(), "to_token value");
        assert_eq!(t.length(), c.length, "to_token length");
    }

    for c in &v.keys {
        let input = hx(&c.input_hex);
        let k = Key::from_bytes(&input);
        assert_eq!(k.length(), c.bit_length, "{}: bit_length", c.name);
        assert_eq!(
            k.bytes(),
            hx(&c.value_hex).as_slice(),
            "{}: packed value",
            c.name
        );

        if let Some(probes) = &c.tokens {
            for p in probes {
                assert_eq!(
                    k.token(p.bit_index, c.token_size),
                    p.token,
                    "{}: token @ {}",
                    c.name,
                    p.bit_index
                );
            }
        }

        let s = k.skip(c.skip_bits);
        assert_eq!(s.length(), c.skip_length, "{}: skip length", c.name);
        assert_eq!(
            s.bytes(),
            hx(&c.skip_value_hex).as_slice(),
            "{}: skip value",
            c.name
        );

        let t = k.take(c.take_bits);
        assert_eq!(t.length(), c.take_length, "{}: take length", c.name);
        assert_eq!(
            t.bytes(),
            hx(&c.take_value_hex).as_slice(),
            "{}: take value",
            c.name
        );
    }

    for c in &v.prefixes {
        let key = Key::from_raw(hx(&c.key_hex).into(), c.key_len);
        let prefix = Key::from_raw(hx(&c.prefix_hex).into(), c.prefix_len);
        assert_eq!(
            key.has_prefix(&prefix),
            c.has_prefix,
            "{}: has_prefix",
            c.name
        );
        assert_eq!(
            key.iterated_has_prefix(&prefix, c.iter_offset, c.token_size),
            c.iter_has_prefix,
            "{}: iterated_has_prefix",
            c.name
        );
    }

    for c in &v.longest_common_prefix {
        let first = Key::from_bytes(&hx(&c.first_hex));
        let second = Key::from_bytes(&hx(&c.second_hex));
        assert_eq!(
            longest_common_prefix(&first, &second, c.second_offset, c.token_size),
            c.common_len,
            "{}: lcp",
            c.name
        );
    }
}
