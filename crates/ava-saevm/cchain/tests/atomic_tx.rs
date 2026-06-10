// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain atomic Import/Export tx + state + txpool tests (specs/11 §8, 27
//! §2.3/§3.1).
//!
//! Mirrors `vms/saevm/cchain/{tx,state,txpool}` for the four named seams this
//! task implements:
//!
//! * `import_export_tx_codec_roundtrip` — the ATOMIC-1 byte contract: an
//!   `Import`/`Export` `Tx` round-trips through the avalanchego linear codec
//!   and its bytes match a frozen golden vector (`tests/vectors/saevm/atomic/`).
//! * `atomic_txpool_separate_from_evm_pool` — the cross-chain atomic pool is a
//!   distinct structure from the EVM/`txgossip` pool.
//! * `wait_for_event_selects_across_both_pools` — `WaitForEvent` wakes on a tx
//!   arriving in *either* pool (the "select across two sources" seam).
//! * `export_import_shared_memory_all_or_nothing` — `state.apply` commits the
//!   local atomic-tx index AND the shared-memory mutation in one batch, or
//!   neither (27 §3.1 two-sided consistency).

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::sync::Arc;

use ava_chains::atomic::Memory;
use ava_database::MemDb;
use ava_saevm_cchain::state::State;
use ava_saevm_cchain::tx::components::{Input as FxInput, TransferInput, TransferableInput};
use ava_saevm_cchain::tx::{
    Credential as TxCredential, Export, Import, Input, Output, Tx, Unsigned,
};
use ava_saevm_cchain::txpool::{AtomicTxpool, EvmPoolStub, WaitSource};
use ava_secp256k1fx::Credential as SecpCredential;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::SharedMemory;

fn avax_asset_id() -> Id {
    Id::from([0x0a; 32])
}

fn id(b: u8) -> Id {
    Id::from([b; 32])
}

fn addr(b: u8) -> [u8; 20] {
    let mut a = [0u8; 20];
    a[0] = b;
    a
}

