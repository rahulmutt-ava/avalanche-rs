// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Table-driven stateless `SyntacticVerifier` tests over all five X-Chain tx
//! types (M5.12).
//!
//! Spec: `specs/09-avm-xchain.md` §6.1, §3.3; TX-AVM-1; `specs/07` §3.1
//! FlowChecker. Go reference: `../avalanchego/vms/avm/txs/executor/
//! syntactic_verifier_test.go`.
//!
//! Each case builds a tx, runs `SyntacticVerifier::verify`, and asserts the
//! expected `Ok(())` or the exact avm `Error` sentinel (`assert_matches!`),
//! mirroring where Go uses `errors.Is`.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use assert_matches::assert_matches;

use ava_avm::error::Error;
use ava_avm::txs::components::{
    AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput, UtxoId,
};
use ava_avm::txs::executor::{Backend, Config, SyntacticVerifier};
use ava_avm::txs::{
    BaseTx, CreateAssetTx, ExportTx, FxCredential, ImportTx, InitialState, Operation, OperationTx,
    Tx, UnsignedTx,
};
use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

const NETWORK_ID: u32 = 10;
const TX_FEE: u64 = 1000;
const CREATE_ASSET_TX_FEE: u64 = 2000;
const NUM_FXS: usize = 3;

fn chain_id() -> Id {
    let mut b = [0u8; 32];
    b[..5].copy_from_slice(&[0x05, 0x04, 0x03, 0x02, 0x01]);
    Id::from(b)
}

fn fee_asset_id() -> Id {
    let mut b = [0u8; 32];
    b[..3].copy_from_slice(&[0x01, 0x02, 0x03]);
    Id::from(b)
}

fn addr() -> ShortId {
    ShortId::from([
        0xfc, 0xed, 0xa8, 0xf9, 0x0f, 0xcb, 0x5d, 0x30, 0x61, 0x4b, 0x99, 0xd7, 0x9f, 0xc4, 0xba,
        0xa2, 0x93, 0x07, 0x76, 0x26,
    ])
}

fn backend() -> Backend {
    Backend::new(
        NETWORK_ID,
        chain_id(),
        Config::new(TX_FEE, CREATE_ASSET_TX_FEE),
        fee_asset_id(),
        NUM_FXS,
        true,
    )
}

fn owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![addr()])
}

fn transfer_output(amt: u64) -> TransferableOutput {
    TransferableOutput {
        asset_id: fee_asset_id(),
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    }
}

/// A transferable input over the fee asset, referencing `(tx_id, idx)`.
fn transfer_input(tx_byte: u8, idx: u32, amt: u64) -> TransferableInput {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    TransferableInput {
        tx_id: Id::from(tx_id),
        output_index: idx,
        asset_id: fee_asset_id(),
        r#in: Input::SecpTransfer(TransferInput::new(amt, vec![0])),
    }
}

fn secp_credential() -> FxCredential {
    FxCredential::new(Id::EMPTY, SecpCredential::new(vec![[0u8; 65]]))
}

/// A valid `avax.BaseTx`: one input of `in_amt`, one output of `out_amt`,
/// correct network/chain id, empty memo. The caller picks the amounts so the
/// flow check (`in == out + fee`) holds.
fn ok_base(in_amt: u64, out_amt: u64) -> AvaxBaseTx {
    AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![transfer_output(out_amt)],
        ins: vec![transfer_input(0xaa, 0, in_amt)],
        memo: Vec::new(),
    }
}

/// Wraps an `UnsignedTx` plus a matching credential count into a signed `Tx`.
fn signed(unsigned: UnsignedTx, num_creds: usize) -> Tx {
    let mut tx = Tx::new(unsigned);
    tx.creds = (0..num_creds).map(|_| secp_credential()).collect();
    tx
}

fn verify(tx: &Tx) -> Result<(), Error> {
    let b = backend();
    SyntacticVerifier::new(&b, tx).verify()
}

// ---------------------------------------------------------------------------
// BaseTx
// ---------------------------------------------------------------------------

#[test]
fn base_tx_ok() {
    // in 2000 = out 1000 + fee 1000.
    let tx = signed(UnsignedTx::Base(BaseTx::new(ok_base(2000, 1000))), 1);
    assert_matches!(verify(&tx), Ok(()));
}

