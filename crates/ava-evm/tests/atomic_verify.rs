// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Atomic semantic-verify tests (spec 10 §6.5): conflict sets across a block's
//! own txs + its processing ancestry, and the coreth mainnet `bonusBlocks`
//! skip-set (height → block ID).
//!
//! Reference: coreth `plugin/evm/atomic/vm/tx_semantic_verifier.go` (`conflicts`),
//! `plugin/evm/atomic/vm/vm.go` (`verifyTxs`), `plugin/evm/atomic/import_tx.go` /
//! `export_tx.go` (`InputUTXOs`), `plugin/evm/atomic/vm/bonus_blocks.go`.

use std::collections::BTreeSet;

use assert_matches::assert_matches;
use ava_avm::txs::components::{Input as FxInput, TransferableInput};
use ava_evm::atomic::tx::{AtomicTx, EvmInput, UnsignedExportTx, UnsignedImportTx};
use ava_evm::atomic::verify::{input_utxos, mainnet_bonus_blocks, verify_no_conflicts};
use ava_evm::error::Error;
use ava_secp256k1fx::TransferInput;
use ava_types::id::Id;

/// 32-byte id with every byte = `b`.
fn id32(b: u8) -> Id {
    Id::from([b; 32])
}

/// An import tx consuming a single UTXO `(tx_id, output_index)`.
fn import_consuming(tx_id: Id, output_index: u32) -> AtomicTx {
    AtomicTx::Import(UnsignedImportTx {
        network_id: 1,
        blockchain_id: id32(0x11),
        source_chain: id32(0x22),
        imported_inputs: vec![TransferableInput {
            tx_id,
            output_index,
            asset_id: id32(0xAA),
            r#in: FxInput::SecpTransfer(TransferInput::new(5000, vec![0])),
        }],
        outs: Vec::new(),
    })
}

/// An export tx debiting a single EVM account `(address, nonce)`.
fn export_debiting(address: [u8; 20], nonce: u64) -> AtomicTx {
    AtomicTx::Export(UnsignedExportTx {
        network_id: 1,
        blockchain_id: id32(0x11),
        destination_chain: id32(0x33),
        ins: vec![EvmInput {
            address,
            amount: 3000,
            asset_id: id32(0xAA),
            nonce,
        }],
        exported_outputs: Vec::new(),
    })
}

#[test]
fn rejects_conflicting_inputs_across_ancestry() {
    // The single source UTXO the import consumes.
    let import = import_consuming(id32(0x44), 1);
    let consumed = input_utxos(&import);
    assert_eq!(consumed.len(), 1);

    // 1) A clean block with one import + one (disjoint) export passes against an
    //    empty ancestry.
    let export = export_debiting([0x05; 20], 7);
    let no_ancestors = BTreeSet::new();
    verify_no_conflicts(&[import.clone(), export.clone()], &no_ancestors)
        .expect("disjoint inputs verify");

    // 2) Intra-block conflict: two txs in the SAME block consuming the same UTXO.
    let import_dup = import_consuming(id32(0x44), 1);
    let err = verify_no_conflicts(&[import.clone(), import_dup], &no_ancestors)
        .expect_err("intra-block conflict must be rejected");
    assert_matches!(err, Error::ConflictingAtomicInputs);

    // 3) Ancestry conflict: the UTXO is already consumed by an atomic tx in a
    //    processing ancestor block (its input set seeds `ancestor_inputs`).
    let mut ancestor_inputs: BTreeSet<Id> = BTreeSet::new();
    ancestor_inputs.extend(consumed.iter().copied());
    let err = verify_no_conflicts(std::slice::from_ref(&import), &ancestor_inputs)
        .expect_err("ancestry conflict must be rejected");
    assert_matches!(err, Error::ConflictingAtomicInputs);

    // 4) Export-vs-export ancestry conflict (the (nonce,address) input id).
    let export_dup = export_debiting([0x05; 20], 7);
    let mut export_ancestry: BTreeSet<Id> = BTreeSet::new();
    export_ancestry.extend(input_utxos(&export).iter().copied());
    let err = verify_no_conflicts(&[export_dup], &export_ancestry)
        .expect_err("export ancestry conflict must be rejected");
    assert_matches!(err, Error::ConflictingAtomicInputs);

    // 5) The export input id is byte-identical to coreth's
    //    `Packer{PackLong(nonce); PackBytes(address)}`:
    //    nonce(8 BE) ++ 0x00000014 (len=20) ++ address(20).
    let export_ids = input_utxos(&export);
    assert_eq!(export_ids.len(), 1);
    let want = {
        let mut raw = [0u8; 32];
        raw[..8].copy_from_slice(&7u64.to_be_bytes());
        raw[8..12].copy_from_slice(&20u32.to_be_bytes());
        raw[12..].copy_from_slice(&[0x05; 20]);
        Id::from(raw)
    };
    assert!(
        export_ids.contains(&want),
        "export input id must match Go packing"
    );

    // 6) The import input id is `tx_id.prefix(output_index)`.
    let want_import = id32(0x44).prefix(&[1]);
    assert!(
        consumed.contains(&want_import),
        "import input id must be InputID()"
    );
}

#[test]
fn bonus_blocks_skip_set_matches_go() {
    let bonus = mainnet_bonus_blocks();

    // The map has exactly the 57 mainnet bonus blocks coreth ships
    // (`plugin/evm/atomic/vm/bonus_blocks.go`).
    assert_eq!(bonus.len(), 57);

    // Spot-check the boundary entries verbatim against the Go source.
    assert_eq!(
        bonus.get(&102972).map(Id::to_string),
        Some("Njm9TcLUXRojZk8YhEM6ksvfiPdC1TME4zJvGaDXgzMCyB6oB".to_string())
    );
    assert_eq!(
        bonus.get(&103105).map(Id::to_string),
        Some("BYqLB6xpqy7HsAgP2XNfGE8Ubg1uEzse5mBPTSJH9z5s8pvMa".to_string())
    );
    assert_eq!(
        bonus.get(&103633).map(Id::to_string),
        Some("2QiHZwLhQ3xLuyyfcdo5yCUfoSqWDvRZox5ECU19HiswfroCGp".to_string())
    );

    // A height not in the set is not a bonus block.
    assert!(bonus.get(&1).is_none());
    assert!(bonus.get(&103634).is_none());
}
