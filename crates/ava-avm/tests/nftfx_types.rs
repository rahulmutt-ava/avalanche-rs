// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for `ava_avm::nftfx` types and codec (M5.3).

#![allow(unused_crate_dependencies)]

use ava_avm::nftfx::{
    Credential, MintOperation, MintOutput, TransferOperation, TransferOutput, marshal,
    unmarshal_transfer_output,
};
use ava_secp256k1fx::{Input, OutputOwners};
use ava_types::short_id::ShortId;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// TransferOutput round-trip
// ---------------------------------------------------------------------------

/// Round-trip `nftfx::TransferOutput` through `marshal`/`unmarshal_transfer_output`.
#[test]
fn transfer_output_round_trip() {
    let owners = OutputOwners::new(0, 1, vec![addr(ADDR1), addr(ADDR2)]);
    let original = TransferOutput {
        group_id: 42,
        payload: vec![0xde, 0xad, 0xbe, 0xef],
        owners,
    };
    let bytes = marshal(&original);
    let decoded = unmarshal_transfer_output(&bytes).expect("unmarshal_transfer_output failed");
    assert_eq!(decoded, original, "round-trip mismatch");
}

/// Empty payload round-trips correctly.
#[test]
fn transfer_output_empty_payload_round_trip() {
    let owners = OutputOwners::new(0, 0, vec![]);
    let original = TransferOutput {
        group_id: 0,
        payload: vec![],
        owners,
    };
    let bytes = marshal(&original);
    let decoded = unmarshal_transfer_output(&bytes).expect("unmarshal failed");
    assert_eq!(decoded, original);
}

// ---------------------------------------------------------------------------
// MintOperation::outs() synthesizes TransferOutputs
// ---------------------------------------------------------------------------

/// `MintOperation::outs()` produces one `TransferOutput` per owner entry,
/// all sharing the same `group_id` and `payload`.
#[test]
fn mint_operation_outs_synthesizes_transfer_outputs() {
    let mint_input = Input::new(vec![0]);
    let group_id: u32 = 7;
    let payload = vec![0x01, 0x02, 0x03];

    let owners1 = OutputOwners::new(0, 1, vec![addr(ADDR1)]);
    let owners2 = OutputOwners::new(0, 1, vec![addr(ADDR2)]);

    let op = MintOperation {
        mint_input,
        group_id,
        payload: payload.clone(),
        outputs: vec![owners1.clone(), owners2.clone()],
    };

    let outs = op.outs();
    assert_eq!(outs.len(), 2, "expected 2 outputs");

    let out0 = outs.first().expect("outs[0] missing");
    assert_eq!(out0.group_id, group_id);
    assert_eq!(out0.payload, payload);
    assert_eq!(out0.owners, owners1);

    let out1 = outs.get(1).expect("outs[1] missing");
    assert_eq!(out1.group_id, group_id);
    assert_eq!(out1.payload, payload);
    assert_eq!(out1.owners, owners2);
}

// ---------------------------------------------------------------------------
// TransferOperation::outs()
// ---------------------------------------------------------------------------

/// `TransferOperation::outs()` returns a single-element vec wrapping its output.
#[test]
fn transfer_operation_outs_wraps_output() {
    let owners = OutputOwners::new(0, 1, vec![addr(ADDR1)]);
    let output = TransferOutput {
        group_id: 3,
        payload: vec![0xff],
        owners,
    };
    let op = TransferOperation {
        input: Input::new(vec![0]),
        output: output.clone(),
    };

    let outs = op.outs();
    assert_eq!(outs.len(), 1);
    assert_eq!(outs.first().expect("outs[0] missing"), &output);
}

// ---------------------------------------------------------------------------
// MintOutput round-trip
// ---------------------------------------------------------------------------

#[test]
fn mint_output_round_trip() {
    use ava_avm::nftfx::unmarshal_mint_output;

    let owners = OutputOwners::new(0, 1, vec![addr(ADDR1)]);
    let original = MintOutput {
        group_id: 99,
        owners,
    };
    let bytes = ava_avm::nftfx::marshal(&original);
    let decoded = unmarshal_mint_output(&bytes).expect("unmarshal_mint_output failed");
    assert_eq!(decoded, original);
}

// ---------------------------------------------------------------------------
// Credential round-trip
// ---------------------------------------------------------------------------

#[test]
fn credential_round_trip() {
    use ava_avm::nftfx::unmarshal_credential;
    use ava_crypto::secp256k1::SIGNATURE_LEN;
    use ava_secp256k1fx::Credential as SecpCredential;

    let sig = [0xab; SIGNATURE_LEN];
    let inner = SecpCredential::new(vec![sig]);
    let original = Credential(inner);
    let bytes = ava_avm::nftfx::marshal(&original);
    let decoded = unmarshal_credential(&bytes).expect("unmarshal_credential failed");
    assert_eq!(decoded, original);
}

// ---------------------------------------------------------------------------
// verify() — payload size cap
// ---------------------------------------------------------------------------

#[test]
fn transfer_output_verify_payload_too_large() {
    use ava_vm::components::verify::Verifiable;

    let owners = OutputOwners::new(0, 0, vec![]);
    let output = TransferOutput {
        group_id: 0,
        payload: vec![0u8; 1025],
        owners,
    };
    assert!(
        output.verify().is_err(),
        "expected error for payload > 1 KiB"
    );
}

#[test]
fn transfer_output_verify_payload_at_limit_ok() {
    use ava_vm::components::verify::Verifiable;

    let owners = OutputOwners::new(0, 0, vec![]);
    let output = TransferOutput {
        group_id: 0,
        payload: vec![0u8; 1024],
        owners,
    };
    assert!(output.verify().is_ok(), "payload == 1024 should be valid");
}
