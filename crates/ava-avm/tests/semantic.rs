// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Stateful `SemanticVerifier` tests over the five X-Chain tx types (M5.13).
//!
//! Spec: `specs/09-avm-xchain.md` §6.2 (SemanticVerify + verify_fx_usage +
//! GRANDFATHERED_OPERATION_TX + SameSubnet); 07 §3.1 (SharedMemory). Go
//! reference: `../avalanchego/vms/avm/txs/executor/semantic_verifier.go`.
//!
//! Each case seeds the chain state (input UTXOs + the asset's `CreateAssetTx`),
//! builds a tx, runs `SemanticVerifier::verify`, and asserts the exact `Ok(())`
//! or avm `Error` sentinel (`assert_matches!`), mirroring Go's `errors.Is`.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;

use assert_matches::assert_matches;

use ava_avm::error::Error;
use ava_avm::fx::dispatch::Dispatch;
use ava_avm::state::{Chain, State};
use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{
    Asset, AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput, UtxoId,
};
use ava_avm::txs::executor::semantic::{SemanticVerifier, SubnetResolver, Utxo};
use ava_avm::txs::executor::{Backend, Config, GRANDFATHERED_OPERATION_TX};
use ava_avm::txs::{
    BaseTx, CreateAssetTx, ExportTx, FxCredential, ImportTx, InitialState, Operation, OperationTx,
    Tx, UnsignedTx,
};
use ava_database::MemDb;
use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;
use ava_vm::components::avax::shared_memory::{IndexedResult, Requests, SharedMemory};

const NETWORK_ID: u32 = 10;
const TX_FEE: u64 = 1000;
const CREATE_ASSET_TX_FEE: u64 = 2000;
const NUM_FXS: usize = 3;

fn chain_id() -> Id {
    Id::from([0x05; 32])
}

fn subnet_id() -> Id {
    Id::from([0x07; 32])
}

fn source_chain() -> Id {
    Id::from([0x09; 32])
}

fn addr() -> ShortId {
    ShortId::from([0xab; 20])
}

fn owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![addr()])
}

fn backend(bootstrapped: bool) -> Backend {
    Backend::new(
        NETWORK_ID,
        chain_id(),
        Config::new(TX_FEE, CREATE_ASSET_TX_FEE),
        Id::EMPTY,
        NUM_FXS,
        bootstrapped,
    )
}

/// A `SubnetResolver` that maps every peer chain into `subnet_id`, so
/// `SameSubnet` passes (the local chain is in `subnet_id` too).
struct SameSubnetResolver;
impl SubnetResolver for SameSubnetResolver {
    fn get_subnet_id(&self, _chain: Id) -> Result<Id, Error> {
        Ok(subnet_id())
    }
}

/// A fake `SharedMemory` returning canned UTXO bytes for any key set, in order.
struct FakeSharedMemory {
    values: Vec<Vec<u8>>,
}
impl SharedMemory for FakeSharedMemory {
    fn get(&self, _peer_chain: Id, keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, ava_vm::error::Error> {
        assert_eq!(keys.len(), self.values.len());
        Ok(self.values.clone())
    }
    fn indexed(
        &self,
        _peer_chain: Id,
        _traits: &[Vec<u8>],
        _start_trait: &[u8],
        _start_key: &[u8],
        _limit: usize,
    ) -> Result<IndexedResult, ava_vm::error::Error> {
        unimplemented!()
    }
    fn apply(
        &self,
        _requests: std::collections::BTreeMap<Id, Requests>,
        _batches: &[ava_database::BatchOps],
    ) -> Result<(), ava_vm::error::Error> {
        unimplemented!()
    }
}

fn dispatch() -> Dispatch {
    Dispatch::new(
        Id::EMPTY,
        Id::from([1u8; 32]),
        Id::from([2u8; 32]),
        Arc::new(MockClock::default()),
    )
}

/// A `CreateAssetTx` defining a single asset enabling `fx_index`; its `tx_id`
/// becomes the asset id used by the spending tx.
fn create_asset_tx(fx_index: u32) -> Tx {
    let ca = CreateAssetTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: Vec::new(),
            ins: Vec::new(),
            memo: Vec::new(),
        }),
        name: "Asset".to_string(),
        symbol: "MYA".to_string(),
        denomination: 8,
        states: vec![InitialState::new(
            fx_index,
            vec![Output::SecpTransfer(TransferOutput::new(1, owners()))],
        )],
    };
    let mut tx = Tx::new(UnsignedTx::CreateAsset(ca));
    tx.initialize(Codec()).expect("initialize create-asset");
    tx
}

/// A non-CreateAsset tx (to trigger `NotAnAsset`).
fn base_only_tx() -> Tx {
    let mut tx = Tx::new(UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: Vec::new(),
        ins: Vec::new(),
        memo: Vec::new(),
    })));
    tx.initialize(Codec()).expect("initialize base");
    tx
}

fn transfer_output(asset_id: Id, amt: u64) -> TransferableOutput {
    TransferableOutput {
        asset_id,
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    }
}

