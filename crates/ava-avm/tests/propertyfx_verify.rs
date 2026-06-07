// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M5.8 — `ava_avm::propertyfx::Fx` operation verification tests (specs 09 §4.3,
//! FX-AVM-1).
//!
//! propertyfx mirrors Go `vms/propertyfx/fx.go`:
//!
//! * `verify_operation(Mint)` requires the consumed UTXO to be a `MintOutput`
//!   (else [`ava_avm::Error::WrongUtxoType`]), the produced `mint_output.owners`
//!   to equal the consumed UTXO's owners (else [`ava_avm::Error::WrongMintOutput`]),
//!   then runs the secp spend gate over the mint input;
//! * `verify_operation(Burn)` requires the consumed UTXO to be an `OwnedOutput`
//!   (else `WrongUtxoType`), then runs the secp spend gate over the burn input;
//! * `verify_transfer` is unsupported and always returns
//!   [`ava_avm::Error::CantTransfer`].

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use assert_matches::assert_matches;

use ava_avm::Error;
use ava_avm::propertyfx::{
    BurnOperation, Credential, Fx, MintOperation, MintOutput, OwnedOutput, PropertyOperation,
    PropertyUtxo,
};
use ava_crypto::secp256k1::PrivateKey;
use ava_secp256k1fx::{Credential as SecpCredential, Input, OutputOwners};
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;

/// Builds a bootstrapped `propertyfx::Fx` reading time from a mock clock at epoch.
fn fx_bootstrapped() -> Fx {
    let clock = MockClock::at(UNIX_EPOCH);
    let mut fx = Fx::new(Arc::new(clock));
    fx.bootstrapping();
    fx.bootstrapped();
    fx
}

/// Deterministic owner keys (RFC6979 signing matches Go bit-for-bit).
fn owner_keys(n: usize) -> Vec<PrivateKey> {
    (0..n)
        .map(|i| {
            let mut kb = [0u8; 32];
            kb[31] = (i as u8) + 1;
            kb[0] = 0xA0 + i as u8;
            PrivateKey::from_bytes(&kb).unwrap()
        })
        .collect()
}

/// The sorted-unique owner address set plus a sorted-index → key-index lookup.
fn sorted_owners(keys: &[PrivateKey]) -> (Vec<ShortId>, Vec<usize>) {
    let mut paired: Vec<(ShortId, usize)> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.public_key().address(), i))
        .collect();
    paired.sort_by_key(|(addr, _)| addr.to_bytes());
    let addrs = paired.iter().map(|(addr, _)| *addr).collect();
    let key_of_sorted = paired.iter().map(|(_, ki)| *ki).collect();
    (addrs, key_of_sorted)
}

/// Signs `tx`'s hash with the keys at the first `thr` sorted indices.
fn sign_first(
    tx: &[u8],
    keys: &[PrivateKey],
    key_of_sorted: &[usize],
    thr: u32,
) -> (Vec<u32>, Vec<[u8; 65]>) {
    let indices: Vec<u32> = (0..thr).collect();
    let sigs: Vec<[u8; 65]> = indices
        .iter()
        .map(|&idx| {
            keys[key_of_sorted[idx as usize]]
                .sign_hash(&ava_crypto::hashing::sha256(tx))
                .unwrap()
        })
        .collect();
    (indices, sigs)
}

/// `verify_transfer` is always disallowed for propertyfx (Go `errCantTransfer`).
#[test]
fn verify_transfer_disallowed() {
    let fx = fx_bootstrapped();
    assert_matches!(fx.verify_transfer(), Err(Error::CantTransfer));
}

