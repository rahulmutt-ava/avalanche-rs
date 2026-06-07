// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Consolidated byte-exact codec golden harness (M4.6).
//!
//! Spec: `specs/08-platformvm-pchain.md` §11.1; `specs/02-testing-strategy.md`
//! §4, §6. Go reference: `../avalanchego/vms/platformvm/txs/*_test.go` and
//! `../avalanchego/vms/platformvm/block/`.
//!
//! Two tests:
//!
//! * `golden::pchain_block_hash` — iterates every block JSON vector under
//!   `tests/vectors/platformvm/`, asserting `Block::parse(bytes).id() ==
//!   sha256(bytes) == expected_id` and a byte-exact re-encode.
//! * `golden::pchain_tx_codec` — byte-exact `Codec.Marshal` + round-trip for one
//!   `UnsignedTx` vector per covered variant, ported verbatim from the Go
//!   `expectedBytes` constants. Currently covers `AddPermissionlessValidatorTx`
//!   (25), `RegisterL1ValidatorTx` (36), `IncreaseL1ValidatorBalanceTx` (38),
//!   `SetL1ValidatorWeightTx` (37) and `DisableL1ValidatorTx` (39). The four L1
//!   txs share one identical Go `BaseTx` body (built once here); see
//!   `tests/PORTING.md` for the per-variant coverage matrix and the variants
//!   covered only by the `prop_roundtrip` round-trip property test.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use serde::Deserialize;

use ava_crypto::hashing;
use ava_platformvm::CODEC_VERSION;
use ava_platformvm::block::{Block, BlockBody};
use ava_platformvm::signer::{ProofOfPossession, Signer};
use ava_platformvm::stakeable::{LockIn, LockOut};
use ava_platformvm::txs::components::{
    Auth, BaseTx as AvaxBaseTx, Input, Output, Owner, TransferableInput, TransferableOutput,
};
use ava_platformvm::txs::{
    self, AddPermissionlessValidatorTx, BaseTx, DisableL1ValidatorTx, IncreaseL1ValidatorBalanceTx,
    RegisterL1ValidatorTx, SetL1ValidatorWeightTx, UnsignedTx, Validator,
};
use ava_secp256k1fx::{Input as Secp256k1Input, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;

// ---------------------------------------------------------------------------
// Shared constants (verbatim from the Go serialization vectors)
// ---------------------------------------------------------------------------

const AVAX_ASSET_ID: [u8; 32] = [
    0x21, 0xe6, 0x73, 0x17, 0xcb, 0xc4, 0xbe, 0x2a, 0xeb, 0x00, 0x67, 0x7a, 0xd6, 0x46, 0x27, 0x78,
    0xa8, 0xf5, 0x22, 0x74, 0xb9, 0xd6, 0x05, 0xdf, 0x25, 0x91, 0xb2, 0x30, 0x27, 0xa8, 0x7d, 0xff,
];
const CUSTOM_ASSET_ID: [u8; 32] = [
    0x99, 0x77, 0x55, 0x77, 0x11, 0x33, 0x55, 0x31, 0x99, 0x77, 0x55, 0x77, 0x11, 0x33, 0x55, 0x31,
    0x99, 0x77, 0x55, 0x77, 0x11, 0x33, 0x55, 0x31, 0x99, 0x77, 0x55, 0x77, 0x11, 0x33, 0x55, 0x31,
];
const TX_ID: [u8; 32] = [
    0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88,
    0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88,
];
const ADDR: [u8; 20] = [
    0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
    0x44, 0x55, 0x66, 0x77,
];
const VALIDATION_ID: [u8; 32] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
];
/// BLS compressed public key from the Go vectors.
const BLS_PUBKEY: [u8; 48] = [
    0xaf, 0xf4, 0xac, 0xb4, 0xc5, 0x43, 0x9b, 0x5d, 0x42, 0x6c, 0xad, 0xf9, 0xe9, 0x46, 0xd3, 0xa4,
    0x52, 0xf7, 0xde, 0x34, 0x14, 0xd1, 0xad, 0x27, 0x33, 0x61, 0x33, 0x21, 0x1d, 0x8b, 0x90, 0xcf,
    0x49, 0xfb, 0x97, 0xee, 0xbc, 0xde, 0xee, 0xf7, 0x14, 0xdc, 0x20, 0xf5, 0x4e, 0xd0, 0xd4, 0xd1,
];
/// BLS proof-of-possession signature from the Go vectors.
const BLS_SIG: [u8; 96] = [
    0x8c, 0xfd, 0x79, 0x09, 0xd1, 0x53, 0xb9, 0x60, 0x4b, 0x62, 0xb1, 0x43, 0xba, 0x36, 0x20, 0x7b,
    0xb7, 0xe6, 0x48, 0x67, 0x42, 0x44, 0x80, 0x20, 0x2a, 0x67, 0xdc, 0x68, 0x76, 0x83, 0x46, 0xd9,
    0x5c, 0x90, 0x98, 0x3c, 0x2d, 0x27, 0x9c, 0x64, 0xc4, 0x3c, 0x51, 0x13, 0x6b, 0x2a, 0x05, 0xe0,
    0x16, 0x02, 0xd5, 0x2a, 0xa6, 0x37, 0x6f, 0xda, 0x17, 0xfa, 0x6e, 0x2a, 0x18, 0xa0, 0x83, 0xe4,
    0x9d, 0x9c, 0x45, 0x0e, 0xab, 0x7b, 0x89, 0xb1, 0xd5, 0x55, 0x5d, 0xa5, 0xc4, 0x89, 0x87, 0x2e,
    0x02, 0xb7, 0xe5, 0x22, 0x7b, 0x77, 0x55, 0x0a, 0xf1, 0x33, 0x0e, 0x5a, 0x71, 0xf8, 0xc3, 0x68,
];