fn transfer_input(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> TransferableInput {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    TransferableInput {
        tx_id: Id::from(tx_id),
        output_index: idx,
        asset_id,
        r#in: Input::SecpTransfer(TransferInput::new(amt, vec![0])),
    }
}

fn secp_credential() -> FxCredential {
    FxCredential::new(Id::EMPTY, SecpCredential::new(vec![[0u8; 65]]))
}

/// Marshals a `Utxo` carrying `asset_id` + a transfer output of `amt` at
/// `(tx_byte, idx)` into its canonical avm codec bytes.
fn utxo_bytes(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> (Id, Vec<u8>) {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    let utxo = Utxo {
        tx_id: Id::from(tx_id),
        output_index: idx,
        asset_id,
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    };
    (utxo.input_id(), utxo.marshal().expect("marshal utxo"))
}

fn signed(unsigned: UnsignedTx, num_creds: usize) -> Tx {
    let mut tx = Tx::new(unsigned);
    tx.creds = (0..num_creds).map(|_| secp_credential()).collect();
    tx.initialize(Codec()).expect("initialize tx");
    tx
}

/// Seeds a fresh `State` with the asset's `CreateAssetTx` and returns
/// `(state, asset_id)`.
fn seed_asset(fx_index: u32) -> (State<MemDb>, Id) {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");
    let ca = create_asset_tx(fx_index);
    let asset_id = ca.id();
    state.add_tx(asset_id, ca.bytes().to_vec());
    (state, asset_id)
}

// ---------------------------------------------------------------------------
// BaseTx
// ---------------------------------------------------------------------------

#[test]
fn base_tx_spends_known_utxo() {
    let (mut state, asset_id) = seed_asset(0);
    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 2000);
    state.add_utxo(utxo_id, bytes);

    let base = AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![transfer_output(asset_id, 1000)],
        ins: vec![transfer_input(0xaa, 0, asset_id, 2000)],
        memo: Vec::new(),
    };
    let tx = signed(UnsignedTx::Base(BaseTx::new(base)), 1);

    let b = backend(true);
    let fxs = dispatch();
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id);
    assert_matches!(v.verify(), Ok(()));
}

#[test]
fn asset_id_mismatch() {
    let (mut state, asset_id) = seed_asset(0);
    // UTXO holds `asset_id`, but the input claims a different asset.
    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 2000);
    state.add_utxo(utxo_id, bytes);

    let other_asset = Id::from([0xee; 32]);
    let base = AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![transfer_output(other_asset, 1000)],
        ins: vec![transfer_input(0xaa, 0, other_asset, 2000)],
        memo: Vec::new(),
    };
    let tx = signed(UnsignedTx::Base(BaseTx::new(base)), 1);

    let b = backend(true);
    let fxs = dispatch();
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id);
    assert_matches!(v.verify(), Err(Error::AssetIdMismatch));
}

#[test]
fn incompatible_fx() {
    // The asset enables only fx index 2 (property), but the spend uses the secp
    // credential routed to fx index 0.
    let (mut state, asset_id) = seed_asset(2);
    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 2000);
    state.add_utxo(utxo_id, bytes);

    let base = AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![transfer_output(asset_id, 1000)],
        ins: vec![transfer_input(0xaa, 0, asset_id, 2000)],
        memo: Vec::new(),
    };
    let tx = signed(UnsignedTx::Base(BaseTx::new(base)), 1);

    let b = backend(true);
    let fxs = dispatch();
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id);
    assert_matches!(v.verify(), Err(Error::IncompatibleFx));
}

#[test]
fn not_an_asset() {
    // Store a non-CreateAsset tx under the asset id.
    let base_db = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base_db)).expect("state");
    let not_asset = base_only_tx();
    let asset_id = not_asset.id();
    state.add_tx(asset_id, not_asset.bytes().to_vec());

    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 2000);
    state.add_utxo(utxo_id, bytes);

    let base = AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![transfer_output(asset_id, 1000)],
        ins: vec![transfer_input(0xaa, 0, asset_id, 2000)],
        memo: Vec::new(),
    };
    let tx = signed(UnsignedTx::Base(BaseTx::new(base)), 1);

    let b = backend(true);
    let fxs = dispatch();
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id);
    assert_matches!(v.verify(), Err(Error::NotAnAsset));
}

// ---------------------------------------------------------------------------
// OperationTx
// ---------------------------------------------------------------------------

