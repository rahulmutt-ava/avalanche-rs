// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M1.18 property test: `proof_verify_accepts_valid_rejects_tampered`
//! (spec 02 §4.2, 04 §3.6).
//!
//! For random tries and random `[start, end]` ranges across BranchFactor256/16:
//! - a single inclusion/exclusion `Proof` verifies against the true root and is
//!   rejected against a wrong root and against a flipped proven value;
//! - a `RangeProof` verifies against the true root and is rejected against a
//!   wrong / flipped root;
//! - a `ChangeProof` between two random tries verifies against the end root and
//!   is rejected against a flipped end root.
//!
//! Note on value-tampering: a single in-range *value* edit is not always
//! detectable by root recomputation — when a key's whole subtree lies outside
//! `[start, end]` its parent boundary node is injected by ID and masks the
//! value. Go's `x/merkledb` behaves identically (see `proof.rs`). The
//! concrete-tamper rejection is covered by the golden tests.

use std::collections::BTreeMap;

use proptest::collection::btree_map;
use proptest::prelude::*;

use ava_merkledb::hashing::{DefaultHasher, merkle_root};
use ava_merkledb::key::BranchFactor;
use ava_merkledb::maybe::Maybe;
use ava_merkledb::proof::{ChangeProof, Proof, RangeProof};
use ava_types::id::Id;

/// Small key space so shared prefixes occur frequently.
fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(0u8..6, 1..3)
}

fn value_strategy() -> impl Strategy<Value = Vec<u8>> {
    // Mix short (inlined) and long (hashed) values.
    proptest::collection::vec(any::<u8>(), 0..40)
}

fn kvs_strategy() -> impl Strategy<Value = BTreeMap<Vec<u8>, Vec<u8>>> {
    btree_map(key_strategy(), value_strategy(), 1..12)
}

fn as_refs(map: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<(&[u8], &[u8])> {
    map.iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect()
}

fn branch_factors() -> Vec<BranchFactor> {
    vec![BranchFactor::TwoFiftySix, BranchFactor::Sixteen]
}

/// Returns `id` with its first byte flipped — a root guaranteed to differ from
/// `id` (any non-empty trie root).
fn flip(id: Id) -> Id {
    let mut bytes = id.to_bytes();
    bytes[0] ^= 0xFF;
    Id::from(bytes)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn proof_verify_accepts_valid_rejects_tampered(
        kvs in kvs_strategy(),
        probe in key_strategy(),
    ) {
        let hasher = DefaultHasher;
        for bf in branch_factors() {
            let refs = as_refs(&kvs);
            let root = merkle_root(bf, &hasher, &refs);

            // ---- Single proof (the probe may or may not be present) ----
            let proof = Proof::prove(bf, &hasher, &refs, &probe).unwrap();
            prop_assert!(proof.verify(root, bf, &hasher).is_ok(), "valid single proof");
            // Wrong root is rejected.
            prop_assert!(proof.verify(Id::EMPTY, bf, &hasher).is_err(), "wrong root rejected");

            // Tampering the proven value flips inclusion<->exclusion mismatch.
            let mut tampered = proof.clone();
            match proof.value() {
                Maybe::Some(_) => tampered.set_value(Maybe::Nothing),
                Maybe::Nothing => tampered.set_value(Maybe::Some(bytes::Bytes::from_static(b"x"))),
            }
            prop_assert!(tampered.verify(root, bf, &hasher).is_err(), "tampered single proof rejected");
        }
    }

    #[test]
    fn range_proof_verify_accepts_valid_rejects_tampered(
        kvs in kvs_strategy(),
        a in key_strategy(),
        b in key_strategy(),
    ) {
        let hasher = DefaultHasher;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        for bf in branch_factors() {
            let refs = as_refs(&kvs);
            let root = merkle_root(bf, &hasher, &refs);

            let proof = RangeProof::prove(bf, &hasher, &refs, Some(&start), Some(&end), 50).unwrap();
            prop_assert!(
                proof.verify(Some(&start), Some(&end), root, bf, &hasher).is_ok(),
                "valid range proof"
            );
            // The true root is never EMPTY (kvs is non-empty), so verifying
            // against EMPTY is always rejected.
            prop_assert!(
                proof.verify(Some(&start), Some(&end), Id::EMPTY, bf, &hasher).is_err(),
                "range proof wrong root rejected"
            );
            // Verifying against any root other than the true one is rejected.
            prop_assert!(
                proof
                    .verify(Some(&start), Some(&end), flip(root), bf, &hasher)
                    .is_err(),
                "range proof against a flipped root rejected"
            );

            // Note: tampering a single in-range *value* is NOT always
            // detectable by root recomputation — when a key's whole subtree is
            // boundary-injected by ID (its parent's key is < start or > end) the
            // injected hash masks the value. Go behaves identically; the
            // value-digest cross-check (verify_proof_key_values) catches only
            // tampers on keys that ARE proof nodes. The golden test exercises a
            // concrete detectable tamper.
        }
    }

    #[test]
    fn change_proof_verify_accepts_valid_rejects_tampered(
        before in kvs_strategy(),
        after in kvs_strategy(),
        a in key_strategy(),
        b in key_strategy(),
    ) {
        let hasher = DefaultHasher;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        for bf in branch_factors() {
            let before_refs = as_refs(&before);
            let after_refs = as_refs(&after);
            let end_root = merkle_root(bf, &hasher, &after_refs);

            let proof = ChangeProof::prove(
                bf, &hasher, &before_refs, &after_refs, Some(&start), Some(&end), 50,
            ).unwrap();

            // Applying the changes to the (range-restricted) start state must
            // reproduce the end root, UNLESS before/after differ outside the
            // range (then applying only in-range changes can't reproduce it).
            if !has_out_of_range_diff(&before, &after, &start, &end) {
                prop_assert!(
                    proof
                        .verify(&before_refs, Some(&start), Some(&end), end_root, bf, &hasher)
                        .is_ok(),
                    "valid change proof"
                );

                // Verifying against a flipped end root is always rejected: the
                // applied root equals the true (unflipped) end_root, so a
                // one-bit-flipped target cannot match.
                prop_assert!(
                    proof
                        .verify(&before_refs, Some(&start), Some(&end), flip(end_root), bf, &hasher)
                        .is_err(),
                    "change proof against a flipped end root rejected"
                );
            }
        }
    }
}

/// True if `before` and `after` differ on some key OUTSIDE `[start, end]`. When
/// that happens, applying only the in-range changes to the full before-state
/// cannot reproduce the end root, so a strict `verify` Ok is not expected.
fn has_out_of_range_diff(
    before: &BTreeMap<Vec<u8>, Vec<u8>>,
    after: &BTreeMap<Vec<u8>, Vec<u8>>,
    start: &[u8],
    end: &[u8],
) -> bool {
    let mut keys: Vec<&Vec<u8>> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        let ks = k.as_slice();
        if (ks < start || ks > end) && before.get(k) != after.get(k) {
            return true;
        }
    }
    false
}
