// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Self-consistent X-Chain `StandardBlock` block-hash golden harness (M5.15).
//!
//! Spec: `specs/09-avm-xchain.md` §7 (StandardBlock field order: parent, height,
//! time, root, txs; type id 20); `specs/02-testing-strategy.md` §6 (golden block
//! hashes). Go reference: `../avalanchego/vms/avm/block/{block,standard_block,
//! parser}.go`.
//!
//! Go's `vms/avm/block/block_test.go` builds blocks **programmatically** — there
//! is no hardcoded `expectedBytes` hex const to copy — so this golden is
//! self-consistent, not a Go-extracted byte vector (the full Go-byte-exact
//! differential is M5.22's job). It:
//!
//! 1. constructs a deterministic `StandardBlock` whose single contained tx is the
//!    exact `BaseTx` from the M5.5 `golden_tx_codec` vector (so the block bytes are
//!    deterministic);
//! 2. asserts the on-wire field order matches `standard_block.go` (parent, height,
//!    time, root, txs), locking byte-exactness of the FORMAT;
//! 3. marshals via the `Block` interface (type-id-prefix 20), asserts
//!    `block_id == sha256(bytes)`, and round-trips `parse(bytes)` → re-marshal →
//!    equal bytes + equal id (and re-derives the contained tx's `tx_id`).
//!
//! TODO(M8/ava-genesis): assert mainnet/fuji X-Chain genesis block id +
//! stop-vertex parent against the `ava-genesis` constants (the crate does not
//! exist yet — M8).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use ava_avm::block::{Block, BlockBody, StandardBlock};
use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput};
use ava_avm::txs::{BaseTx, CODEC_VERSION, Tx, UnsignedTx};
use ava_crypto::hashing;
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

/// Rebuilds the exact `BaseTx` from the M5.5 `golden_tx_codec` vector (ported
/// verbatim from Go `base_tx_test.go`), so the enclosing block bytes are
/// deterministic.
fn golden_base_tx() -> BaseTx {
    let mut chain_id = [0u8; 32];
    chain_id[..5].copy_from_slice(&[0x05, 0x04, 0x03, 0x02, 0x01]);
    let mut asset_id = [0u8; 32];
    asset_id[..3].copy_from_slice(&[0x01, 0x02, 0x03]);
    let addr: [u8; 20] = [
        0xfc, 0xed, 0xa8, 0xf9, 0x0f, 0xcb, 0x5d, 0x30, 0x61, 0x4b, 0x99, 0xd7, 0x9f, 0xc4, 0xba,
        0xa2, 0x93, 0x07, 0x76, 0x26,
    ];
    let in_tx_id: [u8; 32] = [
        0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8, 0xf7, 0xf6, 0xf5, 0xf4, 0xf3, 0xf2, 0xf1,
        0xf0, 0xef, 0xee, 0xed, 0xec, 0xeb, 0xea, 0xe9, 0xe8, 0xe7, 0xe6, 0xe5, 0xe4, 0xe3, 0xe2,
        0xe1, 0xe0,
    ];

    BaseTx::new(AvaxBaseTx {
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
    })
}

mod golden {
    use super::*;

    #[test]
    fn xchain_block_hash() {
        let c = Codec();

        // A deterministic parent id and a single deterministic contained tx.
        let mut parent_arr = [0u8; 32];
        parent_arr[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let parent_id = Id::from(parent_arr);
        let height: u64 = 7;
        let time: u64 = 1_640_000_000;

        let mut tx = Tx::new(UnsignedTx::Base(golden_base_tx()));
        tx.initialize(c).expect("initialize tx");
        let want_tx_id = tx.id();
        let want_tx_bytes = tx.bytes().to_vec();

        // Build the block (marshals as the Block interface, type-id-prefix 20).
        let blk =
            StandardBlock::new_block(c, parent_id, height, time, vec![tx]).expect("build block");
        let bytes = blk.bytes().to_vec();

        // --- field-order lock: parent, height, time, root, txs (09 §7) ---
        // The on-wire layout is: codec version (2) | block typeID 20 (4) |
        // parent (32) | height (8) | time (8) | root (32) | num txs (4) | tx[0]…
        assert_eq!(&bytes[0..2], &[0x00, 0x00], "codec version prefix");
        assert_eq!(
            &bytes[2..6],
            &[0x00, 0x00, 0x00, 0x14],
            "block typeID == 20"
        );
        assert_eq!(&bytes[6..38], &parent_arr, "parent id field first");
        assert_eq!(
            &bytes[38..46],
            &height.to_be_bytes(),
            "height (u64 be) second"
        );
        assert_eq!(&bytes[46..54], &time.to_be_bytes(), "time (u64 be) third");
        assert_eq!(&bytes[54..86], &[0u8; 32], "merkle root (zero) fourth");
        assert_eq!(
            &bytes[86..90],
            &[0x00, 0x00, 0x00, 0x01],
            "num txs == 1 (txs last)"
        );
        // The contained tx follows the txs-length prefix, byte-exact. A tx
        // embedded inside the block shares the block's single 2-byte codec
        // version prefix, so its inline bytes are the standalone signed-tx bytes
        // with their own leading 2-byte version prefix stripped.
        assert_eq!(
            &bytes[90..],
            &want_tx_bytes[2..],
            "tx[0] bytes inline (version-prefix shared with block)"
        );

        // --- block_id == sha256(bytes) ---
        assert_eq!(
            blk.id(),
            Id::from(hashing::sha256(&bytes)),
            "block_id == sha256(bytes)"
        );

        // --- field accessors ---
        assert_eq!(blk.parent_id(), parent_id, "parent accessor");
        assert_eq!(blk.height(), height, "height accessor");
        assert_eq!(blk.timestamp(), time, "timestamp (unix secs) accessor");
        assert_eq!(blk.merkle_root(), Id::EMPTY, "merkle root zero/unused");
        assert_eq!(blk.txs().len(), 1, "one contained tx");
        assert_eq!(blk.type_id(), 20, "block type_id == 20");

        // --- round-trip: parse → re-marshal → equal bytes + equal id ---
        let parsed = Block::parse(c, &bytes).expect("parse block");
        assert_eq!(parsed.id(), blk.id(), "round-trip block_id");
        assert_eq!(parsed.bytes(), bytes.as_slice(), "round-trip bytes");
        assert_eq!(parsed.parent_id(), parent_id, "round-trip parent");
        assert_eq!(parsed.height(), height, "round-trip height");
        assert_eq!(parsed.timestamp(), time, "round-trip time");
        assert_eq!(parsed.txs().len(), 1, "round-trip tx count");

        // parse re-derives each contained tx's tx_id.
        assert_eq!(parsed.txs()[0].id(), want_tx_id, "parse re-derives tx_id");
        assert_eq!(
            parsed.txs()[0].bytes(),
            want_tx_bytes.as_slice(),
            "parse re-caches tx bytes"
        );

        // Re-marshal the parsed body is byte-exact.
        let reenc = c
            .marshal(CODEC_VERSION, parsed.body())
            .expect("re-marshal body");
        assert_eq!(reenc, bytes, "re-encode == original bytes");

        // The parsed variant is a StandardBlock.
        assert!(
            matches!(parsed.body(), BlockBody::Standard(_)),
            "parsed variant is StandardBlock"
        );
    }
}
