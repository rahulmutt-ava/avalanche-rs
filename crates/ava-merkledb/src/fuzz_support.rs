// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared fuzz/property support for the merkledb (M1.25, spec 02 §8).
//!
//! Gated behind the `fuzzing` feature so it stays out of the normal public API.
//! Defines the single canonical [`DbOp`] op-stream model + the op-application
//! check against a [`BTreeMap`] oracle, and the `decode_db_node`-never-panics
//! check. Both the nightly `cargo-fuzz` targets under `fuzz/` and the stable
//! `tests/prop_fuzz_smoke.rs` proptest call into here, so the logic exists
//! exactly **once** (the nightly fuzz path and the local stable gate cannot
//! drift).
//!
//! The `fuzzing` feature is enabled by the standalone `fuzz/` crate (which
//! reuses [`DbOp`]) and by the dev-test build (the verification gate runs
//! `--all-features`).

use std::collections::BTreeMap;
use std::sync::Arc;

use arbitrary::Arbitrary;
use bytes::Bytes;

use ava_database::MemDb;

use crate::{BatchOp, BranchFactor, MerkleDb};

/// One operation in a structure-aware op stream.
///
/// Keys and values are deliberately drawn from a tiny byte space (see
/// [`small_key`]) so shared prefixes / collisions occur often, which is far
/// more stressful for the radix-trie structure than long random keys.
#[derive(Arbitrary, Clone, Debug, PartialEq, Eq)]
pub enum DbOp {
    /// Insert (or overwrite) `value` at `key`.
    Put { key: Vec<u8>, value: Vec<u8> },
    /// Delete `key`.
    Delete { key: Vec<u8> },
    /// Read `key` (must agree with the oracle).
    Get { key: Vec<u8> },
    /// Existence check on `key` (must agree with the oracle).
    Has { key: Vec<u8> },
    /// Read back every key currently in the oracle.
    Iterate,
}

/// Clamps an arbitrary key into the small, collision-prone key space
/// (1..=4 bytes, each in `0..8`). An empty input maps to a single `0` byte so
/// every op still targets a key.
#[must_use]
pub fn small_key(raw: &[u8]) -> Vec<u8> {
    if raw.is_empty() {
        return vec![0];
    }
    let n = 1 + (raw.len() % 4);
    raw.iter().take(n).map(|b| b % 8).collect()
}

/// Clamps an arbitrary value into a bounded space (0..=40 bytes), mixing the
/// short (inlined digest) and long (hashed digest) value paths.
#[must_use]
pub fn small_value(raw: &[u8]) -> Vec<u8> {
    let n = raw.len() % 41;
    raw.iter().take(n).copied().collect()
}

/// Applies an op stream to a fresh in-memory [`MerkleDb`] and a [`BTreeMap`]
/// oracle, asserting `Get`/`Has`/`Iterate` always agree with the oracle and
/// that nothing panics or over-reads. Shared verbatim by the `op_stream` fuzz
/// target and the `prop::fuzz_op_stream_smoke` proptest.
///
/// # Panics
///
/// Panics (the intended fuzz/proptest failure mode) on any oracle mismatch or
/// internal merkledb error.
pub fn run_op_stream(ops: &[DbOp]) {
    let base = Arc::new(MemDb::new());
    let db = MerkleDb::new(base, BranchFactor::TwoFiftySix)
        .expect("open merkledb over MemDb must not fail");
    let mut oracle: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();

    for op in ops {
        match op {
            DbOp::Put { key, value } => {
                let k = small_key(key);
                let v = small_value(value);
                oracle.insert(k.clone(), v.clone());
                db.new_view(vec![BatchOp::put(&k, &v)])
                    .expect("new_view(put)")
                    .commit()
                    .expect("commit(put)");
            }
            DbOp::Delete { key } => {
                let k = small_key(key);
                oracle.remove(&k);
                db.new_view(vec![BatchOp::delete(&k)])
                    .expect("new_view(delete)")
                    .commit()
                    .expect("commit(delete)");
            }
            DbOp::Get { key } => {
                let k = small_key(key);
                let got = db.get_value(&k).expect("get_value");
                let want = oracle.get(&k).map(|v| Bytes::copy_from_slice(v));
                assert_eq!(got, want, "get({k:?}) disagreed with oracle");
            }
            DbOp::Has { key } => {
                let k = small_key(key);
                let got = db.get_value(&k).expect("get_value").is_some();
                assert_eq!(got, oracle.contains_key(&k), "has({k:?}) disagreed");
            }
            DbOp::Iterate => {
                for (k, v) in &oracle {
                    let got = db.get_value(k).expect("get_value");
                    assert_eq!(
                        got,
                        Some(Bytes::copy_from_slice(v)),
                        "iterate read-back of {k:?} disagreed"
                    );
                }
            }
        }
    }

    // Final full reconciliation: every oracle key reads back its value.
    for (k, v) in &oracle {
        assert_eq!(
            db.get_value(k).expect("get_value"),
            Some(Bytes::copy_from_slice(v)),
            "final read-back of {k:?} disagreed with oracle"
        );
    }
}

/// Feeds arbitrary bytes to [`crate::decode_db_node`], asserting it never
/// panics or over-reads (it returns `Err` on malformed input — the spec §8
/// "decoding never panics" invariant). Shared by the `node_codec` fuzz target
/// and the `prop::node_codec_never_panics` proptest.
pub fn run_node_codec(data: &[u8]) {
    // The contract under test is purely "does not panic / over-read"; both
    // Ok and Err are acceptable outcomes.
    let _ = crate::decode_db_node(data);
}
