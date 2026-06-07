// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `prop::multisig_verify` + the Go-vector multisig cases + the
//! `output_owners_verify` table (specs 07 §4.2, §4.3, specs 02).
//!
//! Provenance: the `multisig` cases in `tests/vectors/secp256k1fx/vectors.json`
//! were generated against the pinned `../avalanchego` tree and validated by
//! driving the real `secp256k1fx.Fx.VerifyCredentials` (each case's expected
//! error was asserted in Go before capture). See `tests/PORTING.md`.

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
use serde::Deserialize;

use ava_crypto::secp256k1::PrivateKey;
use ava_secp256k1fx::{Credential, Error, Fx, Input, OutputOwners};
use ava_types::short_id::ShortId;
use ava_utils::clock::{Clock, MockClock};

// ---------------------------------------------------------------------------
// Go-derived multisig cases (real signatures captured from avalanchego)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct MultisigCase {
    name: String,
    locktime: u64,
    threshold: u32,
    addrs_hex: Vec<String>,
    sig_indices: Vec<u32>,
    sigs_hex: Vec<String>,
    clock_unix: u64,
    bootstrapped: bool,
    tx_bytes_hex: String,
    expect_err: String,
}

#[derive(Debug, Deserialize)]
struct Vectors {
    multisig: Vec<MultisigCase>,
}

fn load() -> Vectors {
    serde_json::from_str(include_str!("vectors/secp256k1fx/vectors.json")).expect("parse vectors")
}

fn fx_at(unix: u64, bootstrapped: bool) -> Fx {
    let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(unix));
    let mut fx = Fx::new(Arc::new(clock));
    fx.bootstrapping();
    if bootstrapped {
        fx.bootstrapped();
    }
    fx
}

#[test]
fn go_multisig_vectors() {
    for tc in load().multisig {
        let fx = fx_at(tc.clock_unix, tc.bootstrapped);
        let addrs: Vec<ShortId> = tc
            .addrs_hex
            .iter()
            .map(|h| ShortId::from_slice(&hex::decode(h).unwrap()).unwrap())
            .collect();
        let owner = OutputOwners::new(tc.locktime, tc.threshold, addrs);
        let input = Input::new(tc.sig_indices.clone());
        let sigs: Vec<[u8; 65]> = tc
            .sigs_hex
            .iter()
            .map(|h| {
                let b = hex::decode(h).unwrap();
                let mut s = [0u8; 65];
                s.copy_from_slice(&b);
                s
            })
            .collect();
        let cred = Credential::new(sigs);
        let tx = hex::decode(&tc.tx_bytes_hex).unwrap();

        let got = fx.verify_credentials(&tx, &input, &cred, &owner);
        match tc.expect_err.as_str() {
            "" => assert_matches!(got, Ok(()), "case {} should accept", tc.name),
            "ErrTimelocked" => {
                assert_matches!(got, Err(Error::Timelocked), "case {}", tc.name);
            }
            "ErrTooManySigners" => {
                assert_matches!(got, Err(Error::TooManySigners), "case {}", tc.name);
            }
            "ErrTooFewSigners" => {
                assert_matches!(got, Err(Error::TooFewSigners), "case {}", tc.name);
            }
            "ErrInputCredentialSignersMismatch" => {
                assert_matches!(
                    got,
                    Err(Error::InputCredentialSignersMismatch),
                    "case {}",
                    tc.name
                );
            }
            "ErrInputOutputIndexOutOfBounds" => {
                assert_matches!(
                    got,
                    Err(Error::InputOutputIndexOutOfBounds),
                    "case {}",
                    tc.name
                );
            }
            "ErrWrongSig" => assert_matches!(got, Err(Error::WrongSig), "case {}", tc.name),
            other => panic!("unhandled expected error {other}"),
        }
    }
}

// ---------------------------------------------------------------------------
// output_owners_verify table (07 §4.2)
// ---------------------------------------------------------------------------

#[test]
fn output_owners_verify_table() {
    use ava_vm::components::verify::Verifiable;

    let a = |b: u8| ShortId::from([b; 20]);

    // threshold > len(addrs) ⇒ OutputUnspendable
    assert_matches!(
        OutputOwners::new(0, 2, vec![a(1)]).verify(),
        Err(Error::OutputUnspendable)
    );
    // threshold == 0 && !addrs.is_empty() ⇒ OutputUnoptimized
    assert_matches!(
        OutputOwners::new(0, 0, vec![a(1)]).verify(),
        Err(Error::OutputUnoptimized)
    );
    // unsorted addrs ⇒ AddrsNotSortedUnique
    assert_matches!(
        OutputOwners::new(0, 1, vec![a(2), a(1)]).verify(),
        Err(Error::AddrsNotSortedUnique)
    );
    // duplicate addrs ⇒ AddrsNotSortedUnique
    assert_matches!(
        OutputOwners::new(0, 1, vec![a(1), a(1)]).verify(),
        Err(Error::AddrsNotSortedUnique)
    );
    // valid: sorted-unique, threshold <= len
    assert_matches!(OutputOwners::new(0, 1, vec![a(1), a(2)]).verify(), Ok(()));
    // valid: empty owners, zero threshold
    assert_matches!(OutputOwners::new(0, 0, vec![]).verify(), Ok(()));
}

