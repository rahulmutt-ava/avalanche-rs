// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M5.6 — `ava_avm::fx::secp` verification wiring proptests (specs 09 §4, §4.1;
//! 07 §4.3).
//!
//! The avm-side secp fx adapter is a thin wrapper over
//! `ava_secp256k1fx::Fx::verify_credentials` (the multisig spend gate). These
//! tests assert:
//!
//! * `verify_transfer` accepts iff exactly `threshold` valid sorted-unique sigs
//!   from owner addresses are supplied and the locktime has matured, and rejects
//!   `utxo.amt != in.amt` with [`ava_avm::Error::MismatchedAmounts`];
//! * `verify_operation` enforces owners-equality between the produced mint output
//!   and the consumed `MintOutput` UTXO, then runs the spend gate over the mint
//!   input;
//! * both verifiers short-circuit to `Ok(())` when the fx is not bootstrapped
//!   (Go skips signature verification while replaying history).

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
use proptest::prelude::*;

use ava_avm::Error;
use ava_avm::fx::Fx;
use ava_avm::fx::secp::SecpFx;
use ava_crypto::secp256k1::PrivateKey;
use ava_secp256k1fx::{Credential, Input, MintOutput, OutputOwners, TransferInput, TransferOutput};
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;
use ava_vm::error::Error as FxErr;

/// Builds a `SecpFx` reading time from a mock clock at `unix`, optionally
/// transitioned past bootstrap.
fn fx_at(unix: u64, bootstrapped: bool) -> SecpFx {
    let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(unix));
    let mut fx = SecpFx::new(Arc::new(clock));
    fx.bootstrapping();
    if bootstrapped {
        fx.bootstrapped();
    }
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

/// Builds the sorted-unique owner address set + a lookup from sorted index → key
/// index.
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// `verify_transfer` accepts iff a correct exactly-threshold spend over a
    /// matured output with `utxo.amt == in.amt`; mismatched amounts reject with
    /// `MismatchedAmounts`, and the multisig sentinels surface from the gate.
    #[test]
    fn verify_transfer_accepts_iff_threshold_valid_sigs(
        num_owners in 1usize..6,
        threshold in 1u32..6,
        locktime in 0u64..1000,
        clock_unix in 0u64..1000,
        amt in 1u64..1_000_000,
        tx in proptest::collection::vec(any::<u8>(), 1..64),
    ) {
        let n = num_owners;
        let keys = owner_keys(n);
        let (addrs, key_of_sorted) = sorted_owners(&keys);
        prop_assume!(addrs.len() == n);

        let thr = threshold.min(n as u32);
        prop_assume!(thr >= 1);

        let owner = OutputOwners::new(locktime, thr, addrs);
        let utxo = TransferOutput::new(amt, owner);
        let matured = locktime <= clock_unix;

        let fx = fx_at(clock_unix, true);

        let (indices, sigs) = sign_first(&tx, &keys, &key_of_sorted, thr);
        let input = TransferInput::new(amt, indices.clone());
        let cred = Credential::new(sigs.clone());

        // --- correct, exactly-threshold spend ---
        let got = fx.verify_transfer(&tx, &input, &cred, &utxo);
        if matured {
            prop_assert!(got.is_ok(), "correct spend must accept: {got:?}");
        } else {
            prop_assert!(
                matches!(got, Err(Error::Fx(FxErr::Timelocked))),
                "premature: {got:?}"
            );
        }

        // --- amount mismatch (utxo.amt != in.amt) ⇒ MismatchedAmounts ---
        // The amount check runs before the spend gate, so it fires regardless of
        // locktime maturity.
        let bad_input = TransferInput::new(amt + 1, indices.clone());
        let r = fx.verify_transfer(&tx, &bad_input, &cred, &utxo);
        prop_assert!(matches!(r, Err(Error::MismatchedAmounts)), "amount mismatch: {r:?}");

        // --- too many signers ---
        if matured && (thr as usize) < n {
            let (over_idx, over_sigs) = sign_first(&tx, &keys, &key_of_sorted, thr + 1);
            let r = fx.verify_transfer(
                &tx,
                &TransferInput::new(amt, over_idx),
                &Credential::new(over_sigs),
                &utxo,
            );
            prop_assert!(
                matches!(r, Err(Error::Fx(FxErr::TooManySigners))),
                "over-sign: {r:?}"
            );
        }

        // --- wrong sig ---
        if matured {
            let mut bad = sigs.clone();
            bad.last_mut().unwrap()[0] ^= 0xFF;
            let r = fx.verify_transfer(&tx, &input, &Credential::new(bad), &utxo);
            prop_assert!(
                matches!(r, Err(Error::Fx(FxErr::WrongSig))),
                "wrong sig: {r:?}"
            );
        }
    }

    /// `verify_operation` requires the produced mint owners to equal the consumed
    /// `MintOutput` UTXO's owners, then runs the spend gate over the mint input.
    #[test]
    fn verify_operation_enforces_owner_equality_and_gate(
        num_owners in 1usize..6,
        threshold in 1u32..6,
        clock_unix in 0u64..1000,
        tx in proptest::collection::vec(any::<u8>(), 1..64),
    ) {
        let n = num_owners;
        let keys = owner_keys(n);
        let (addrs, key_of_sorted) = sorted_owners(&keys);
        prop_assume!(addrs.len() == n);

        let thr = threshold.min(n as u32);
        prop_assume!(thr >= 1);

        let owner = OutputOwners::new(0, thr, addrs.clone());
        let utxo = MintOutput::new(owner.clone());
        let mint_out = MintOutput::new(owner.clone());

        let fx = fx_at(clock_unix, true);

        let (indices, sigs) = sign_first(&tx, &keys, &key_of_sorted, thr);
        let mint_input = Input::new(indices);
        let cred = Credential::new(sigs);

        // --- matching owners + correct sigs ⇒ accept ---
        let got = fx.verify_operation(&tx, &mint_input, &mint_out, &cred, &utxo);
        prop_assert!(got.is_ok(), "matching owners + valid sigs: {got:?}");

        // --- mismatched owners ⇒ WrongMintCreated ---
        let other = OutputOwners::new(1, thr, addrs);
        let other_out = MintOutput::new(other);
        let r = fx.verify_operation(&tx, &mint_input, &other_out, &cred, &utxo);
        prop_assert!(matches!(r, Err(Error::WrongMintCreated)), "owner mismatch: {r:?}");
    }
}

/// When the fx is **not** bootstrapped, both verifiers skip signature checks and
/// return `Ok(())` even with garbage credentials (Go bootstrap-replay parity).
#[test]
fn verify_disabled_when_not_bootstrapped() {
    let fx = fx_at(0, false);
    let tx: Vec<u8> = b"tx".to_vec();
    let a = ShortId::from([1u8; 20]);
    let owner = OutputOwners::new(0, 1, vec![a]);
    // amount mismatch + garbage sig — both ignored while not bootstrapped.
    let utxo = TransferOutput::new(100, owner.clone());
    let input = TransferInput::new(999, vec![0]);
    let cred = Credential::new(vec![[0xFFu8; 65]]);

    assert_matches!(fx.verify_transfer(&tx, &input, &cred, &utxo), Ok(()));

    // mismatched owners (locktime differs) + garbage sig — both ignored too.
    let mint = MintOutput::new(owner);
    let other = MintOutput::new(OutputOwners::new(7, 1, vec![a]));
    assert_matches!(
        fx.verify_operation(&tx, &Input::new(vec![0]), &other, &cred, &mint),
        Ok(())
    );
}
