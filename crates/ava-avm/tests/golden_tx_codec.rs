// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Byte-exact X-Chain tx-codec golden harness (M5.5).
//!
//! Spec: `specs/09-avm-xchain.md` §2.1 (the 21-entry type-ID table), §2.2,
//! CODEC-AVM-1; `specs/02-testing-strategy.md` §6 (golden). Go reference:
//! `../avalanchego/vms/avm/txs/*_test.go` and the fx `Initialize` registration
//! order in `../avalanchego/vms/{secp256k1fx,nftfx,propertyfx}/fx.go`.
//!
//! Two assertions:
//!
//! * the standard and genesis registries both assign the exact 21-entry
//!   `(name, type_id)` table from §2.1 (ids 0..20), and the `TypeToFxIndex`
//!   routing table maps each fx type-id to its fx ordinal (§2.2); and
//! * byte-exact `Codec.Marshal` + round-trip for a hand-constructed `BaseTx`,
//!   ported verbatim from the Go `base_tx_test.go` `expected` constant, with
//!   `tx_id == sha256(signed_bytes)`.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use ava_avm::FxIndex;
use ava_avm::txs::codec::{
    Codec, GenesisCodec, build_type_id_registry, type_id_registry_table, type_to_fx_index,
};
use ava_avm::txs::components::{AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput};
use ava_avm::txs::{BaseTx, Tx, UnsignedTx};
use ava_crypto::hashing;
use ava_secp256k1fx::{Input as SecpInput, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

/// The canonical 21-entry `(name, type_id)` table (specs 09 §2.1).
const EXPECTED_TABLE: &[(&str, u32)] = &[
    ("BaseTx", 0),
    ("CreateAssetTx", 1),
    ("OperationTx", 2),
    ("ImportTx", 3),
    ("ExportTx", 4),
    ("secp256k1fx.TransferInput", 5),
    ("secp256k1fx.MintOutput", 6),
    ("secp256k1fx.TransferOutput", 7),
    ("secp256k1fx.MintOperation", 8),
    ("secp256k1fx.Credential", 9),
    ("nftfx.MintOutput", 10),
    ("nftfx.TransferOutput", 11),
    ("nftfx.MintOperation", 12),
    ("nftfx.TransferOperation", 13),
    ("nftfx.Credential", 14),
    ("propertyfx.MintOutput", 15),
    ("propertyfx.OwnedOutput", 16),
    ("propertyfx.MintOperation", 17),
    ("propertyfx.BurnOperation", 18),
    ("propertyfx.Credential", 19),
    ("block.StandardBlock", 20),
];

mod golden {
    use super::*;

    #[test]
    fn xchain_tx_codec() {
        // --- (a) all 21 type-ids on both registries (CODEC-AVM-1) ---
        let table: Vec<(String, u32)> = type_id_registry_table();
        let expected: Vec<(String, u32)> = EXPECTED_TABLE
            .iter()
            .map(|(n, i)| ((*n).to_string(), *i))
            .collect();
        assert_eq!(table, expected, "21-entry type-ID table mismatch");

        let r = build_type_id_registry().expect("build registry");
        assert_eq!(r.next_id(), 21, "registry must end at id 21");

        // The two managers register the same numbering space; they only differ in
        // their max decode size. Both must be constructible.
        let c = Codec();
        let gc = GenesisCodec();

        // --- (a') TypeToFxIndex routing table (specs 09 §2.2) ---
        let fx = type_to_fx_index();
        // secp256k1fx 5..9
        for id in 5u32..=9 {
            assert_eq!(fx.get(&id), Some(&FxIndex::Secp256k1), "type {id} -> secp");
        }
        // nftfx 10..14
        for id in 10u32..=14 {
            assert_eq!(fx.get(&id), Some(&FxIndex::Nft), "type {id} -> nft");
        }
        // propertyfx 15..19
        for id in 15u32..=19 {
            assert_eq!(
                fx.get(&id),
                Some(&FxIndex::Property),
                "type {id} -> property"
            );
        }
        // tx types (0..4) and the block (20) are not fx types.
        for id in [0u32, 1, 2, 3, 4, 20] {
            assert_eq!(fx.get(&id), None, "type {id} is not an fx type");
        }

        // --- (b) byte-exact BaseTx, ported verbatim from Go base_tx_test.go ---
        // chainID = ids.ID{5,4,3,2,1}; assetID = ids.ID{1,2,3}; networkID = 10.
        let mut chain_id = [0u8; 32];
        chain_id[..5].copy_from_slice(&[0x05, 0x04, 0x03, 0x02, 0x01]);
        let mut asset_id = [0u8; 32];
        asset_id[..3].copy_from_slice(&[0x01, 0x02, 0x03]);
        // keys[0].PublicKey().Address()
        let addr: [u8; 20] = [
            0xfc, 0xed, 0xa8, 0xf9, 0x0f, 0xcb, 0x5d, 0x30, 0x61, 0x4b, 0x99, 0xd7, 0x9f, 0xc4,
            0xba, 0xa2, 0x93, 0x07, 0x76, 0x26,
        ];
        let in_tx_id: [u8; 32] = [
            0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8, 0xf7, 0xf6, 0xf5, 0xf4, 0xf3, 0xf2,
            0xf1, 0xf0, 0xef, 0xee, 0xed, 0xec, 0xeb, 0xea, 0xe9, 0xe8, 0xe7, 0xe6, 0xe5, 0xe4,
            0xe3, 0xe2, 0xe1, 0xe0,
        ];

        let base = BaseTx::new(AvaxBaseTx {
            network_id: 10,
            blockchain_id: Id::from(chain_id),
            outs: vec![TransferableOutput {
                asset_id: Id::from(asset_id),
                out: Output::SecpTransfer(TransferOutput::new(
                    12345,
                    OutputOwners::new(0, 1, vec![ShortId::from(addr)]),
                )),
            }],
            ins: vec![TransferableInput {
                tx_id: Id::from(in_tx_id),
                output_index: 1,
                asset_id: Id::from(asset_id),
                r#in: Input::SecpTransfer(TransferInput::new(54321, vec![2])),
            }],
            memo: vec![0x00, 0x01, 0x02, 0x03],
        });

        // The Go `expected` bytes are the *signed* tx bytes: unsigned (typeid-prefixed)
        // + empty creds. Built here as a flat byte vec.
        let expected: Vec<u8> = {
            let mut v = Vec::new();
            v.extend_from_slice(&[0x00, 0x00]); // codec version
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // UnsignedTx typeID = BaseTx (0)
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x0a]); // networkID = 10
            v.extend_from_slice(&chain_id); // blockchainID
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num outs
            v.extend_from_slice(&asset_id); // out[0].assetID
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]); // fxID = TransferOutput (7)
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x39]); // amount 12345
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // locktime
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // threshold
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num addrs
            v.extend_from_slice(&addr); // address[0]
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num ins
            v.extend_from_slice(&in_tx_id); // in[0].txID
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // utxo index
            v.extend_from_slice(&asset_id); // in[0].assetID
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]); // fxID = TransferInput (5)
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd4, 0x31]); // amount 54321
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num sigs
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // sig index 2
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x04]); // memo len
            v.extend_from_slice(&[0x00, 0x01, 0x02, 0x03]); // memo
            v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // num credentials
            v
        };

        let mut tx = Tx::new(UnsignedTx::Base(base));
        tx.initialize(c).expect("initialize");

        assert_eq!(tx.bytes(), expected.as_slice(), "BaseTx byte-exact");
        assert_eq!(
            tx.id(),
            Id::from(hashing::sha256(&expected)),
            "tx_id == sha256(signed_bytes)"
        );

        // Round-trip via the genesis codec (same numbering, larger max).
        let parsed = Tx::parse(gc, tx.bytes()).expect("parse");
        assert_eq!(parsed.unsigned, tx.unsigned, "round-trip unsigned");
        assert_eq!(parsed.id(), tx.id(), "round-trip id");

        // Silence: SecpInput is imported for documentation parity with the Go test
        // (the BaseTx input embeds a secp Input); not directly constructed here.
        let _ = SecpInput::default();
    }
}
