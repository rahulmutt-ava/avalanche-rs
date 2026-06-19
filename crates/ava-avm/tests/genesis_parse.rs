// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-avm::genesis` — parse/marshal round-trip tests (M5.f4, specs 09 §1).
#![allow(unused_crate_dependencies)]

use ava_avm::genesis::{Genesis, GenesisAsset};
use ava_avm::txs::CreateAssetTx;

#[test]
fn genesis_marshal_parse_round_trips() {
    let g = Genesis {
        txs: vec![GenesisAsset {
            alias: "AVAX".to_string(),
            tx: CreateAssetTx {
                name: "Avalanche".to_string(),
                symbol: "AVAX".to_string(),
                denomination: 9,
                ..CreateAssetTx::default()
            },
        }],
    };
    let bytes = g.marshal().expect("Genesis::marshal");
    let back = Genesis::parse(&bytes).expect("Genesis::parse");
    assert_eq!(g, back, "Genesis round-trip");
}

#[test]
fn genesis_parse_rejects_truncated_bytes() {
    let err = Genesis::parse(&[0x00, 0x00]);
    assert!(err.is_err(), "truncated genesis bytes must error");
}
