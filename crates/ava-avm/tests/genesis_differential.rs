// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Go-oracle differential test for AVM genesis (M5.f4).
//!
//! The genesis bytes and the expected AVAX asset id were recorded from a real
//! Go avalanchego node (local network, network-id 12345) via an env-gated
//! emitter placed in `~/avalanchego` that replicated `genesis.FromConfig`'s
//! X-Chain block (`avm.NewGenesis(...).Bytes()` + `AVAXAssetID(bytes)`).
//! The hex vector is checked in at `tests/vectors/genesis/local.hex`.

#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ava_avm::genesis::Genesis;
use ava_avm::txs::codec::GenesisCodec;
use ava_avm::txs::{Tx, UnsignedTx};
use ava_types::id::Id;

/// AVM genesis bytes for the local network (network-id 12345), recorded from
/// the Go oracle (`genesis.FromConfig` X-Chain block, avalanchego local).
const LOCAL_GENESIS_HEX: &str = include_str!("vectors/genesis/local.hex");

/// Index-0 (AVAX) asset id recorded from the Go oracle.
const EXPECTED_AVAX_ASSET_ID_HEX: &str =
    "dbcf890f77f49b96857648b72b77f9f82937f28a68704af05da0dc12ba53f2db";

#[test]
fn rust_parse_matches_go_oracle_asset_id() {
    let bytes = hex::decode(LOCAL_GENESIS_HEX.trim()).expect("hex-decode genesis vector");
    let genesis = Genesis::parse(&bytes).expect("Genesis::parse");
    let asset = genesis
        .txs
        .into_iter()
        .next()
        .expect("at least one genesis asset");
    let mut tx = Tx::new(UnsignedTx::CreateAsset(asset.tx));
    tx.initialize(GenesisCodec()).expect("tx.initialize");

    let raw = hex::decode(EXPECTED_AVAX_ASSET_ID_HEX).expect("hex-decode expected id");
    let expected = Id::from_slice(&raw).expect("expected Id from slice");

    assert_eq!(
        tx.id(),
        expected,
        "AVAX asset id parity with Go oracle (local network)"
    );
}

#[test]
fn genesis_bytes_round_trip() {
    let bytes = hex::decode(LOCAL_GENESIS_HEX.trim()).expect("hex-decode genesis vector");
    let genesis = Genesis::parse(&bytes).expect("Genesis::parse");
    let re_encoded = genesis.marshal().expect("Genesis::marshal");
    assert_eq!(
        re_encoded, bytes,
        "genesis bytes round-trip: marshal(parse(bytes)) == bytes (byte-identical wire parity with Go)"
    );
}
