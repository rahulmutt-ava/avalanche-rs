// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain VM `Initialize` harness + `/avax` API tests (specs/11 §8 —
//! `cchain/{vm,api}.go`; §5 — the harness supplies the `Initialize` that
//! `sae::Vm` omits).
//!
//! Mirrors `vms/saevm/cchain/{vm_test,api_test}.go` for the two seams this task
//! implements:
//!
//! * `initialize_builds_genesis_hooks_sae_and_atomic_pool` — [`Vm::initialize`]
//!   constructs the genesis SAE block, builds the C-Chain hooks, composes
//!   [`ava_saevm_core::Vm`] (the `sae::Vm` analog), then the atomic txpool, and
//!   reports the genesis block as last-accepted (Go `vm.go::Initialize`).
//! * `avax_api_import_export_mounted_at_avax` — the `avax` JSON-RPC service is
//!   reachable at the `/avax` extension path alongside the SAE EVM RPC and
//!   responds to `issueTx` (Import/Export) + `getAtomicTx` in-process (Go
//!   `api.go::{IssueTx,GetAtomicTx}`, `vm.go::CreateHandlers`).

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::sync::Arc;

use ava_chains::atomic::Memory;
use ava_database::MemDb;
use ava_saevm_cchain::api::{AVAX_EXTENSION_PATH, AvaxService};
use ava_saevm_cchain::tx::components::{Input as FxInput, TransferInput};
use ava_saevm_cchain::tx::components::{Output as FxOutput, TransferableInput, TransferableOutput};
use ava_saevm_cchain::tx::{
    Credential as TxCredential, Export, Import, Input, Output, Tx, Unsigned,
};
use ava_saevm_cchain::vm::Vm;
use ava_secp256k1fx::Credential as SecpCredential;
use ava_types::id::Id;

fn avax_asset_id() -> Id {
    Id::from([0x0a; 32])
}

fn c_chain_id() -> Id {
    Id::from([0xc0; 32])
}

fn id(b: u8) -> Id {
    Id::from([b; 32])
}

fn addr(b: u8) -> [u8; 20] {
    let mut a = [0u8; 20];
    a[0] = b;
    a
}

fn import_tx() -> Tx {
    let unsigned = Unsigned::Import(Import {
        network_id: 1,
        blockchain_id: c_chain_id(),
        source_chain: id(0x0b),
        imported_ins: vec![TransferableInput {
            tx_id: id(0x33),
            output_index: 0,
            asset_id: avax_asset_id(),
            r#in: FxInput::SecpTransfer(TransferInput::new(1_000, vec![0])),
        }],
        outs: vec![Output {
            address: addr(0x11),
            amount: 1_000,
            asset_id: avax_asset_id(),
        }],
    });
    Tx {
        unsigned,
        creds: vec![TxCredential::Secp256k1(SecpCredential::new(vec![
            [0u8; 65],
        ]))],
    }
}

fn export_tx() -> Tx {
    let unsigned = Unsigned::Export(Export {
        network_id: 1,
        blockchain_id: c_chain_id(),
        destination_chain: id(0x0b),
        ins: vec![Input {
            address: addr(0x22),
            amount: 400,
            asset_id: avax_asset_id(),
            nonce: 0,
        }],
        exported_outs: vec![TransferableOutput {
            asset_id: avax_asset_id(),
            out: FxOutput::SecpTransfer(ava_secp256k1fx::TransferOutput::new(
                400,
                ava_secp256k1fx::OutputOwners::new(0, 1, vec![]),
            )),
        }],
    });
    Tx {
        unsigned,
        creds: vec![TxCredential::Secp256k1(SecpCredential::new(vec![
            [0u8; 65],
        ]))],
    }
}

/// A shared base DB + shared memory + atomic state, as the cchain VM and shared
/// memory MUST share an underlying database (Go `vm_test.go::newSUT`).
fn new_vm() -> Vm {
    let base: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let memory = Memory::new(Arc::clone(&base));
    let sm = memory.new_shared_memory(c_chain_id());
    Vm::initialize(&base, Arc::new(sm), c_chain_id(), avax_asset_id()).expect("initialize")
}

#[tokio::test]
async fn initialize_builds_genesis_hooks_sae_and_atomic_pool() {
    let vm = new_vm();

    // The genesis SAE block is the last-accepted block (height 0).
    let last = vm.last_accepted();
    let genesis = vm.core().block_by_id(last).expect("genesis block");
    assert_eq!(genesis.block().height(), 0, "genesis at height 0");

    // The atomic txpool was constructed over the chain's AVAX asset id and is
    // empty initially.
    assert_eq!(vm.atomic_txpool().avax_asset_id(), avax_asset_id());
    assert!(vm.atomic_txpool().is_empty(), "fresh atomic pool is empty");

    // The composed core VM can build on genesis (the hooks + sae::Vm are wired).
    let built = vm.core().build(None).expect("build on genesis");
    assert_eq!(built.block().height(), 1, "built block extends genesis");
}

#[tokio::test]
async fn avax_api_import_export_mounted_at_avax() {
    let vm = new_vm();

    // The avax service is mounted at the /avax extension path alongside the SAE
    // EVM RPC handlers (Go `vm.go::CreateHandlers`).
    let handlers = vm.create_handlers();
    assert!(
        handlers.contains_key(AVAX_EXTENSION_PATH),
        "avax service mounted at {AVAX_EXTENSION_PATH}"
    );

    let service: &AvaxService = vm.avax_service();

    // issueTx admits an Import tx into the atomic pool (Go `api.go::IssueTx`).
    let imp = import_tx();
    let imp_id = imp.id();
    let issued = service.issue_tx(&imp).expect("issue import tx");
    assert_eq!(issued, imp_id, "issueTx returns the tx id");
    assert!(vm.atomic_txpool().has(imp_id), "import tx is pooled");

    // issueTx admits an Export tx too.
    let exp = export_tx();
    let exp_id = exp.id();
    let issued = service.issue_tx(&exp).expect("issue export tx");
    assert_eq!(issued, exp_id, "issueTx returns the export tx id");
    assert!(vm.atomic_txpool().has(exp_id), "export tx is pooled");

    // Re-issuing the same tx is a no-op that still reports the id (Go ignores
    // ErrAlreadyKnown).
    let reissued = service.issue_tx(&imp).expect("re-issue import tx");
    assert_eq!(reissued, imp_id, "re-issue is idempotent");

    // A JSON request/response round-trips through the serde_json handler.
    let req = serde_json::json!({ "method": "avax.issueTx", "txID": exp_id.to_string() });
    let resp = service.handle(&req).expect("handle json request");
    assert_eq!(resp["txID"], serde_json::Value::String(exp_id.to_string()));
}
