// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Byte-exact golden-vector tests for the C-Chain atomic Import/Export tx codec
//! (M6.14, spec 10 §6.1). The vectors in
//! `tests/vectors/cchain/atomic/atomic_txs.json` are Go-EXECUTED against coreth
//! `plugin/evm/atomic` (see the sibling `_provenance.md`); this test asserts the
//! Rust linear-codec output is byte-identical.

use ava_avm::txs::components::{
    Input as FxInput, Output as FxOutput, TransferableInput, TransferableOutput,
};
use ava_codec::Serializable;
use ava_evm::atomic::tx::{
    AtomicTx, CODEC_VERSION, EvmInput, EvmOutput, UnsignedExportTx, UnsignedImportTx, codec,
};
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use serde_json::Value;

fn vectors() -> Value {
    let raw = include_str!("vectors/cchain/atomic/atomic_txs.json");
    serde_json::from_str(raw).expect("parse golden vectors")
}

fn id32(b: u8) -> Id {
    Id::from([b; 32])
}

fn marshal<T: Serializable>(v: &T) -> String {
    hex::encode(codec().marshal(CODEC_VERSION, v).expect("marshal"))
}

fn golden_import() -> UnsignedImportTx {
    UnsignedImportTx {
        network_id: 1,
        blockchain_id: id32(0x11),
        source_chain: id32(0x22),
        imported_inputs: vec![TransferableInput {
            tx_id: id32(0x44),
            output_index: 1,
            asset_id: id32(0xAA),
            r#in: FxInput::SecpTransfer(TransferInput::new(5000, vec![0])),
        }],
        outs: vec![EvmOutput {
            address: [0x01; 20],
            amount: 4999,
            asset_id: id32(0xAA),
        }],
    }
}

fn golden_export() -> UnsignedExportTx {
    UnsignedExportTx {
        network_id: 1,
        blockchain_id: id32(0x11),
        destination_chain: id32(0x33),
        ins: vec![EvmInput {
            address: [0x02; 20],
            amount: 3000,
            asset_id: id32(0xAA),
            nonce: 7,
        }],
        exported_outputs: vec![TransferableOutput {
            asset_id: id32(0xAA),
            out: FxOutput::SecpTransfer(TransferOutput {
                amt: 3000,
                owners: OutputOwners {
                    locktime: 0,
                    threshold: 1,
                    addrs: vec![ShortId::from([0x05; 20])],
                },
            }),
        }],
    }
}

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing {ptr}"))
        .to_string()
}

#[test]
fn evm_output_input_byte_exact() {
    let v = vectors();

    let out = EvmOutput {
        address: [0x01; 20],
        amount: 1000,
        asset_id: id32(0xAA),
    };
    assert_eq!(marshal(&out), s(&v, "/evm_output/codec_hex"));

    let evm_in = EvmInput {
        address: [0x02; 20],
        amount: 2000,
        asset_id: id32(0xAA),
        nonce: 7,
    };
    assert_eq!(marshal(&evm_in), s(&v, "/evm_input/codec_hex"));
}

#[test]
fn unsigned_import_export_byte_exact() {
    let v = vectors();

    // The bare-struct encoding (no interface type_id prefix) — matches Go's
    // `Codec.Marshal(0, &concretePtr)` golden dump.
    assert_eq!(
        marshal(&golden_import()),
        s(&v, "/unsigned_import_tx/struct_codec_hex")
    );
    assert_eq!(
        marshal(&golden_export()),
        s(&v, "/unsigned_export_tx/struct_codec_hex")
    );

    // The interface encoding (leading u32 type_id: 0 import / 1 export) — the form
    // the signed `Tx` envelope carries. It is the struct bytes with a 4-byte
    // type_id inserted right after the 2-byte version prefix.
    let import_struct = s(&v, "/unsigned_import_tx/struct_codec_hex");
    let import_iface = marshal(&AtomicTx::Import(golden_import()));
    assert_eq!(import_iface, splice_type_id(&import_struct, 0));

    let export_struct = s(&v, "/unsigned_export_tx/struct_codec_hex");
    let export_iface = marshal(&AtomicTx::Export(golden_export()));
    assert_eq!(export_iface, splice_type_id(&export_struct, 1));
}

/// Inserts a big-endian `u32` `type_id` (8 hex chars) right after the 2-byte
/// codec version prefix (4 hex chars) of a bare-struct hex encoding, yielding the
/// interface-framed encoding.
fn splice_type_id(struct_hex: &str, type_id: u32) -> String {
    let (version, body) = struct_hex.split_at(4);
    format!("{version}{type_id:08x}{body}")
}

#[test]
fn atomic_ops_match_go_vectors() {
    let v = vectors();

    // Import → RemoveRequests = utxoIDs on the source chain.
    let (chain, reqs) = golden_import().atomic_ops();
    assert_eq!(
        hex::encode(chain.to_bytes()),
        s(&v, "/import_atomic_ops/chain")
    );
    assert!(reqs.put.is_empty());
    assert_eq!(reqs.remove.len(), 1);
    assert_eq!(
        hex::encode(&reqs.remove[0]),
        s(&v, "/import_atomic_ops/remove_requests/0")
    );

    // Export → PutRequests = elems on the destination chain.
    let tx_id_hex = s(&v, "/export_tx_id");
    let tx_id = Id::from_slice(&hex::decode(&tx_id_hex).expect("decode tx id")).expect("tx id");
    let (chain, reqs) = golden_export()
        .atomic_ops(tx_id)
        .expect("export atomic ops");
    assert_eq!(
        hex::encode(chain.to_bytes()),
        s(&v, "/export_atomic_ops/chain")
    );
    assert!(reqs.remove.is_empty());
    assert_eq!(reqs.put.len(), 1);
    let elem = &reqs.put[0];
    assert_eq!(
        hex::encode(&elem.key),
        s(&v, "/export_atomic_ops/put_requests/0/key")
    );
    assert_eq!(
        hex::encode(&elem.value),
        s(&v, "/export_atomic_ops/put_requests/0/value")
    );
    assert_eq!(elem.traits.len(), 1);
    assert_eq!(
        hex::encode(&elem.traits[0]),
        s(&v, "/export_atomic_ops/put_requests/0/traits/0")
    );
}

#[test]
fn constants_match_go_vectors() {
    let v = vectors();
    let c = |ptr: &str| {
        v.pointer(ptr)
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("missing {ptr}"))
    };
    assert_eq!(ava_evm::atomic::tx::X2C_RATE, c("/constants/x2c_rate"));
    assert_eq!(
        ava_evm::atomic::tx::TX_BYTES_GAS,
        c("/constants/tx_bytes_gas")
    );
    assert_eq!(
        ava_evm::atomic::tx::EVM_OUTPUT_GAS,
        c("/constants/evm_output_gas")
    );
    assert_eq!(
        ava_evm::atomic::tx::EVM_INPUT_GAS,
        c("/constants/evm_input_gas")
    );
    assert_eq!(
        ava_evm::atomic::tx::COST_PER_SIGNATURE,
        c("/constants/cost_per_signature")
    );
}
