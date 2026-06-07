// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Executor` integration tests — UTXO state transitions + atomic requests
//! (M5.14, specs 09 §6.3, EXEC-AVM-1, ATOMIC-1).
//!
//! Each case builds minimal tx structs, runs `Executor::execute`, and asserts:
//! - deleted input ids (consumed UTXOs),
//! - produced UTXO ids at the correct output indices (EXEC-AVM-1),
//! - atomic `Requests` for import/export chains.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;

use ava_avm::state::{Chain, Diff, ReadOnlyChain, State};
use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{
    Asset, AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput, UtxoId,
};
use ava_avm::txs::executor::exec::Executor;
use ava_avm::txs::executor::semantic::Utxo;
use ava_avm::txs::{
    BaseTx, CreateAssetTx, ExportTx, FxOperation, ImportTx, InitialState, Operation, OperationTx,
    Tx, UnsignedTx,
};
use ava_database::MemDb;
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

const NETWORK_ID: u32 = 10;

fn chain_id() -> Id {
    Id::from([0x05; 32])
}

fn source_chain() -> Id {
    Id::from([0x09; 32])
}

fn dest_chain() -> Id {
    Id::from([0x0b; 32])
}

fn asset_id() -> Id {
    Id::from([0xaa; 32])
}

fn addr() -> ShortId {
    ShortId::from([0xab; 20])
}

fn owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![addr()])
}

fn transfer_out(amt: u64) -> TransferableOutput {
    TransferableOutput {
        asset_id: asset_id(),
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    }
}

fn transfer_in(tx_byte: u8, idx: u32, amt: u64) -> TransferableInput {
    let mut raw = [0u8; 32];
    raw[0] = tx_byte;
    TransferableInput {
        tx_id: Id::from(raw),
        output_index: idx,
        asset_id: asset_id(),
        r#in: Input::SecpTransfer(TransferInput::new(amt, vec![0])),
    }
}

/// Seed a fresh `Diff` (over a `State`) with a UTXO for the given `(tx_byte, idx, amt)`.
fn diff_with_utxos(utxos: &[(u8, u32, u64)]) -> Diff {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");
    for &(tx_byte, idx, amt) in utxos {
        let mut raw = [0u8; 32];
        raw[0] = tx_byte;
        let utxo = Utxo {
            tx_id: Id::from(raw),
            output_index: idx,
            asset_id: asset_id(),
            out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
        };
        let utxo_id = utxo.input_id();
        state.add_utxo(utxo_id, utxo.marshal().expect("marshal"));
    }
    let parent: Arc<dyn Chain> = state.snapshot();
    Diff::new_on(parent).expect("diff")
}

fn input_id(tx_byte: u8, idx: u32) -> Id {
    let mut raw = [0u8; 32];
    raw[0] = tx_byte;
    Id::from(raw).prefix(&[u64::from(idx)])
}

// ---------------------------------------------------------------------------
// BaseTx: consume inputs, produce outputs
// ---------------------------------------------------------------------------

/// `base_tx_consume_produce` — the executor deletes each input's UTXO id and
/// adds a produced UTXO at `output_index = i` for each output.
#[test]
fn base_tx_consume_produce() {
    let mut diff = diff_with_utxos(&[(0x10, 0, 5000), (0x11, 0, 3000)]);

    let base = AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![transfer_out(4000), transfer_out(2000)],
        ins: vec![transfer_in(0x10, 0, 5000), transfer_in(0x11, 0, 3000)],
        memo: Vec::new(),
    };
    let mut tx = Tx::new(UnsignedTx::Base(BaseTx::new(base)));
    tx.initialize(Codec()).expect("initialize");
    let tx_id = tx.id();

    let result = Executor::execute(&tx.unsigned, tx_id, &mut diff).expect("execute");

    // No atomic requests for a plain BaseTx.
    assert!(result.atomic_requests.is_empty());

    // Each input's UTXO is deleted.
    assert!(diff.get_utxo(input_id(0x10, 0)).is_err());
    assert!(diff.get_utxo(input_id(0x11, 0)).is_err());

    // Produced UTXOs: output_index 0 and 1.
    let produced_0 = tx_id.prefix(&[0u64]);
    let produced_1 = tx_id.prefix(&[1u64]);
    assert!(diff.get_utxo(produced_0).is_ok(), "output index 0 missing");
    assert!(diff.get_utxo(produced_1).is_ok(), "output index 1 missing");
}

// ---------------------------------------------------------------------------
// CreateAssetTx: asset_id == tx_id, output_index continues from len(outs)
// ---------------------------------------------------------------------------