fn id(bytes: [u8; 32]) -> Id {
    Id::from(bytes)
}

fn owners_one_addr() -> OutputOwners {
    OutputOwners::new(0, 1, vec![ShortId::from(ADDR)])
}

// ---------------------------------------------------------------------------
// Shared L1 BaseTx body (verbatim from the four L1 Go serialization vectors,
// which share an identical `BaseTx`: net 10, two stakeable-lock outs, three
// ins incl. a stakeable-lock in, and the emoji memo).
// ---------------------------------------------------------------------------

fn l1_shared_base() -> BaseTx {
    let out0 = TransferableOutput {
        asset_id: id(AVAX_ASSET_ID),
        out: Output::StakeableLock(LockOut::new(
            87_654_321,
            Output::Transfer(TransferOutput::new(
                1,
                OutputOwners::new(12_345_678, 0, vec![]),
            )),
        )),
    };
    let out1 = TransferableOutput {
        asset_id: id(CUSTOM_ASSET_ID),
        out: Output::StakeableLock(LockOut::new(
            876_543_210,
            Output::Transfer(TransferOutput::new(
                0xffff_ffff_ffff_ffff,
                owners_one_addr(),
            )),
        )),
    };
    let in0 = TransferableInput {
        tx_id: id(TX_ID),
        output_index: 1,
        asset_id: id(AVAX_ASSET_ID),
        r#in: Input::Transfer(TransferInput::new(1_000_000_000, vec![2, 5])),
    };
    let in1 = TransferableInput {
        tx_id: id(TX_ID),
        output_index: 2,
        asset_id: id(CUSTOM_ASSET_ID),
        r#in: Input::StakeableLock(LockIn::new(
            876_543_210,
            Input::Transfer(TransferInput::new(0xefff_ffff_ffff_ffff, vec![0])),
        )),
    };
    let in2 = TransferableInput {
        tx_id: id(TX_ID),
        output_index: 3,
        asset_id: id(CUSTOM_ASSET_ID),
        r#in: Input::Transfer(TransferInput::new(0x1000_0000_0000_0000, vec![])),
    };

    BaseTx::new(AvaxBaseTx {
        network_id: 10, // constants.UnitTestID
        blockchain_id: Id::EMPTY,
        outs: vec![out0, out1],
        ins: vec![in0, in1, in2],
        memo: "😅\nwell that's\x01\x23\x45!".as_bytes().to_vec(),
    })
}