#[test]
fn operation_tx_cred_index() {
    // An OperationTx with one base input and one op. The op's credential index is
    // `len(ins) + op_index = 1`. The op's fx-operation is the M5.5 placeholder,
    // which is unroutable; the bootstrapped, non-grandfathered path must reach the
    // op (proving the cred-index offset executed) and surface `UnknownFx`.
    let (mut state, asset_id) = seed_asset(0);
    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 2000);
    state.add_utxo(utxo_id, bytes);

    // The op's consumed UTXO id; `op_utxo_ref` matches the `utxo_bytes` builder
    // (tx_id with byte 0 = 0xbb, rest zero) so the op-input fetch succeeds.
    let mut op_ref = [0u8; 32];
    op_ref[0] = 0xbb;
    let op = Operation {
        asset: Asset::new(asset_id),
        utxo_ids: vec![UtxoId::new(Id::from(op_ref), 0)],
        op: ava_avm::txs::FxOperation::Unsupported(Vec::new()),
        fx_id: Id::EMPTY,
    };
    let op_tx = OperationTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xaa, 0, asset_id, 2000)],
            memo: Vec::new(),
        }),
        ops: vec![op],
    };
    // numInputs = 1 base in + 1 op = 2 creds.
    let tx = signed(UnsignedTx::Operation(op_tx), 2);

    // Seed the op's input UTXO so the op fetch succeeds and the verifier reaches
    // fx routing on the (unroutable) placeholder op.
    let (op_utxo_id, op_bytes) = utxo_bytes(0xbb, 0, asset_id, 5);
    state.add_utxo(op_utxo_id, op_bytes);

    let b = backend(true);
    let fxs = dispatch();
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id);
    assert_matches!(v.verify(), Err(Error::UnknownFx));
}

#[test]
fn grandfathered_op_skips_verification() {
    let (mut state, asset_id) = seed_asset(0);
    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 2000);
    state.add_utxo(utxo_id, bytes);

    let op = Operation {
        asset: Asset::new(asset_id),
        utxo_ids: vec![UtxoId::new(Id::from([0xbb; 32]), 0)],
        op: ava_avm::txs::FxOperation::Unsupported(Vec::new()),
        fx_id: Id::EMPTY,
    };
    let op_tx = OperationTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xaa, 0, asset_id, 2000)],
            memo: Vec::new(),
        }),
        ops: vec![op],
    };
    let mut tx = Tx::new(UnsignedTx::Operation(op_tx));
    tx.creds = vec![secp_credential(), secp_credential()];
    tx.initialize(Codec()).expect("initialize");
    // Force the tx id to the grandfathered constant; op verification must be
    // skipped exactly as Go does (it never reaches the unroutable placeholder op).
    tx.tx_id = GRANDFATHERED_OPERATION_TX.parse().expect("grandfather id");

    let b = backend(true);
    let fxs = dispatch();
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id);
    assert_matches!(v.verify(), Ok(()));
}

#[test]
fn not_bootstrapped_skips_op_verify() {
    let (mut state, asset_id) = seed_asset(0);
    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 2000);
    state.add_utxo(utxo_id, bytes);

    let op = Operation {
        asset: Asset::new(asset_id),
        utxo_ids: vec![UtxoId::new(Id::from([0xbb; 32]), 0)],
        op: ava_avm::txs::FxOperation::Unsupported(Vec::new()),
        fx_id: Id::EMPTY,
    };
    let op_tx = OperationTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xaa, 0, asset_id, 2000)],
            memo: Vec::new(),
        }),
        ops: vec![op],
    };
    let tx = signed(UnsignedTx::Operation(op_tx), 2);

    // !bootstrapped -> op verification skipped (and the base-tx fx transfer is
    // also skipped inside the not-bootstrapped secp fx).
    let b = backend(false);
    let fxs = dispatch();
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id);
    assert_matches!(v.verify(), Ok(()));
}

// ---------------------------------------------------------------------------
// ImportTx
// ---------------------------------------------------------------------------

#[test]
fn import_fetches_shared_memory() {
    let (mut state, asset_id) = seed_asset(0);
    // Local base input.
    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 1000);
    state.add_utxo(utxo_id, bytes);

    // The imported UTXO lives in shared memory, not local state.
    let (_imp_id, imp_bytes) = utxo_bytes(0xcc, 0, asset_id, 1000);

    let import = ImportTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xaa, 0, asset_id, 1000)],
            memo: Vec::new(),
        }),
        source_chain: source_chain(),
        imported_ins: vec![transfer_input(0xcc, 0, asset_id, 1000)],
    };
    let tx = signed(UnsignedTx::Import(import), 2);

    let b = backend(true);
    let fxs = dispatch();
    let sm = FakeSharedMemory {
        values: vec![imp_bytes],
    };
    let resolver = SameSubnetResolver;
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id)
        .with_shared_memory(&sm)
        .with_same_subnet(subnet_id(), &resolver);
    assert_matches!(v.verify(), Ok(()));
}

// ---------------------------------------------------------------------------
// ExportTx
// ---------------------------------------------------------------------------

#[test]
fn export_verifies_fx_usage() {
    let (mut state, asset_id) = seed_asset(0);
    let (utxo_id, bytes) = utxo_bytes(0xaa, 0, asset_id, 3000);
    state.add_utxo(utxo_id, bytes);

    let export = ExportTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xaa, 0, asset_id, 3000)],
            memo: Vec::new(),
        }),
        destination_chain: source_chain(),
        exported_outs: vec![transfer_output(asset_id, 1000)],
    };
    let tx = signed(UnsignedTx::Export(export), 1);

    let b = backend(true);
    let fxs = dispatch();
    let resolver = SameSubnetResolver;
    let v = SemanticVerifier::new(&b, &state, &tx, &fxs, asset_id)
        .with_same_subnet(subnet_id(), &resolver);
    assert_matches!(v.verify(), Ok(()));
}