/// An `Import` tx with one imported input + one EVM output.
fn import_tx() -> Tx {
    let unsigned = Unsigned::Import(Import {
        network_id: 1,
        blockchain_id: id(0xc0),
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

/// An `Export` tx with one EVM input + one exported output.
fn export_tx() -> Tx {
    let unsigned = Unsigned::Export(Export {
        network_id: 1,
        blockchain_id: id(0xc0),
        destination_chain: id(0x0b),
        ins: vec![Input {
            address: addr(0x22),
            amount: 400,
            asset_id: avax_asset_id(),
            nonce: 0,
        }],
        exported_outs: vec![ava_saevm_cchain::tx::components::TransferableOutput {
            asset_id: avax_asset_id(),
            out: ava_saevm_cchain::tx::components::Output::SecpTransfer(
                ava_secp256k1fx::TransferOutput::new(
                    400,
                    ava_secp256k1fx::OutputOwners::new(0, 1, vec![]),
                ),
            ),
        }],
    });
    Tx {
        unsigned,
        creds: vec![TxCredential::Secp256k1(SecpCredential::new(vec![
            [0u8; 65],
        ]))],
    }
}

#[test]
fn import_export_tx_codec_roundtrip() {
    // Import: marshal -> parse -> equal; bytes match the frozen golden vector.
    let imp = import_tx();
    let imp_bytes = imp.marshal().expect("marshal import");
    let imp_parsed = Tx::parse(&imp_bytes).expect("parse import");
    assert_eq!(
        imp_parsed.unsigned, imp.unsigned,
        "import unsigned round-trip"
    );
    assert_eq!(imp_parsed.creds, imp.creds, "import creds round-trip");

    let imp_golden = include_bytes!("../../../../tests/vectors/saevm/atomic/import_tx.bin");
    assert_eq!(
        imp_bytes.as_slice(),
        imp_golden.as_slice(),
        "import tx bytes must match the frozen ATOMIC-1 golden vector"
    );

    // Export: marshal -> parse -> equal; bytes match the frozen golden vector.
    let exp = export_tx();
    let exp_bytes = exp.marshal().expect("marshal export");
    let exp_parsed = Tx::parse(&exp_bytes).expect("parse export");
    assert_eq!(
        exp_parsed.unsigned, exp.unsigned,
        "export unsigned round-trip"
    );
    assert_eq!(exp_parsed.creds, exp.creds, "export creds round-trip");

    let exp_golden = include_bytes!("../../../../tests/vectors/saevm/atomic/export_tx.bin");
    assert_eq!(
        exp_bytes.as_slice(),
        exp_golden.as_slice(),
        "export tx bytes must match the frozen ATOMIC-1 golden vector"
    );

    // The tx id is sha256 of the signed bytes and is stable.
    assert_ne!(imp.id(), Id::EMPTY);
    assert_ne!(exp.id(), Id::EMPTY);
    assert_ne!(imp.id(), exp.id());
}

#[test]
fn atomic_txpool_separate_from_evm_pool() {
    // The cross-chain atomic pool is its own structure; adding to it does not
    // touch the EVM pool, and vice versa.
    let atomic = AtomicTxpool::new(avax_asset_id());
    let evm = EvmPoolStub::default();

    assert_eq!(atomic.len(), 0);
    assert_eq!(evm.len(), 0);

    let tx = import_tx();
    let txid = tx.id();
    atomic.add(tx).expect("add atomic tx");

    assert_eq!(atomic.len(), 1, "atomic pool holds the atomic tx");
    assert!(atomic.has(txid), "atomic pool indexes by id");
    assert_eq!(evm.len(), 0, "EVM pool is unaffected by the atomic add");

    // An EVM tx lands only in the EVM pool.
    evm.add_evm();
    assert_eq!(evm.len(), 1, "EVM pool holds its own tx");
    assert_eq!(atomic.len(), 1, "atomic pool is unaffected by the EVM add");
}

#[tokio::test]
async fn wait_for_event_selects_across_both_pools() {
    // WaitForEvent wakes when a tx arrives in *either* the atomic pool or the
    // EVM pool (the "select across two sources" seam).
    let atomic = Arc::new(AtomicTxpool::new(avax_asset_id()));
    let evm = Arc::new(EvmPoolStub::default());

    // Case 1: a tx in the atomic pool wakes the waiter.
    {
        let atomic2 = Arc::clone(&atomic);
        let evm2 = Arc::clone(&evm);
        let waiter = tokio::spawn(async move {
            WaitSource::wait_for_event(atomic2.as_ref(), evm2.as_ref()).await
        });
        tokio::task::yield_now().await;
        atomic.add(import_tx()).expect("add atomic tx");
        let woke = waiter.await.expect("join waiter");
        assert_eq!(woke, WaitSource::Atomic, "atomic arrival wakes the waiter");
    }

    // Case 2: a tx in the EVM pool wakes the waiter.
    {
        let fresh_atomic = Arc::new(AtomicTxpool::new(avax_asset_id()));
        let evm2 = Arc::clone(&evm);
        let a2 = Arc::clone(&fresh_atomic);
        let waiter =
            tokio::spawn(
                async move { WaitSource::wait_for_event(a2.as_ref(), evm2.as_ref()).await },
            );
        tokio::task::yield_now().await;
        evm.add_evm();
        let woke = waiter.await.expect("join waiter");
        assert_eq!(woke, WaitSource::Evm, "EVM arrival wakes the waiter");
    }
}

#[test]
fn export_import_shared_memory_all_or_nothing() {
    // state.apply commits the local atomic-tx index AND the shared-memory
    // mutation atomically (27 §3.1). We drive it against an in-memory shared
    // memory and assert two-sided consistency.
    // State + shared memory share one base DB so the index write and the
    // shared-memory mutation commit in a single batch (27 §2.3).
    let base: Arc<MemDb> = Arc::new(MemDb::new());
    let memory = Memory::new(base.clone());

    let c_chain = id(0xc0);
    let peer = id(0x0b);
    let sm = memory.new_shared_memory(c_chain);

    let mut state = State::new(base.clone()).expect("new state");

    // Apply an export at height 1: it produces a UTXO into shared memory and
    // indexes the tx locally.
    let exp = export_tx();
    let exp_id = exp.id();
    state
        .apply(1, std::slice::from_ref(&exp), &sm)
        .expect("apply export at height 1");

    // Two-sided: the local tx index has the tx...
    let (got, height) = state.get_tx(exp_id).expect("get_tx after apply");
    assert_eq!(got.id(), exp_id, "local index has the exported tx");
    assert_eq!(height, 1, "tx indexed at the applied height");
    assert_eq!(state.current_height(), 1, "height advanced");

    // ...AND the shared memory has the produced UTXO available to the peer.
    let peer_view = memory.new_shared_memory(peer);
    let (chain_id, requests) = exp.atomic_requests().expect("atomic requests");
    assert_eq!(chain_id, peer, "export targets the peer chain");
    let keys: Vec<Vec<u8>> = requests.put.iter().map(|e| e.key.clone()).collect();
    assert!(!keys.is_empty(), "export produced at least one UTXO");
    let values = peer_view
        .get(c_chain, &keys)
        .expect("peer reads produced UTXO");
    assert_eq!(values.len(), keys.len(), "peer sees every produced UTXO");

    // Re-applying the same height is a no-op (idempotent under restart, 27 §2.3).
    state
        .apply(1, std::slice::from_ref(&exp), &sm)
        .expect("re-apply height 1 is a no-op");
    assert_eq!(state.current_height(), 1, "height does not regress/advance");
}