/// `create_asset_indexing` — EXEC-AVM-1: the BaseTx `outs` get indices 0..N,
/// then each `InitialState`'s outputs continue monotonically from N across
/// multiple `InitialState`s in declared order.
#[test]
fn create_asset_indexing() {
    // No inputs/outputs in the BaseTx — focus purely on InitialState indexing.
    let ca = CreateAssetTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_out(100)], // index 0
            ins: Vec::new(),
            memo: Vec::new(),
        }),
        name: "TestAsset".to_string(),
        symbol: "TAS".to_string(),
        denomination: 0,
        states: vec![
            // fx_index 0: two outputs → indices 1, 2
            InitialState::new(
                0,
                vec![
                    Output::SecpTransfer(TransferOutput::new(10, owners())),
                    Output::SecpTransfer(TransferOutput::new(20, owners())),
                ],
            ),
            // fx_index 1: one output → index 3
            InitialState::new(
                1,
                vec![Output::SecpTransfer(TransferOutput::new(30, owners()))],
            ),
        ],
    };

    let mut diff = diff_with_utxos(&[]);
    let mut tx = Tx::new(UnsignedTx::CreateAsset(ca));
    tx.initialize(Codec()).expect("initialize");
    let tx_id = tx.id();

    let result = Executor::execute(&tx.unsigned, tx_id, &mut diff).expect("execute");
    assert!(result.atomic_requests.is_empty());

    // BaseTx out at index 0.
    let base_out_0 = tx_id.prefix(&[0u64]);
    assert!(diff.get_utxo(base_out_0).is_ok(), "base out[0] missing");

    // InitialState outputs continue from index 1.
    // The asset_id in each UTXO equals the tx_id (EXEC-AVM-1).
    for (expected_idx, _label) in [(1u64, "s0_out0"), (2u64, "s0_out1"), (3u64, "s1_out0")] {
        let uid = tx_id.prefix(&[expected_idx]);
        let bytes = diff.get_utxo(uid).expect("utxo present");
        let utxo = Utxo::unmarshal(&bytes).expect("unmarshal");
        assert_eq!(utxo.tx_id, tx_id, "tx_id mismatch at index {expected_idx}");
        assert_eq!(
            utxo.asset_id, tx_id,
            "asset_id must equal tx_id (EXEC-AVM-1) at index {expected_idx}"
        );
        assert_eq!(utxo.output_index, expected_idx as u32, "output_index wrong");
    }
}

// ---------------------------------------------------------------------------
// OperationTx: op input UTXOs deleted, op outputs appended after outs
// ---------------------------------------------------------------------------

/// `operation_tx_outs_indexing` — the OperationTx path: base inputs consumed,
/// each op's input UTXOs deleted, and base `outs` produced at indices
/// `0..len(outs)`.
///
/// NOTE: op *output* index continuation (appending `op.outs()` after the base
/// `outs`, EXEC-AVM-1) cannot be exercised yet — `FxOperation` currently only
/// has the `Unsupported` placeholder (no concrete typed op carries outputs).
/// The op-output indexing assertion lands with the typed `FxOperation` variants
/// (secp/nft/property op type-ids), gated on the M5.5 codec wiring. This test
/// covers everything the executor can do for OperationTx today.
#[test]
fn operation_tx_outs_indexing() {
    // Two base outputs (indices 0, 1); one op whose input UTXO is consumed.
    let mut diff = diff_with_utxos(&[(0x20, 0, 1000)]);

    // Seed the op input UTXO directly into the diff (asset_id = asset_id()).
    let op_utxo = Utxo {
        tx_id: Id::from([0x30u8; 32]),
        output_index: 0,
        asset_id: asset_id(),
        out: Output::SecpTransfer(TransferOutput::new(50, owners())),
    };
    diff.add_utxo(op_utxo.input_id(), op_utxo.marshal().expect("marshal"));

    let op = Operation {
        asset: Asset::new(asset_id()),
        utxo_ids: vec![UtxoId::new(Id::from([0x30u8; 32]), 0)],
        op: FxOperation::Unsupported(Vec::new()),
        fx_id: Id::EMPTY,
    };

    let op_tx = OperationTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_out(400), transfer_out(300)],
            ins: vec![transfer_in(0x20, 0, 1000)],
            memo: Vec::new(),
        }),
        ops: vec![op],
    };

    let mut tx = Tx::new(UnsignedTx::Operation(op_tx));
    tx.initialize(Codec()).expect("initialize");
    let tx_id = tx.id();

    let result = Executor::execute(&tx.unsigned, tx_id, &mut diff).expect("execute");
    assert!(result.atomic_requests.is_empty());

    // The base input UTXO is deleted.
    assert!(diff.get_utxo(input_id(0x20, 0)).is_err());

    // The op input UTXO is deleted.
    let op_utxo_id = Id::from([0x30u8; 32]).prefix(&[0u64]);
    assert!(diff.get_utxo(op_utxo_id).is_err());

    // Base outs at indices 0, 1.
    assert!(diff.get_utxo(tx_id.prefix(&[0u64])).is_ok());
    assert!(diff.get_utxo(tx_id.prefix(&[1u64])).is_ok());
}