/// The shared L1 `BaseTx` body bytes (header + outs + ins + memo), parameterized
/// by the tx type id. Reproduces the common prefix of all four L1 vectors.
fn l1_prefix_bytes(type_id: u32) -> Vec<u8> {
    let mut v = vec![0x00, 0x00]; // codec version
    v.extend_from_slice(&type_id.to_be_bytes());
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x0a]); // network id = 10
    v.extend_from_slice(&[0u8; 32]); // blockchain id
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // num outputs
    // outputs[0]: AVAX, LockOut(22) { locktime, TransferOutput(7) { amt=1, owners{locktime,0,0} } }
    v.extend_from_slice(&AVAX_ASSET_ID);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x16]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x05, 0x39, 0x7f, 0xb1]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0xbc, 0x61, 0x4e]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    // outputs[1]: custom, LockOut(22) { locktime, TransferOutput(7) { amt=max, owners{0,1,[addr]} } }
    v.extend_from_slice(&CUSTOM_ASSET_ID);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x16]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x34, 0x3e, 0xfc, 0xea]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]);
    v.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    v.extend_from_slice(&ADDR);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x03]); // num inputs
    // inputs[0]: AVAX, TransferInput(5) { amt=1 Avax, sigs[2,5] }
    v.extend_from_slice(&TX_ID);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    v.extend_from_slice(&AVAX_ASSET_ID);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x3b, 0x9a, 0xca, 0x00]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
    // inputs[1]: custom, LockIn(21) { locktime, TransferInput(5) { amt, sigs[0] } }
    v.extend_from_slice(&TX_ID);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
    v.extend_from_slice(&CUSTOM_ASSET_ID);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x15]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x34, 0x3e, 0xfc, 0xea]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
    v.extend_from_slice(&[0xef, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    // inputs[2]: custom, TransferInput(5) { amt, sigs[] }
    v.extend_from_slice(&TX_ID);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x03]);
    v.extend_from_slice(&CUSTOM_ASSET_ID);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
    v.extend_from_slice(&[0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    // memo (len 20, the emoji bytes)
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x14]);
    v.extend_from_slice(&[
        0xf0, 0x9f, 0x98, 0x85, 0x0a, 0x77, 0x65, 0x6c, 0x6c, 0x20, 0x74, 0x68, 0x61, 0x74, 0x27,
        0x73, 0x01, 0x23, 0x45, 0x21,
    ]);
    v
}

// ---------------------------------------------------------------------------
// Per-variant tx golden vectors
// ---------------------------------------------------------------------------

