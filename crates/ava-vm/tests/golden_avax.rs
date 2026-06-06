// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::avax_*` — golden vectors for the avax UTXO model (specs 07 §3.1,
//! specs 02).
//!
//! Provenance:
//! * `avax_utxoid_derivation` — the `InputID` hashes are captured from the Go
//!   reference `ids.ID.Prefix(uint64(outputIndex))` over a fixed TxID
//!   (`0x00..0x1f`). The Go derivation matches `vms/components/avax/utxo_id.go`
//!   (`UTXOID.InputID`).
//! * `transferable_sort` — the comparators reproduce Go's
//!   `vms/components/avax/transferables.go`
//!   (`innerSortTransferableOutputs.Less` = `(assetID, codec(out) bytes)`;
//!   `TransferableInput.Compare` = UTXOID = `(txID, outputIndex)`).
//! * `flowchecker_balances` — the produce/consume ledger from
//!   `vms/components/avax/flow_checker.go` (`FlowChecker` + `VerifyTx`).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::sync::Arc;

use assert_matches::assert_matches;

use ava_snow::ChainContext;
use ava_types::id::Id;
use ava_vm::Result;
use ava_vm::components::avax::{
    Asset, FlowChecker, TransferableIn, TransferableInput, TransferableOut, TransferableOutput,
    UtxoId, is_sorted_and_unique_transferable_inputs, is_sorted_transferable_outputs,
    sort_transferable_inputs, sort_transferable_outputs, verify_tx,
};
use ava_vm::components::verify::{State, Verifiable};
use ava_vm::error::Error;

// ---- a minimal test fx output/input (stands in for secp256k1fx, M3.x) ----

/// A trivial fx output carrying an amount; `codec_bytes` is `be64(amount)` so the
/// secondary output sort key is deterministic and inspectable.
#[derive(Debug)]
struct TestOut {
    amount: u64,
}

impl Verifiable for TestOut {
    fn verify(&self) -> Result<()> {
        Ok(())
    }
}
impl State for TestOut {
    fn init_ctx(&self, _ctx: &ChainContext) {}
}
impl TransferableOut for TestOut {
    fn amount(&self) -> u64 {
        self.amount
    }
    fn codec_bytes(&self) -> Vec<u8> {
        self.amount.to_be_bytes().to_vec()
    }
}

#[derive(Debug)]
struct TestIn {
    amount: u64,
}

impl Verifiable for TestIn {
    fn verify(&self) -> Result<()> {
        Ok(())
    }
}
impl TransferableIn for TestIn {
    fn amount(&self) -> u64 {
        self.amount
    }
    fn cost(&self) -> Result<u64> {
        Ok(0)
    }
}

fn out(asset: Id, amount: u64) -> TransferableOutput {
    TransferableOutput {
        asset: Asset::new(asset),
        fx_id: Id::EMPTY,
        out: Arc::new(TestOut { amount }),
    }
}

fn input(tx_id: Id, idx: u32, asset: Id, amount: u64) -> TransferableInput {
    TransferableInput {
        utxo_id: UtxoId::new(tx_id, idx),
        asset: Asset::new(asset),
        fx_id: Id::EMPTY,
        r#in: Arc::new(TestIn { amount }),
    }
}

fn id_from_byte(b: u8) -> Id {
    Id::from([b; 32])
}

/// `golden::avax_utxoid_derivation` — `UtxoId::input_id() ==
/// TxID.prefix(be64(output_index))`, against Go-captured hashes.
#[test]
fn avax_utxoid_derivation() {
    // Fixed TxID = bytes 0x00..0x1f.
    let mut tx_bytes = [0u8; 32];
    for (i, b) in tx_bytes.iter_mut().enumerate() {
        *b = i as u8;
    }
    let tx_id = Id::from(tx_bytes);

    // (output_index, expected InputID bytes) — captured from Go `ids.ID.Prefix`.
    let golden: &[(u32, [u8; 32])] = &[
        (
            0,
            [
                0xf4, 0xb3, 0x29, 0x77, 0xb2, 0x59, 0xa8, 0x7e, 0x6f, 0x26, 0x4a, 0x88, 0x30, 0x3c,
                0xe6, 0x29, 0x61, 0xdc, 0xac, 0x49, 0x02, 0xc7, 0x39, 0xb8, 0x9c, 0xf9, 0xc1, 0x6b,
                0xcc, 0x95, 0x5e, 0xb6,
            ],
        ),
        (
            1,
            [
                0xfa, 0xb6, 0x9a, 0xe5, 0xa1, 0x69, 0x65, 0x3b, 0x31, 0x8a, 0x6e, 0x2e, 0x39, 0x27,
                0xfb, 0x96, 0xf6, 0xe7, 0x9f, 0xec, 0x0d, 0x6e, 0x20, 0xb9, 0x5d, 0xe2, 0x73, 0xc0,
                0x2b, 0x7f, 0x02, 0x00,
            ],
        ),
        (
            7,
            [
                0xff, 0x52, 0xb4, 0x3d, 0x0c, 0xbd, 0x66, 0xe4, 0x94, 0xe9, 0x91, 0x0b, 0xbd, 0x23,
                0x97, 0xbc, 0x89, 0x7e, 0xb6, 0xdf, 0xe9, 0x62, 0xbf, 0x2e, 0x72, 0x46, 0x84, 0xce,
                0x9f, 0x07, 0xcf, 0x13,
            ],
        ),
        (
            4_294_967_295,
            [
                0x86, 0x5d, 0xce, 0x7b, 0xdc, 0xfc, 0x29, 0x9a, 0xa2, 0xb5, 0xe5, 0x39, 0x78, 0xb9,
                0x7d, 0x55, 0x36, 0xdf, 0x90, 0xf4, 0xe5, 0xac, 0x51, 0xba, 0x08, 0x28, 0x57, 0xe6,
                0x98, 0x6c, 0x45, 0x71,
            ],
        ),
    ];

    for &(idx, expected_bytes) in golden {
        let utxo_id = UtxoId::new(tx_id, idx);
        let got = utxo_id.input_id();
        let expected = Id::from(expected_bytes);
        assert_eq!(got, expected, "InputID for output_index={idx}");

        // The formula matches `tx_id.prefix(&[idx as u64])`, and the value is
        // cached (a second call returns the same id).
        assert_eq!(got, tx_id.prefix(&[u64::from(idx)]));
        assert_eq!(utxo_id.input_id(), got, "cached InputID is stable");
    }
}

