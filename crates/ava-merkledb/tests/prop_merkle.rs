// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M1.16 property tests for the merkledb (spec 02 §4.2):
//!
//! - `merkle_order_independent_root` — a random K/V set yields the **same** root
//!   regardless of insertion order (no `HashMap` on the hashing path; 00 §6.1).
//! - `root(after delete-all) == ids::EMPTY`.
//! - `view_layering == direct application`.
//! - a `BTreeMap`-oracle: `get` after a random op sequence equals the oracle.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use proptest::collection::{btree_map, vec};
use proptest::prelude::*;

use ava_database::MemDb;
use ava_merkledb::{BatchOp, BranchFactor, DefaultHasher, MerkleDb, merkle_root};
use ava_types::id::Id;

/// A small key space so collisions/shared-prefixes occur frequently (more
/// stressful for the trie structure than fully-random long keys).
fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    vec(0u8..8, 1..4)
}

fn value_strategy() -> impl Strategy<Value = Vec<u8>> {
    // Mix short (< 32-byte, inlined digest) and long (hashed digest) values.
    vec(any::<u8>(), 0..40)
}

fn kvs_strategy() -> impl Strategy<Value = BTreeMap<Vec<u8>, Vec<u8>>> {
    btree_map(key_strategy(), value_strategy(), 0..16)
}

/// Builds a DB, commits each (k, v) as its own view in the given `order`, and
/// returns the resulting root.
fn db_root_in_order(kvs: &BTreeMap<Vec<u8>, Vec<u8>>, order: &[usize]) -> Id {
    let base = Arc::new(MemDb::new());
    let db = MerkleDb::new(base, BranchFactor::TwoFiftySix).unwrap();
    let entries: Vec<(&Vec<u8>, &Vec<u8>)> = kvs.iter().collect();
    for &i in order {
        let (k, v) = entries[i];
        let view = db.new_view(vec![BatchOp::put(k, v)]).unwrap();
        view.commit().unwrap();
    }
    db.get_merkle_root().unwrap()
}

proptest! {
    /// Inserting a K/V set in any permutation yields the same root.
    #[test]
    fn merkle_order_independent_root(kvs in kvs_strategy(), seed in any::<u64>()) {
        // Reference root: the order-independent in-memory builder.
        let refs: Vec<(&[u8], &[u8])> =
            kvs.iter().map(|(k, v)| (k.as_slice(), v.as_slice())).collect();
        let expected = merkle_root(BranchFactor::TwoFiftySix, &DefaultHasher, &refs);

        // Ascending order.
        let ascending: Vec<usize> = (0..kvs.len()).collect();
        prop_assert_eq!(db_root_in_order(&kvs, &ascending), expected);

        // A pseudo-random permutation derived from `seed`.
        let mut perm = ascending.clone();
        // Fisher-Yates with a tiny xorshift PRNG (no external rng dep).
        let mut state = seed | 1;
        for i in (1..perm.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let j = (state as usize) % (i + 1);
            perm.swap(i, j);
        }
        prop_assert_eq!(db_root_in_order(&kvs, &perm), expected);
    }

    /// Deleting every key returns the trie to the empty root.
    #[test]
    fn delete_all_yields_empty_root(kvs in kvs_strategy()) {
        let base = Arc::new(MemDb::new());
        let db = MerkleDb::new(base, BranchFactor::TwoFiftySix).unwrap();

        // Insert everything.
        let puts: Vec<BatchOp> =
            kvs.iter().map(|(k, v)| BatchOp::put(k, v)).collect();
        if !puts.is_empty() {
            db.new_view(puts).unwrap().commit().unwrap();
        }

        // Delete everything.
        let dels: Vec<BatchOp> = kvs.keys().map(|k| BatchOp::delete(k)).collect();
        if !dels.is_empty() {
            db.new_view(dels).unwrap().commit().unwrap();
        }

        prop_assert_eq!(db.get_merkle_root().unwrap(), Id::EMPTY);
    }

    /// A layered view's root equals the direct application of the merged set.
    #[test]
    fn view_layering_equals_direct(
        base_kvs in kvs_strategy(),
        overlay in kvs_strategy(),
    ) {
        let base = Arc::new(MemDb::new());
        let db = MerkleDb::new(base, BranchFactor::TwoFiftySix).unwrap();

        // Commit the base set to the DB.
        let base_puts: Vec<BatchOp> =
            base_kvs.iter().map(|(k, v)| BatchOp::put(k, v)).collect();
        if !base_puts.is_empty() {
            db.new_view(base_puts).unwrap().commit().unwrap();
        }

        // Layer an overlay view (puts) without committing.
        let overlay_puts: Vec<BatchOp> =
            overlay.iter().map(|(k, v)| BatchOp::put(k, v)).collect();
        let view = db.new_view(overlay_puts).unwrap();
        let view_root = view.get_merkle_root().unwrap();

        // The merged set (overlay wins on conflict).
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = base_kvs.clone();
        for (k, v) in &overlay {
            merged.insert(k.clone(), v.clone());
        }
        let refs: Vec<(&[u8], &[u8])> =
            merged.iter().map(|(k, v)| (k.as_slice(), v.as_slice())).collect();
        let direct = merkle_root(BranchFactor::TwoFiftySix, &DefaultHasher, &refs);

        prop_assert_eq!(view_root, direct);
    }

    /// `get` after a random op sequence equals a `BTreeMap` oracle.
    #[test]
    fn get_matches_btreemap_oracle(
        ops in vec((key_strategy(), value_strategy(), any::<bool>()), 0..40),
    ) {
        let base = Arc::new(MemDb::new());
        let db = MerkleDb::new(base, BranchFactor::TwoFiftySix).unwrap();
        let mut oracle: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();

        for (k, v, is_delete) in &ops {
            let op = if *is_delete {
                oracle.remove(k);
                BatchOp::delete(k)
            } else {
                oracle.insert(k.clone(), v.clone());
                BatchOp::put(k, v)
            };
            db.new_view(vec![op]).unwrap().commit().unwrap();
        }

        // Every oracle key reads back its value.
        for (k, v) in &oracle {
            prop_assert_eq!(
                db.get_value(k).unwrap(),
                Some(Bytes::copy_from_slice(v))
            );
        }
        // Absent keys read back None.
        for (k, _, _) in &ops {
            if !oracle.contains_key(k) {
                prop_assert_eq!(db.get_value(k).unwrap(), None);
            }
        }

        // The DB root matches the oracle's direct root.
        let refs: Vec<(&[u8], &[u8])> =
            oracle.iter().map(|(k, v)| (k.as_slice(), v.as_slice())).collect();
        let expected = merkle_root(BranchFactor::TwoFiftySix, &DefaultHasher, &refs);
        prop_assert_eq!(db.get_merkle_root().unwrap(), expected);
    }
}