// ---------------------------------------------------------------------------
// ImportTx: builds Requests { remove: [input_ids] } keyed by source_chain
// ---------------------------------------------------------------------------

/// `import_builds_remove_requests` — each imported input's `input_id` appears in
/// `Requests { remove }` keyed by `source_chain`.
#[test]
fn import_builds_remove_requests() {
    let mut diff = diff_with_utxos(&[]);

    let import = ImportTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_out(900)],
            ins: Vec::new(),
            memo: Vec::new(),
        }),
        source_chain: source_chain(),
        imported_ins: vec![transfer_in(0x40, 0, 500), transfer_in(0x41, 0, 400)],
    };

    let mut tx = Tx::new(UnsignedTx::Import(import));
    tx.initialize(Codec()).expect("initialize");
    let tx_id = tx.id();

    let result = Executor::execute(&tx.unsigned, tx_id, &mut diff).expect("execute");

    // One entry keyed by source_chain.
    assert_eq!(result.atomic_requests.len(), 1);
    let req = result
        .atomic_requests
        .get(&source_chain())
        .expect("source_chain key missing");

    // Two remove entries, no put entries.
    assert_eq!(req.put.len(), 0, "no puts for import");
    assert_eq!(req.remove.len(), 2);

    let expected_0 = input_id(0x40, 0).to_bytes().to_vec();
    let expected_1 = input_id(0x41, 0).to_bytes().to_vec();
    assert!(
        req.remove.contains(&expected_0),
        "imported[0] input_id missing from remove"
    );
    assert!(
        req.remove.contains(&expected_1),
        "imported[1] input_id missing from remove"
    );

    // The consumed shared-memory input ids are recorded in `result.inputs`.
    assert_eq!(result.inputs.len(), 2);
    assert!(result.inputs.contains(&input_id(0x40, 0)));
    assert!(result.inputs.contains(&input_id(0x41, 0)));
}

// ---------------------------------------------------------------------------
// ExportTx: builds Requests { put: [Element{key,value,traits}] } for dest
// ---------------------------------------------------------------------------

/// `export_builds_put_requests` — exported outputs produce `Element`s keyed by
/// `destination_chain`; output_index continues from `len(outs)` (EXEC-AVM-1).
/// `Element.key` = `input_id` bytes, `Element.value` = marshaled UTXO bytes,
/// `Element.traits` = owner addresses.
#[test]
fn export_builds_put_requests() {
    let mut diff = diff_with_utxos(&[(0x50, 0, 1500)]);

    let export = ExportTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_out(700)], // index 0
            ins: vec![transfer_in(0x50, 0, 1500)],
            memo: Vec::new(),
        }),
        destination_chain: dest_chain(),
        exported_outs: vec![
            transfer_out(400), // index 1
            transfer_out(300), // index 2
        ],
    };

    let mut tx = Tx::new(UnsignedTx::Export(export));
    tx.initialize(Codec()).expect("initialize");
    let tx_id = tx.id();

    let result = Executor::execute(&tx.unsigned, tx_id, &mut diff).expect("execute");

    // One entry keyed by dest_chain.
    assert_eq!(result.atomic_requests.len(), 1);
    let req = result
        .atomic_requests
        .get(&dest_chain())
        .expect("dest_chain key missing");

    assert_eq!(req.remove.len(), 0, "no removes for export");
    assert_eq!(req.put.len(), 2, "two exported outputs");

    // Element at put[0] → output_index 1, Element at put[1] → output_index 2.
    for (i, elem) in req.put.iter().enumerate() {
        let output_index = 1u32 + i as u32;
        let expected_uid = tx_id.prefix(&[u64::from(output_index)]);
        let expected_key = expected_uid.to_bytes().to_vec();
        assert_eq!(
            elem.key, expected_key,
            "Element[{i}].key wrong (output_index {output_index})"
        );

        // Unmarshal the value and verify tx_id/output_index/asset_id.
        let utxo = Utxo::unmarshal(&elem.value).expect("unmarshal element value");
        assert_eq!(utxo.tx_id, tx_id);
        assert_eq!(utxo.output_index, output_index);
        assert_eq!(utxo.asset_id, asset_id());

        // Traits must be non-empty (owner addresses).
        assert!(!elem.traits.is_empty(), "traits should carry addresses");
        let expected_addr = addr().as_bytes().to_vec();
        assert!(
            elem.traits.contains(&expected_addr),
            "owner address missing from traits"
        );
    }

    // The input UTXO is consumed from local state.
    assert!(diff.get_utxo(input_id(0x50, 0)).is_err());
}