/// `golden::transferable_sort` — outputs by `(assetID, codec bytes)`, inputs by
/// UTXOID; both consensus-affecting.
#[test]
fn transferable_sort() {
    let asset_a = id_from_byte(0x01);
    let asset_b = id_from_byte(0x02);

    // Outputs: shuffle (asset, amount) pairs; expect sort by assetID then by
    // codec bytes (= be64(amount)).
    let mut outs = vec![
        out(asset_b, 5),
        out(asset_a, 9),
        out(asset_a, 3),
        out(asset_b, 1),
    ];
    assert!(!is_sorted_transferable_outputs(&outs));
    sort_transferable_outputs(&mut outs);
    assert!(is_sorted_transferable_outputs(&outs));
    // asset_a (0x01) before asset_b (0x02); within an asset, smaller amount
    // first (be64 is monotone).
    let order: Vec<(u8, u64)> = outs
        .iter()
        .map(|o| (o.asset.id.to_bytes()[0], o.out.amount()))
        .collect();
    assert_eq!(order, vec![(1, 3), (1, 9), (2, 1), (2, 5)]);

    // Inputs: sort by UTXOID = (txID, outputIndex).
    let tx1 = id_from_byte(0x10);
    let tx2 = id_from_byte(0x20);
    let mut ins = vec![
        input(tx2, 0, asset_a, 1),
        input(tx1, 5, asset_a, 1),
        input(tx1, 2, asset_a, 1),
    ];
    assert!(!is_sorted_and_unique_transferable_inputs(&ins));
    sort_transferable_inputs(&mut ins);
    assert!(is_sorted_and_unique_transferable_inputs(&ins));
    let in_order: Vec<(u8, u32)> = ins
        .iter()
        .map(|i| (i.utxo_id.tx_id.to_bytes()[0], i.utxo_id.output_index))
        .collect();
    assert_eq!(in_order, vec![(0x10, 2), (0x10, 5), (0x20, 0)]);

    // Duplicate UTXOID ⇒ not sorted-and-unique.
    let dup = vec![input(tx1, 2, asset_a, 1), input(tx1, 2, asset_a, 1)];
    assert!(!is_sorted_and_unique_transferable_inputs(&dup));
}

/// `golden::flowchecker_balances` — per-asset produce/consume ledger with checked
/// overflow.
#[test]
fn flowchecker_balances() {
    let asset_a = id_from_byte(0x01);
    let asset_b = id_from_byte(0x02);

    // Balanced: consumed >= produced for every asset.
    let mut fc = FlowChecker::new();
    fc.produce(asset_a, 100);
    fc.consume(asset_a, 100);
    fc.produce(asset_b, 5);
    fc.consume(asset_b, 10);
    fc.verify().expect("balanced");

    // Insufficient funds: produced > consumed for asset_a.
    let mut fc2 = FlowChecker::new();
    fc2.produce(asset_a, 100);
    fc2.consume(asset_a, 99);
    assert_matches!(fc2.verify(), Err(Error::InsufficientFunds));

    // Overflow on produce ⇒ Error::Overflow (sticky).
    let mut fc3 = FlowChecker::new();
    fc3.produce(asset_a, u64::MAX);
    fc3.produce(asset_a, 1);
    assert_matches!(fc3.verify(), Err(Error::Overflow));

    // verify_tx: a single balanced, sorted input/output set with a burned fee.
    let fee_asset = asset_a;
    let outs = vec![out(asset_a, 50)];
    // Inputs cover outputs (50) + fee (10) = 60.
    let ins = vec![input(id_from_byte(0x10), 0, asset_a, 60)];
    verify_tx(10, fee_asset, &[ins], &[outs]).expect("verify_tx balanced");

    // verify_tx: unsorted outputs ⇒ OutputsNotSorted.
    let bad_outs = vec![out(asset_b, 1), out(asset_a, 1)]; // asset_b before asset_a
    let some_ins = vec![input(id_from_byte(0x10), 0, asset_a, 100)];
    assert_matches!(
        verify_tx(0, fee_asset, &[some_ins], &[bad_outs]),
        Err(Error::OutputsNotSorted)
    );
}