#[test]
fn memo_too_long() {
    let mut base = ok_base(2000, 1000);
    base.memo = vec![0u8; 257];
    let tx = signed(UnsignedTx::Base(BaseTx::new(base)), 1);
    assert_matches!(verify(&tx), Err(Error::MemoTooLarge));
}

#[test]
fn unsorted_outs() {
    // A definitively-unsorted pair: a larger-bytes output before a smaller one
    // (the secondary sort key is the marshaled output bytes, so a higher
    // locktime sorts strictly after a lower one).
    let mut base = ok_base(4000, 1000);
    let mut a = transfer_output(1000);
    let mut b = transfer_output(1000);
    if let Output::SecpTransfer(o) = &mut a.out {
        o.owners.locktime = 9;
    }
    if let Output::SecpTransfer(o) = &mut b.out {
        o.owners.locktime = 1;
    }
    base.outs = vec![a, b];
    let tx = signed(UnsignedTx::Base(BaseTx::new(base)), 1);
    assert_matches!(verify(&tx), Err(Error::OutputsNotSorted));
}

#[test]
fn num_creds_mismatch() {
    // One input, zero credentials.
    let tx = signed(UnsignedTx::Base(BaseTx::new(ok_base(2000, 1000))), 0);
    assert_matches!(verify(&tx), Err(Error::WrongNumberOfCredentials));
}

#[test]
fn wrong_network_id() {
    let mut base = ok_base(2000, 1000);
    base.network_id = NETWORK_ID + 1;
    let tx = signed(UnsignedTx::Base(BaseTx::new(base)), 1);
    assert_matches!(verify(&tx), Err(Error::WrongNetworkId));
}

// ---------------------------------------------------------------------------
// CreateAssetTx
// ---------------------------------------------------------------------------

fn ok_create_asset(name: &str, symbol: &str, denom: u8) -> CreateAssetTx {
    CreateAssetTx {
        base: BaseTx::new(ok_base(3000, 1000)), // in 3000 = out 1000 + create fee 2000.
        name: name.to_string(),
        symbol: symbol.to_string(),
        denomination: denom,
        states: vec![InitialState::new(
            0,
            vec![Output::SecpTransfer(TransferOutput::new(1, owners()))],
        )],
    }
}

#[test]
fn create_asset_ok() {
    let tx = signed(
        UnsignedTx::CreateAsset(ok_create_asset("My Asset", "MYA", 8)),
        1,
    );
    assert_matches!(verify(&tx), Ok(()));
}

#[test]
fn create_asset_name_empty() {
    let tx = signed(UnsignedTx::CreateAsset(ok_create_asset("", "MYA", 8)), 1);
    assert_matches!(verify(&tx), Err(Error::NameTooShort));
}

#[test]
fn create_asset_name_too_long() {
    let name = "a".repeat(129);
    let tx = signed(UnsignedTx::CreateAsset(ok_create_asset(&name, "MYA", 8)), 1);
    assert_matches!(verify(&tx), Err(Error::NameTooLong));
}

#[test]
fn create_asset_name_leading_ws() {
    let tx = signed(
        UnsignedTx::CreateAsset(ok_create_asset(" Asset", "MYA", 8)),
        1,
    );
    assert_matches!(verify(&tx), Err(Error::UnexpectedWhitespace));
}

#[test]
fn create_asset_name_non_ascii() {
    let tx = signed(
        UnsignedTx::CreateAsset(ok_create_asset("Ässet", "MYA", 8)),
        1,
    );
    assert_matches!(verify(&tx), Err(Error::IllegalNameCharacter));
}

#[test]
fn create_asset_symbol_too_long() {
    let tx = signed(
        UnsignedTx::CreateAsset(ok_create_asset("Asset", "TOOLONG", 8)),
        1,
    );
    assert_matches!(verify(&tx), Err(Error::SymbolTooLong));
}

#[test]
fn create_asset_symbol_lowercase() {
    let tx = signed(
        UnsignedTx::CreateAsset(ok_create_asset("Asset", "mya", 8)),
        1,
    );
    assert_matches!(verify(&tx), Err(Error::IllegalSymbolCharacter));
}

#[test]
fn create_asset_denomination_gt_32() {
    let tx = signed(
        UnsignedTx::CreateAsset(ok_create_asset("Asset", "MYA", 33)),
        1,
    );
    assert_matches!(verify(&tx), Err(Error::DenominationTooLarge));
}