/// A `MintOperation` whose produced `mint_output.owners` differ from the consumed
/// `MintOutput` UTXO's owners ⇒ `WrongMintOutput`.
#[test]
fn mint_owners_mismatch() {
    let keys = owner_keys(2);
    let (addrs, key_of_sorted) = sorted_owners(&keys);
    let thr = 1u32;
    let owner = OutputOwners::new(0, thr, addrs.clone());
    let utxo = MintOutput::new(owner.clone());

    // produced mint output owners differ (locktime differs) from the consumed UTXO.
    let other = OutputOwners::new(7, thr, addrs);
    let tx: Vec<u8> = b"mint-mismatch".to_vec();
    let (indices, sigs) = sign_first(&tx, &keys, &key_of_sorted, thr);
    let op = MintOperation::new(
        Input::new(indices),
        MintOutput::new(other),
        OwnedOutput::new(owner),
    );
    let cred = Credential::new(SecpCredential::new(sigs));

    let fx = fx_bootstrapped();
    assert_matches!(
        fx.verify_operation(
            &tx,
            &PropertyOperation::Mint(op),
            &cred,
            &PropertyUtxo::Mint(utxo)
        ),
        Err(Error::WrongMintOutput)
    );
}

/// A correct `MintOperation`: matching owners + threshold valid sigs ⇒ `Ok`.
#[test]
fn mint_happy_path() {
    let keys = owner_keys(3);
    let (addrs, key_of_sorted) = sorted_owners(&keys);
    let thr = 2u32;
    let owner = OutputOwners::new(0, thr, addrs);
    let utxo = MintOutput::new(owner.clone());

    let tx: Vec<u8> = b"mint-ok".to_vec();
    let (indices, sigs) = sign_first(&tx, &keys, &key_of_sorted, thr);
    let op = MintOperation::new(
        Input::new(indices),
        MintOutput::new(owner.clone()),
        OwnedOutput::new(owner),
    );
    let cred = Credential::new(SecpCredential::new(sigs));

    let fx = fx_bootstrapped();
    assert_matches!(
        fx.verify_operation(
            &tx,
            &PropertyOperation::Mint(op),
            &cred,
            &PropertyUtxo::Mint(utxo)
        ),
        Ok(())
    );
}

/// A correct `BurnOperation` over an `OwnedOutput`: threshold valid sigs ⇒ `Ok`.
#[test]
fn burn_happy_path() {
    let keys = owner_keys(2);
    let (addrs, key_of_sorted) = sorted_owners(&keys);
    let thr = 1u32;
    let owner = OutputOwners::new(0, thr, addrs);
    let utxo = OwnedOutput::new(owner);

    let tx: Vec<u8> = b"burn-ok".to_vec();
    let (indices, sigs) = sign_first(&tx, &keys, &key_of_sorted, thr);
    let op = BurnOperation::new(Input::new(indices));
    let cred = Credential::new(SecpCredential::new(sigs));

    let fx = fx_bootstrapped();
    assert_matches!(
        fx.verify_operation(
            &tx,
            &PropertyOperation::Burn(op),
            &cred,
            &PropertyUtxo::Owned(utxo)
        ),
        Ok(())
    );
}

/// A `MintOperation` against an `OwnedOutput` UTXO (wrong type) ⇒ `WrongUtxoType`;
/// likewise a `BurnOperation` against a `MintOutput` UTXO.
#[test]
fn wrong_utxo_type() {
    let keys = owner_keys(1);
    let (addrs, _) = sorted_owners(&keys);
    let owner = OutputOwners::new(0, 1, addrs);
    let tx: Vec<u8> = b"wrong-type".to_vec();
    let cred = Credential::new(SecpCredential::new(vec![[0u8; 65]]));

    let fx = fx_bootstrapped();

    // Mint op consuming an OwnedOutput UTXO ⇒ WrongUtxoType.
    let mint = MintOperation::new(
        Input::new(vec![0]),
        MintOutput::new(owner.clone()),
        OwnedOutput::new(owner.clone()),
    );
    assert_matches!(
        fx.verify_operation(
            &tx,
            &PropertyOperation::Mint(mint),
            &cred,
            &PropertyUtxo::Owned(OwnedOutput::new(owner.clone()))
        ),
        Err(Error::WrongUtxoType)
    );

    // Burn op consuming a MintOutput UTXO ⇒ WrongUtxoType.
    let burn = BurnOperation::new(Input::new(vec![0]));
    assert_matches!(
        fx.verify_operation(
            &tx,
            &PropertyOperation::Burn(burn),
            &cred,
            &PropertyUtxo::Mint(MintOutput::new(owner))
        ),
        Err(Error::WrongUtxoType)
    );
}
