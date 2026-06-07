// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M5.9 — `ava_avm::fx::dispatch` fx-routing tests (specs 09 §2.2, §4, FX-AVM-1).
//!
//! The avm verifier routes a parsed output / operation / credential to its
//! owning fx by looking up the value's codec type-id in the `TypeToFxIndex`
//! table (M5.5). These tests assert that, given a value of each concrete
//! output / credential / operation type, [`resolve_fx_index`] /
//! [`resolve_fx_index_of`] returns the right [`FxIndex`]:
//!
//! * secp output (typeID 7) → `Secp256k1` (0), nft output (10/11) → `Nft` (1),
//!   property output (15/16) → `Property` (2);
//! * credentials 9 / 14 / 19 → 0 / 1 / 2;
//! * an unregistered type-id → [`ava_avm::Error::UnknownFx`].

#![allow(unused_crate_dependencies, clippy::unwrap_used, clippy::expect_used)]

use assert_matches::assert_matches;

use ava_avm::Error;
use ava_avm::FxIndex;
use ava_avm::fx::dispatch::{FxValue, resolve_fx_index, resolve_fx_index_of};
use ava_avm::nftfx;
use ava_avm::propertyfx;
use ava_avm::txs::components::{Input, Output};
use ava_avm::txs::credential::Credential as AvmCredential;
use ava_secp256k1fx::{Credential as SecpCredential, MintOutput, TransferInput, TransferOutput};

#[test]
fn resolve_type_id_routes_each_fx() {
    // secp256k1fx (5–9) → Secp256k1.
    for id in 5u32..=9 {
        assert_eq!(resolve_fx_index(id).unwrap(), FxIndex::Secp256k1);
    }
    // nftfx (10–14) → Nft.
    for id in 10u32..=14 {
        assert_eq!(resolve_fx_index(id).unwrap(), FxIndex::Nft);
    }
    // propertyfx (15–19) → Property.
    for id in 15u32..=19 {
        assert_eq!(resolve_fx_index(id).unwrap(), FxIndex::Property);
    }
}

#[test]
fn resolve_unknown_type_id_is_unknown_fx() {
    // Tx types (0–4) and the block (20) are not fx types.
    for id in [0u32, 1, 2, 3, 4, 20, 21, 999] {
        assert_matches!(resolve_fx_index(id), Err(Error::UnknownFx));
    }
}

#[test]
fn resolve_secp_output_to_index_0() {
    let out = Output::SecpTransfer(TransferOutput::default());
    assert_eq!(out.fx_type_id(), 7);
    assert_eq!(resolve_fx_index_of(&out).unwrap(), FxIndex::Secp256k1);

    let mint = Output::SecpMint(MintOutput::default());
    assert_eq!(resolve_fx_index_of(&mint).unwrap(), FxIndex::Secp256k1);

    let input = Input::SecpTransfer(TransferInput::default());
    assert_eq!(input.fx_type_id(), 5);
    assert_eq!(resolve_fx_index_of(&input).unwrap(), FxIndex::Secp256k1);
}

#[test]
fn resolve_nft_output_to_index_1() {
    let mint = nftfx::NftOutput::Mint(nftfx::MintOutput::default());
    assert_eq!(mint.fx_type_id(), 10);
    assert_eq!(resolve_fx_index_of(&mint).unwrap(), FxIndex::Nft);

    let transfer = nftfx::NftOutput::Transfer(nftfx::TransferOutput::default());
    assert_eq!(transfer.fx_type_id(), 11);
    assert_eq!(resolve_fx_index_of(&transfer).unwrap(), FxIndex::Nft);

    let op_mint = nftfx::NftOperation::Mint(nftfx::MintOperation::default());
    assert_eq!(op_mint.fx_type_id(), 12);
    assert_eq!(resolve_fx_index_of(&op_mint).unwrap(), FxIndex::Nft);

    let op_transfer = nftfx::NftOperation::Transfer(nftfx::TransferOperation::default());
    assert_eq!(op_transfer.fx_type_id(), 13);
    assert_eq!(resolve_fx_index_of(&op_transfer).unwrap(), FxIndex::Nft);
}

#[test]
fn resolve_property_output_to_index_2() {
    let mint = propertyfx::PropertyUtxo::Mint(propertyfx::MintOutput::default());
    assert_eq!(mint.fx_type_id(), 15);
    assert_eq!(resolve_fx_index_of(&mint).unwrap(), FxIndex::Property);

    let owned = propertyfx::PropertyUtxo::Owned(propertyfx::OwnedOutput::default());
    assert_eq!(owned.fx_type_id(), 16);
    assert_eq!(resolve_fx_index_of(&owned).unwrap(), FxIndex::Property);

    let op_mint = propertyfx::PropertyOperation::Mint(propertyfx::MintOperation::default());
    assert_eq!(op_mint.fx_type_id(), 17);
    assert_eq!(resolve_fx_index_of(&op_mint).unwrap(), FxIndex::Property);

    let op_burn = propertyfx::PropertyOperation::Burn(propertyfx::BurnOperation::default());
    assert_eq!(op_burn.fx_type_id(), 18);
    assert_eq!(resolve_fx_index_of(&op_burn).unwrap(), FxIndex::Property);
}

#[test]
fn resolve_credentials_9_14_19() {
    // secp256k1fx.Credential (9) → 0.
    let secp = AvmCredential::Secp256k1(SecpCredential::default());
    assert_eq!(secp.fx_type_id(), 9);
    assert_eq!(resolve_fx_index_of(&secp).unwrap(), FxIndex::Secp256k1);

    // nftfx.Credential (14) → 1.
    let nft = nftfx::Credential::default();
    assert_eq!(nft.fx_type_id(), 14);
    assert_eq!(resolve_fx_index_of(&nft).unwrap(), FxIndex::Nft);

    // propertyfx.Credential (19) → 2.
    let property = propertyfx::Credential::default();
    assert_eq!(property.fx_type_id(), 19);
    assert_eq!(resolve_fx_index_of(&property).unwrap(), FxIndex::Property);
}
