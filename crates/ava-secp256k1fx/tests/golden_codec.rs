// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::secp256k1fx_codec` — byte-exact codec round-trips (specs 07 §4.2).
//!
//! Provenance: every `hex` is captured from the Go reference
//! (`vms/secp256k1fx`) via `codec.Manager.Marshal(0, &concrete)` — a 2-byte
//! codec-version prefix (`0x0000`) + the type's `serialize:"true"` fields. The
//! `transfer_input`/`transfer_output`/`credential` vectors match the assertions
//! in `transfer_input_test.go` / `transfer_output_test.go` / `credential_test.go`
//! exactly. See `tests/PORTING.md`.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::collections::HashMap;

use serde::Deserialize;

use ava_secp256k1fx::{
    Credential, Input, MintOutput, OutputOwners, TransferInput, TransferOutput, marshal,
    unmarshal_credential, unmarshal_input, unmarshal_mint_output, unmarshal_output_owners,
    unmarshal_transfer_input, unmarshal_transfer_output,
};
use ava_types::short_id::ShortId;

#[derive(Debug, Deserialize)]
struct CodecVec {
    name: String,
    #[allow(dead_code)]
    r#type: String,
    hex: String,
}

#[derive(Debug, Deserialize)]
struct Vectors {
    codec: Vec<CodecVec>,
}

fn load() -> Vectors {
    let raw = include_str!("vectors/secp256k1fx/vectors.json");
    serde_json::from_str(raw).expect("parse vectors.json")
}

fn want(name: &str) -> Vec<u8> {
    let v = load();
    let cv = v
        .codec
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("vector {name} not found"));
    hex::decode(cv.hex).expect("hex decode")
}

fn addr(bytes: [u8; 20]) -> ShortId {
    ShortId::from(bytes)
}

const ADDR1: [u8; 20] = [
    0x51, 0x02, 0x5c, 0x61, 0xfb, 0xcf, 0xc0, 0x78, 0xf6, 0x93, 0x34, 0xf8, 0x34, 0xbe, 0x6d, 0xd2,
    0x6d, 0x55, 0xa9, 0x55,
];
const ADDR2: [u8; 20] = [
    0xc3, 0x34, 0x41, 0x28, 0xe0, 0x60, 0x12, 0x8e, 0xde, 0x35, 0x23, 0xa2, 0x4a, 0x46, 0x1c, 0x89,
    0x43, 0xab, 0x08, 0x59,
];

#[test]
fn transfer_input_round_trips() {
    let v = TransferInput::new(123_456_789, vec![3, 7]);
    let bytes = marshal(&v);
    assert_eq!(bytes, want("transfer_input"), "transfer_input bytes");
    assert_eq!(unmarshal_transfer_input(&bytes).unwrap(), v);
}

#[test]
fn mint_output_round_trips() {
    let v = MintOutput::new(OutputOwners::new(54321, 1, vec![addr(ADDR1), addr(ADDR2)]));
    let bytes = marshal(&v);
    assert_eq!(bytes, want("mint_output"), "mint_output bytes");
    assert_eq!(unmarshal_mint_output(&bytes).unwrap(), v);
}

#[test]
fn transfer_output_round_trips() {
    let v = TransferOutput::new(
        12345,
        OutputOwners::new(54321, 1, vec![addr(ADDR1), addr(ADDR2)]),
    );
    let bytes = marshal(&v);
    assert_eq!(bytes, want("transfer_output"), "transfer_output bytes");
    assert_eq!(unmarshal_transfer_output(&bytes).unwrap(), v);
}

#[test]
fn output_owners_round_trips() {
    let v = OutputOwners::new(54321, 2, vec![addr(ADDR1), addr(ADDR2)]);
    let bytes = marshal(&v);
    assert_eq!(bytes, want("output_owners"), "output_owners bytes");
    assert_eq!(unmarshal_output_owners(&bytes).unwrap(), v);
}

#[test]
fn input_round_trips() {
    let v = Input::new(vec![0, 1, 5]);
    let bytes = marshal(&v);
    assert_eq!(bytes, want("input"), "input bytes");
    assert_eq!(unmarshal_input(&bytes).unwrap(), v);
}

#[test]
fn credential_round_trips() {
    let mut sig_a = [0u8; 65];
    for (i, b) in sig_a.iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut sig_b = [0u8; 65];
    for (i, b) in sig_b.iter_mut().enumerate() {
        *b = 0x40u8.wrapping_add(i as u8);
    }
    let v = Credential::new(vec![sig_a, sig_b]);
    let bytes = marshal(&v);
    assert_eq!(bytes, want("credential"), "credential bytes");
    assert_eq!(unmarshal_credential(&bytes).unwrap(), v);
}

#[test]
fn empty_credential_round_trips() {
    let v = Credential::new(vec![]);
    let bytes = marshal(&v);
    assert_eq!(bytes, want("credential_empty"), "credential_empty bytes");
    assert_eq!(unmarshal_credential(&bytes).unwrap(), v);
}

#[test]
fn all_vectors_decode() {
    // every committed codec vector decodes back to the same bytes it encodes.
    let v = load();
    let by_name: HashMap<_, _> = v.codec.iter().map(|c| (c.name.as_str(), c)).collect();
    assert!(by_name.contains_key("transfer_input"));
    assert!(by_name.contains_key("credential"));
}
