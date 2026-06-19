// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `UnsignedTx::utxos(tx_id)` producer tests (M5.f4, Go `utxoGetter`).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs
)]

use ava_avm::txs::components::Output;
use ava_avm::txs::{CreateAssetTx, InitialState, UnsignedTx};
use ava_secp256k1fx::types::{OutputOwners, TransferOutput};
use ava_types::id::Id;

#[test]
fn create_asset_genesis_utxos_have_continuing_index_and_self_asset() {
    let tx_id = Id::from([7u8; 32]);
    let out0 = Output::SecpTransfer(TransferOutput::new(
        100,
        OutputOwners::new(0, 1, vec![[1u8; 20].into()]),
    ));
    let out1 = Output::SecpTransfer(TransferOutput::new(
        200,
        OutputOwners::new(0, 1, vec![[2u8; 20].into()]),
    ));
    let unsigned = UnsignedTx::CreateAsset(CreateAssetTx {
        name: "Avalanche".to_string(),
        symbol: "AVAX".to_string(),
        denomination: 9,
        states: vec![InitialState::new(0, vec![out0, out1])],
        ..CreateAssetTx::default()
    });

    let utxos = unsigned.utxos(tx_id);
    assert_eq!(utxos.len(), 2, "two genesis UTXOs");
    // base outs empty → indices start at 0 and continue.
    assert_eq!(utxos[0].output_index, 0);
    assert_eq!(utxos[1].output_index, 1);
    // asset id == tx id (the asset is itself).
    assert_eq!(utxos[0].asset_id, tx_id);
    assert_eq!(utxos[1].asset_id, tx_id);
    assert_eq!(utxos[0].tx_id, tx_id);
}