// ---------------------------------------------------------------------------
// prop::multisig_verify — accept iff exactly `threshold` valid owner sigs at
// sorted-unique indices and matured locktime.
// ---------------------------------------------------------------------------

/// Deterministic owner keys (RFC6979 signing matches Go bit-for-bit).
fn owner_keys(n: usize) -> Vec<PrivateKey> {
    (0..n)
        .map(|i| {
            let mut kb = [0u8; 32];
            kb[31] = (i as u8) + 1; // small distinct nonzero scalars
            kb[0] = 0xA0 + i as u8;
            PrivateKey::from_bytes(&kb).unwrap()
        })
        .collect()
}

/// Builds the sorted-unique owner set + a lookup from index→key for the sorted
/// order.
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Correct, exactly-threshold signing at sorted-unique indices with matured
    /// locktime always accepts; mutations reject with the matching sentinel.
    #[test]
    fn multisig_verify(
        num_owners in 1usize..6,
        threshold in 1u32..6,
        locktime in 0u64..1000,
        clock_unix in 0u64..1000,
        tx in proptest::collection::vec(any::<u8>(), 1..64),
    ) {
        let n = num_owners;
        // owners must be unique; dedup by address by construction (distinct keys).
        let keys = owner_keys(n);
        let (addrs, key_of_sorted) = sorted_owners(&keys);
        prop_assume!(addrs.len() == n); // no accidental address collision

        let thr = threshold.min(n as u32);
        prop_assume!(thr >= 1);

        let owner = OutputOwners::new(locktime, thr, addrs.clone());

        let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(clock_unix));
        let mut fx = Fx::new(Arc::new(clock.clone()));
        fx.bootstrapping();
        fx.bootstrapped();

        let matured = locktime <= clock.unix();

        // pick the first `thr` sorted indices (sorted & unique) and sign correctly.
        let indices: Vec<u32> = (0..thr).collect();
        let sigs: Vec<[u8; 65]> = indices
            .iter()
            .map(|&idx| {
                let key = &keys[key_of_sorted[idx as usize]];
                key.sign_hash(&ava_crypto::hashing::sha256(&tx)).unwrap()
            })
            .collect();
        let input = Input::new(indices.clone());
        let cred = Credential::new(sigs.clone());

        let got = fx.verify_credentials(&tx, &input, &cred, &owner);
        if matured {
            prop_assert!(got.is_ok(), "correct exactly-threshold spend must accept: {got:?}");
        } else {
            prop_assert!(matches!(got, Err(Error::Timelocked)), "premature locktime must reject: {got:?}");
        }

        // --- over-sign (too many indices): TooManySigners ---
        if (thr as usize) < n {
            let over: Vec<u32> = (0..=thr).collect();
            let over_sigs: Vec<[u8;65]> = over.iter().map(|&idx| {
                keys[key_of_sorted[idx as usize]].sign_hash(&ava_crypto::hashing::sha256(&tx)).unwrap()
            }).collect();
            let r = fx.verify_credentials(&tx, &Input::new(over.clone()), &Credential::new(over_sigs), &owner);
            if matured {
                prop_assert!(matches!(r, Err(Error::TooManySigners)), "over-sign: {r:?}");
            } else {
                prop_assert!(matches!(r, Err(Error::Timelocked)));
            }
        }

        // --- under-sign (fewer indices): TooFewSigners ---
        if thr >= 2 {
            let under: Vec<u32> = (0..thr-1).collect();
            let under_sigs: Vec<[u8;65]> = under.iter().map(|&idx| {
                keys[key_of_sorted[idx as usize]].sign_hash(&ava_crypto::hashing::sha256(&tx)).unwrap()
            }).collect();
            let r = fx.verify_credentials(&tx, &Input::new(under.clone()), &Credential::new(under_sigs), &owner);
            if matured {
                prop_assert!(matches!(r, Err(Error::TooFewSigners)), "under-sign: {r:?}");
            } else {
                prop_assert!(matches!(r, Err(Error::Timelocked)));
            }
        }

        // --- reordered indices (descending): rejected (not sorted-unique input,
        //     but verify_credentials does not re-check sort; instead a reversed
        //     index pairs the wrong sig with the wrong addr ⇒ WrongSig) ---
        if matured && thr >= 2 {
            let mut rev = indices.clone();
            rev.reverse();
            // keep the sigs in original (ascending) order so position i no longer
            // matches addr at rev[i].
            let r = fx.verify_credentials(&tx, &Input::new(rev), &Credential::new(sigs.clone()), &owner);
            prop_assert!(matches!(r, Err(Error::WrongSig)), "reordered: {r:?}");
        }

        // --- OOB index: InputOutputIndexOutOfBounds ---
        if matured {
            let mut oob = indices.clone();
            *oob.last_mut().unwrap() = n as u32 + 10; // beyond addrs
            let r = fx.verify_credentials(&tx, &Input::new(oob), &cred, &owner);
            prop_assert!(matches!(r, Err(Error::InputOutputIndexOutOfBounds)), "oob: {r:?}");
        }

        // --- wrong sig (corrupt last sig's r): WrongSig ---
        if matured {
            let mut bad = sigs.clone();
            bad.last_mut().unwrap()[0] ^= 0xFF;
            let r = fx.verify_credentials(&tx, &input, &Credential::new(bad), &owner);
            prop_assert!(
                matches!(r, Err(Error::WrongSig)),
                "wrong sig: {r:?}"
            );
        }
    }
}
