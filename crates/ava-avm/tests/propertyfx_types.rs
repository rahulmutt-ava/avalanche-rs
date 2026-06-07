// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Round-trip and structural tests for `ava_avm::propertyfx` types (M5.4).
//!
//! Each type is exercised through the `propertyfx::{marshal, unmarshal_*}`
//! helpers (byte-exact encoding matching Go's `codec.Manager.Marshal(0, …)`),
//! and the `outs()` helpers are asserted for the operation types.

#![allow(unused_crate_dependencies)]

use ava_avm::propertyfx::{
    BurnOperation, Credential, MintOperation, MintOutput, OwnedOutput, PropertyOutput, marshal,
    unmarshal_burn_operation, unmarshal_credential, unmarshal_mint_operation,
    unmarshal_mint_output, unmarshal_owned_output,
};
use ava_secp256k1fx::{Input as SecpInput, OutputOwners};
use ava_types::short_id::ShortId;

// ---------------------------------------------------------------------------
// helpers
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

fn sample_owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![addr(ADDR1), addr(ADDR2)])
}

// ---------------------------------------------------------------------------
// MintOutput round-trip
// ---------------------------------------------------------------------------

#[test]
fn mint_output_round_trips() {
    let owners = sample_owners();
    let v = MintOutput::new(owners.clone());

    let bytes = marshal(&v);
    let decoded = unmarshal_mint_output(&bytes).expect("unmarshal MintOutput");
    assert_eq!(decoded, v);
    // Confirm owners are preserved.
    assert_eq!(decoded.owners, owners);
}

// ---------------------------------------------------------------------------
// OwnedOutput round-trip
// ---------------------------------------------------------------------------

#[test]
fn owned_output_round_trips() {
    let owners = sample_owners();
    let v = OwnedOutput::new(owners.clone());

    let bytes = marshal(&v);
    let decoded = unmarshal_owned_output(&bytes).expect("unmarshal OwnedOutput");
    assert_eq!(decoded, v);
    assert_eq!(decoded.owners, owners);
}

// ---------------------------------------------------------------------------
// MintOutput and OwnedOutput are structurally identical but distinct types
// ---------------------------------------------------------------------------

#[test]
fn mint_output_and_owned_output_wire_bytes_match() {
    let owners = sample_owners();
    let mo = MintOutput::new(owners.clone());
    let oo = OwnedOutput::new(owners);
    // Both have identical { owners } layout → identical wire bytes.
    assert_eq!(marshal(&mo), marshal(&oo));
}

// ---------------------------------------------------------------------------
// MintOperation round-trip + outs()
// ---------------------------------------------------------------------------

#[test]
fn mint_operation_round_trips_and_outs() {
    let mint_input = SecpInput::new(vec![0, 1]);
    let mint_output = MintOutput::new(sample_owners());
    let owned_output = OwnedOutput::new(OutputOwners::new(1000, 1, vec![addr(ADDR1)]));

    let op = MintOperation::new(mint_input, mint_output.clone(), owned_output.clone());

    // Verify outs() returns [PropertyOutput::Mint(mint_output), PropertyOutput::Owned(owned_output)].
    let outs = op.outs();
    assert_eq!(outs.len(), 2);
    assert_eq!(
        outs.first(),
        Some(&PropertyOutput::Mint(mint_output.clone()))
    );
    assert_eq!(
        outs.get(1),
        Some(&PropertyOutput::Owned(owned_output.clone()))
    );

    // Round-trip through codec.
    let bytes = marshal(&op);
    let decoded = unmarshal_mint_operation(&bytes).expect("unmarshal MintOperation");
    assert_eq!(decoded, op);
    assert_eq!(decoded.outs().len(), 2);
}

// ---------------------------------------------------------------------------
// BurnOperation round-trip + outs()
// ---------------------------------------------------------------------------

#[test]
fn burn_operation_round_trips_and_outs_empty() {
    let input = SecpInput::new(vec![0]);
    let op = BurnOperation::new(input);

    // outs() must return empty slice.
    assert_eq!(op.outs().len(), 0);

    // Round-trip.
    let bytes = marshal(&op);
    let decoded = unmarshal_burn_operation(&bytes).expect("unmarshal BurnOperation");
    assert_eq!(decoded, op);
    assert_eq!(decoded.outs().len(), 0);
}

// ---------------------------------------------------------------------------
// Credential round-trip
// ---------------------------------------------------------------------------

#[test]
fn credential_round_trips() {
    use ava_crypto::secp256k1::SIGNATURE_LEN;
    use ava_secp256k1fx::Credential as SecpCredential;

    let sig = [0xabu8; SIGNATURE_LEN];
    let inner = SecpCredential::new(vec![sig]);
    let cred = Credential::new(inner);

    let bytes = marshal(&cred);
    let decoded = unmarshal_credential(&bytes).expect("unmarshal Credential");
    assert_eq!(decoded, cred);
}