/// `(name, UnsignedTx, expected_bytes)` triples covered by byte-exact Go vectors.
fn tx_golden_cases() -> Vec<(&'static str, UnsignedTx, Vec<u8>)> {
    let mut cases = Vec::new();

    // --- AddPermissionlessValidatorTx (25), the "simple primary" vector ---
    {
        const KILO_AVAX: u64 = 2_000 * 1_000_000_000;
        let tx = AddPermissionlessValidatorTx {
            base: BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![TransferableInput {
                    tx_id: id(TX_ID),
                    output_index: 1,
                    asset_id: id(AVAX_ASSET_ID),
                    r#in: Input::Transfer(TransferInput::new(KILO_AVAX, vec![1])),
                }],
                memo: vec![],
            }),
            validator: Validator {
                node_id: NodeId::from([
                    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x11, 0x22, 0x33, 0x44, 0x55,
                    0x66, 0x77, 0x88, 0x11, 0x22, 0x33, 0x44,
                ]),
                start: 12345,
                end: 12345 + 200 * 24 * 60 * 60,
                wght: KILO_AVAX,
            },
            subnet: Id::EMPTY,
            signer: Signer::ProofOfPossession(ProofOfPossession::new(BLS_PUBKEY, BLS_SIG)),
            stake_outs: vec![TransferableOutput {
                asset_id: id(AVAX_ASSET_ID),
                out: Output::Transfer(TransferOutput::new(KILO_AVAX, owners_one_addr())),
            }],
            validator_rewards_owner: Owner::Secp256k1(owners_one_addr()),
            delegator_rewards_owner: Owner::Secp256k1(owners_one_addr()),
            delegation_shares: 1_000_000,
            verified: std::cell::OnceCell::new(),
        };

        let mut v = vec![
            0x00, 0x00, // codec version
            0x00, 0x00, 0x00, 0x19, // AddPermissionlessValidatorTx type id (25)
            0x00, 0x00, 0x00, 0x01, // network id = 1
        ];
        v.extend_from_slice(&[0u8; 32]); // blockchain id
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // num outputs
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num inputs
        v.extend_from_slice(&TX_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // output index
        v.extend_from_slice(&AVAX_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]); // TransferInput type id
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xd1, 0xa9, 0x4a, 0x20, 0x00]); // amount
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num sig indices
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // sig index
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // memo len
        v.extend_from_slice(&[
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88, 0x11, 0x22, 0x33, 0x44,
        ]); // node id
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x39]); // start
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x01, 0x07, 0xdc, 0x39]); // end
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xd1, 0xa9, 0x4a, 0x20, 0x00]); // weight
        v.extend_from_slice(&[0u8; 32]); // subnet id
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x1c]); // BLS PoP type id (28)
        v.extend_from_slice(&BLS_PUBKEY);
        v.extend_from_slice(&BLS_SIG);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num stake outs
        v.extend_from_slice(&AVAX_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]); // TransferOutput type id
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xd1, 0xa9, 0x4a, 0x20, 0x00]); // amount
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // locktime
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // threshold
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num addrs
        v.extend_from_slice(&ADDR);
        // validator rewards owner
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x0b]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&ADDR);
        // delegator rewards owner
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x0b]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&ADDR);
        v.extend_from_slice(&[0x00, 0x0f, 0x42, 0x40]); // delegation shares = 1_000_000

        cases.push((
            "AddPermissionlessValidatorTx",
            UnsignedTx::AddPermissionlessValidator(tx),
            v,
        ));
    }

    // --- RegisterL1ValidatorTx (36) ---
    {
        let tx = RegisterL1ValidatorTx {
            base: l1_shared_base(),
            balance: 1_000_000_000,
            proof_of_possession: BLS_SIG,
            message: b"message".to_vec(),
        };
        let mut v = l1_prefix_bytes(36);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x3b, 0x9a, 0xca, 0x00]); // balance
        v.extend_from_slice(&BLS_SIG); // proof of possession
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]); // message len
        v.extend_from_slice(b"message");
        cases.push((
            "RegisterL1ValidatorTx",
            UnsignedTx::RegisterL1Validator(tx),
            v,
        ));
    }

    // --- IncreaseL1ValidatorBalanceTx (38) ---
    {
        let tx = IncreaseL1ValidatorBalanceTx {
            base: l1_shared_base(),
            validation_id: id(VALIDATION_ID),
            balance: 0xfedc_ba98_7654_3210,
        };
        let mut v = l1_prefix_bytes(38);
        v.extend_from_slice(&VALIDATION_ID);
        v.extend_from_slice(&[0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10]); // balance
        cases.push((
            "IncreaseL1ValidatorBalanceTx",
            UnsignedTx::IncreaseL1ValidatorBalance(tx),
            v,
        ));
    }

    // --- SetL1ValidatorWeightTx (37) ---
    {
        let tx = SetL1ValidatorWeightTx {
            base: l1_shared_base(),
            message: b"message".to_vec(),
        };
        let mut v = l1_prefix_bytes(37);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]); // message len
        v.extend_from_slice(b"message");
        cases.push((
            "SetL1ValidatorWeightTx",
            UnsignedTx::SetL1ValidatorWeight(tx),
            v,
        ));
    }

    // --- DisableL1ValidatorTx (39) ---
    {
        let tx = DisableL1ValidatorTx {
            base: l1_shared_base(),
            validation_id: id(VALIDATION_ID),
            disable_auth: Auth::Secp256k1(Secp256k1Input::new(vec![9])),
        };
        let mut v = l1_prefix_bytes(39);
        v.extend_from_slice(&VALIDATION_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x0a]); // disable auth type id (secp256k1fx.Input 10)
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num indices
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x09]); // index 9
        cases.push((
            "DisableL1ValidatorTx",
            UnsignedTx::DisableL1Validator(tx),
            v,
        ));
    }

    cases
}