#[test]
fn create_asset_states_empty() {
    let mut ca = ok_create_asset("Asset", "MYA", 8);
    ca.states = Vec::new();
    let tx = signed(UnsignedTx::CreateAsset(ca), 1);
    assert_matches!(verify(&tx), Err(Error::NoFxs));
}

#[test]
fn create_asset_states_unsorted() {
    let mut ca = ok_create_asset("Asset", "MYA", 8);
    let out = vec![Output::SecpTransfer(TransferOutput::new(1, owners()))];
    // fx_index 1 before 0 -> not strictly increasing.
    ca.states = vec![InitialState::new(1, out.clone()), InitialState::new(0, out)];
    let tx = signed(UnsignedTx::CreateAsset(ca), 1);
    assert_matches!(verify(&tx), Err(Error::InitialStatesNotSortedUnique));
}

#[test]
fn create_asset_state_unknown_fx() {
    let mut ca = ok_create_asset("Asset", "MYA", 8);
    ca.states = vec![InitialState::new(
        NUM_FXS as u32,
        vec![Output::SecpTransfer(TransferOutput::new(1, owners()))],
    )];
    let tx = signed(UnsignedTx::CreateAsset(ca), 1);
    assert_matches!(verify(&tx), Err(Error::UnknownFx));
}

// ---------------------------------------------------------------------------
// OperationTx
// ---------------------------------------------------------------------------

#[test]
fn operation_tx_empty_ops() {
    let op_tx = OperationTx {
        base: BaseTx::new(ok_base(2000, 1000)),
        ops: Vec::new(),
    };
    let tx = signed(UnsignedTx::Operation(op_tx), 1);
    assert_matches!(verify(&tx), Err(Error::NoOperations));
}

#[test]
fn op_utxo_collides_base_in() {
    // The base input spends (0xaa, 0); an op references the same utxo id.
    let op = Operation {
        asset: ava_avm::txs::components::Asset::new(fee_asset_id()),
        utxo_ids: vec![UtxoId::new(
            {
                let mut b = [0u8; 32];
                b[0] = 0xaa;
                Id::from(b)
            },
            0,
        )],
        op: ava_avm::txs::FxOperation::Unsupported(Vec::new()),
        fx_id: Id::EMPTY,
    };
    let op_tx = OperationTx {
        base: BaseTx::new(ok_base(2000, 1000)),
        ops: vec![op],
    };
    // numInputs = 1 base in + 1 op = 2 creds.
    let tx = signed(UnsignedTx::Operation(op_tx), 2);
    assert_matches!(verify(&tx), Err(Error::DoubleSpend));
}

// ---------------------------------------------------------------------------
// ImportTx
// ---------------------------------------------------------------------------

#[test]
fn import_no_inputs() {
    let import = ImportTx {
        base: BaseTx::new(ok_base(2000, 1000)),
        source_chain: chain_id(),
        imported_ins: Vec::new(),
    };
    let tx = signed(UnsignedTx::Import(import), 1);
    assert_matches!(verify(&tx), Err(Error::NoImportInputs));
}

#[test]
fn import_ok() {
    // base in 1000 + imported in 1000 = out 1000 + fee 1000.
    let import = ImportTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(1000)],
            ins: vec![transfer_input(0xaa, 0, 1000)],
            memo: Vec::new(),
        }),
        source_chain: chain_id(),
        imported_ins: vec![transfer_input(0xbb, 0, 1000)],
    };
    // numInputs = 1 base + 1 imported = 2 creds.
    let tx = signed(UnsignedTx::Import(import), 2);
    assert_matches!(verify(&tx), Ok(()));
}

// ---------------------------------------------------------------------------
// ExportTx
// ---------------------------------------------------------------------------

#[test]
fn export_no_outs() {
    let export = ExportTx {
        base: BaseTx::new(ok_base(2000, 1000)),
        destination_chain: chain_id(),
        exported_outs: Vec::new(),
    };
    let tx = signed(UnsignedTx::Export(export), 1);
    assert_matches!(verify(&tx), Err(Error::NoExportOutputs));
}

#[test]
fn export_ok() {
    // in 3000 = base out 1000 + exported out 1000 + fee 1000.
    let export = ExportTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(1000)],
            ins: vec![transfer_input(0xaa, 0, 3000)],
            memo: Vec::new(),
        }),
        destination_chain: chain_id(),
        exported_outs: vec![transfer_output(1000)],
    };
    let tx = signed(UnsignedTx::Export(export), 1);
    assert_matches!(verify(&tx), Ok(()));
}
