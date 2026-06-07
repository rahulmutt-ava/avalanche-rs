// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M5.7 — `ava_avm::nftfx::Fx` operation verification tests (specs 09 §4.2,
//! FX-AVM-1).
//!
//! nftfx authorizes minting and transferring NFTs but **cannot** be used for a
//! plain transfer spend. These tests mirror `vms/nftfx/fx_test.go`:
//!
//! * `verify_transfer` always rejects with [`Error::CantTransfer`];
//! * a `MintOperation` whose `group_id` differs from the consumed `MintOutput`
//!   UTXO rejects with [`Error::WrongUniqueId`];
//! * a `TransferOperation` whose output `payload` differs from the consumed
//!   `TransferOutput` UTXO rejects with [`Error::WrongBytes`];
//! * mint + transfer happy paths accept (with valid sigs);
//! * a mismatched UTXO type (e.g. a `MintOperation` against a `TransferOutput`)
//!   rejects with [`Error::WrongUtxoType`].
//!
//! The signature/threshold check is delegated to the shared
//! `ava_secp256k1fx::Fx::verify_credentials` gate (reused via the avm secp fx),
//! so these tests run the fx **bootstrapped** with valid sigs.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use assert_matches::assert_matches;

use ava_avm::Error;
use ava_avm::nftfx::{
    Credential, Fx as NftFx, MintOperation, MintOutput, NftOperation, NftOutput, TransferOperation,
    TransferOutput,
};
use ava_crypto::secp256k1::PrivateKey;
use ava_secp256k1fx::types::{Credential as SecpCredential, Input, OutputOwners};
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;

/// Builds a bootstrapped `NftFx` reading time from a mock clock at `unix`.
fn fx_at(unix: u64) -> NftFx {
    let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(unix));
    let mut fx = NftFx::new(Arc::new(clock));
    fx.bootstrapping();
    fx.bootstrapped();
    fx
}

/// One deterministic owner key.
fn owner_key() -> PrivateKey {
    let mut kb = [0u8; 32];
    kb[31] = 1;
    kb[0] = 0xA0;
    PrivateKey::from_bytes(&kb).unwrap()
}

/// A single-signer (threshold 1, matured) owner over `key` and the matching
/// credential signing `tx`'s hash.
fn single_owner(key: &PrivateKey, tx: &[u8]) -> (OutputOwners, SecpCredential) {
    let owner = OutputOwners::new(0, 1, vec![key.public_key().address()]);
    let sig = key.sign_hash(&ava_crypto::hashing::sha256(tx)).unwrap();
    let cred = SecpCredential::new(vec![sig]);
    (owner, cred)
}

#[test]
fn verify_transfer_disallowed() {
    let fx = fx_at(0);
    let tx: Vec<u8> = b"tx".to_vec();
    let owner = OutputOwners::new(0, 1, vec![ShortId::from([1u8; 20])]);
    let utxo = TransferOutput::new(7, vec![0xAB], owner);
    let input = Input::new(vec![0]);
    let cred = Credential(SecpCredential::new(vec![[0u8; 65]]));
    assert_matches!(
        fx.verify_transfer(&tx, &input, &cred, &utxo),
        Err(Error::CantTransfer)
    );
}

#[test]
fn mint_group_id_mismatch() {
    let fx = fx_at(0);
    let tx: Vec<u8> = b"mint-tx".to_vec();
    let key = owner_key();
    let (owner, secp_cred) = single_owner(&key, &tx);

    // Consumed UTXO is for group 7; the operation mints into group 9.
    let utxo = MintOutput::new(7, owner.clone());
    let op = NftOperation::Mint(MintOperation {
        mint_input: Input::new(vec![0]),
        group_id: 9,
        payload: vec![1, 2, 3],
        outputs: vec![owner],
    });
    let cred = Credential(secp_cred);
    assert_matches!(
        fx.verify_operation(&tx, &op, &cred, &NftOutput::Mint(utxo)),
        Err(Error::WrongUniqueId)
    );
}

#[test]
fn transfer_payload_mismatch() {
    let fx = fx_at(0);
    let tx: Vec<u8> = b"transfer-tx".to_vec();
    let key = owner_key();
    let (owner, secp_cred) = single_owner(&key, &tx);

    // Consumed UTXO group 3 / payload [0xAA]; operation output keeps the group
    // but changes the payload ⇒ WrongBytes.
    let utxo = TransferOutput::new(3, vec![0xAA], owner.clone());
    let op = NftOperation::Transfer(TransferOperation {
        input: Input::new(vec![0]),
        output: TransferOutput::new(3, vec![0xBB], owner),
    });
    let cred = Credential(secp_cred);
    assert_matches!(
        fx.verify_operation(&tx, &op, &cred, &NftOutput::Transfer(utxo)),
        Err(Error::WrongBytes)
    );
}

#[test]
fn mint_happy_path() {
    let fx = fx_at(0);
    let tx: Vec<u8> = b"mint-tx".to_vec();
    let key = owner_key();
    let (owner, secp_cred) = single_owner(&key, &tx);

    let utxo = MintOutput::new(7, owner.clone());
    let op = NftOperation::Mint(MintOperation {
        mint_input: Input::new(vec![0]),
        group_id: 7,
        payload: vec![1, 2, 3],
        outputs: vec![owner],
    });
    let cred = Credential(secp_cred);
    assert_matches!(
        fx.verify_operation(&tx, &op, &cred, &NftOutput::Mint(utxo)),
        Ok(())
    );
}

#[test]
fn transfer_happy_path() {
    let fx = fx_at(0);
    let tx: Vec<u8> = b"transfer-tx".to_vec();
    let key = owner_key();
    let (owner, secp_cred) = single_owner(&key, &tx);

    let utxo = TransferOutput::new(3, vec![0xAA], owner.clone());
    let op = NftOperation::Transfer(TransferOperation {
        input: Input::new(vec![0]),
        output: TransferOutput::new(3, vec![0xAA], owner),
    });
    let cred = Credential(secp_cred);
    assert_matches!(
        fx.verify_operation(&tx, &op, &cred, &NftOutput::Transfer(utxo)),
        Ok(())
    );
}

#[test]
fn mint_wrong_utxo_type() {
    let fx = fx_at(0);
    let tx: Vec<u8> = b"mint-tx".to_vec();
    let key = owner_key();
    let (owner, secp_cred) = single_owner(&key, &tx);

    // A MintOperation against a TransferOutput UTXO ⇒ WrongUtxoType.
    let utxo = TransferOutput::new(7, vec![], owner.clone());
    let op = NftOperation::Mint(MintOperation {
        mint_input: Input::new(vec![0]),
        group_id: 7,
        payload: vec![],
        outputs: vec![owner],
    });
    let cred = Credential(secp_cred);
    assert_matches!(
        fx.verify_operation(&tx, &op, &cred, &NftOutput::Transfer(utxo)),
        Err(Error::WrongUtxoType)
    );
}