/// Byte-exact `Codec.Marshal` + round-trip for each covered `UnsignedTx`
/// variant, ported verbatim from the Go `expectedBytes` constants.
#[test]
fn pchain_tx_codec() {
    let c = txs::codec::codec().expect("build codec");
    for (name, unsigned, expected) in tx_golden_cases() {
        let got = c.marshal(CODEC_VERSION, &unsigned).expect("marshal");
        assert_eq!(got, expected, "{name}: byte-exact Codec.Marshal");

        // encode(decode(bytes)) == bytes.
        let mut decoded = UnsignedTx::default();
        c.unmarshal(&got, &mut decoded).expect("unmarshal");
        assert_eq!(decoded, unsigned, "{name}: round-trip equality");
        let reencoded = c.marshal(CODEC_VERSION, &decoded).expect("re-marshal");
        assert_eq!(
            reencoded, expected,
            "{name}: encode(decode(bytes)) == bytes"
        );
    }
}

// ---------------------------------------------------------------------------
// Block hash goldens
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BlockVector {
    type_id: u32,
    parent_hex: String,
    height: u64,
    bytes: String,
    id_hex: String,
}

/// Iterates every `*_block.json` vector under `tests/vectors/platformvm/`,
/// asserting `Block::parse(bytes).id() == sha256(bytes) == expected_id`, the
/// decoded common fields, and a byte-exact re-encode.
#[test]
fn pchain_block_hash() {
    let c = txs::codec::codec().expect("build codec");
    let dir = format!("{}/tests/vectors/platformvm", env!("CARGO_MANIFEST_DIR"));

    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("read vectors dir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.ends_with("_block.json"))
        .collect();
    names.sort();
    assert!(
        names.len() >= 3,
        "expected at least the 3 seeded block vectors, found {names:?}"
    );

    for name in names {
        let raw = std::fs::read_to_string(format!("{dir}/{name}")).expect("read vector");
        let v: BlockVector = serde_json::from_str(&raw).expect("parse vector");

        let bytes = hex::decode(&v.bytes).expect("decode bytes hex");
        let want_id_arr: [u8; 32] = hex::decode(&v.id_hex)
            .expect("decode id hex")
            .try_into()
            .expect("id is 32 bytes");
        let want_id = Id::from(want_id_arr);
        let parent_arr: [u8; 32] = hex::decode(&v.parent_hex)
            .expect("decode parent hex")
            .try_into()
            .expect("parent is 32 bytes");
        let want_parent = Id::from(parent_arr);

        let blk = Block::parse(&c, &bytes).expect("parse block");
        assert_eq!(blk.id(), want_id, "{name}: block_id mismatch");
        assert_eq!(
            blk.id(),
            Id::from(hashing::sha256(&bytes)),
            "{name}: block_id != sha256(bytes)"
        );
        assert_eq!(blk.type_id(), v.type_id, "{name}: type_id mismatch");
        assert_eq!(blk.parent_id(), want_parent, "{name}: parent mismatch");
        assert_eq!(blk.height(), v.height, "{name}: height mismatch");

        let reenc = c.marshal(CODEC_VERSION, blk.body()).expect("re-marshal");
        assert_eq!(reenc, bytes, "{name}: re-encode != original bytes");

        // The parsed body is a real `BlockBody` variant (its discriminant is
        // asserted via `blk.type_id()` above).
        let _body: &BlockBody = blk.body();
    }
}
