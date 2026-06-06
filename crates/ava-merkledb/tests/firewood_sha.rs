// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `unit::firewood_propose_commit_roundtrip` (M1.20, spec 04 §4.2).
//!
//! Proves the firewood wrapper links and that the propose/commit/revision
//! lifecycle behaves as consensus expects:
//! - a proposal exposes its post-application root **before** commit (consensus
//!   votes on it),
//! - `commit()` advances the database tip to that root,
//! - read-after-commit returns the committed value,
//! - a historical revision (`get_at(old_root, …)`) still reads the prior value
//!   within the retained revision window.
//!
//! Runs against a real on-disk firewood instance in a `tempfile` scratch dir.
//! These assertions are hash-mode agnostic (they never assert a specific root
//! byte value), so they pass in both SHA-256 and ethhash (`--all-features`) mode.

#![cfg(feature = "firewood")]

use ava_merkledb::firewood::{BatchOp, FirewoodDb};
use ava_types::id::Id;

fn put(key: &[u8], value: &[u8]) -> BatchOp {
    BatchOp::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}

#[test]
fn firewood_propose_commit_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = FirewoodDb::open(dir.path()).expect("open firewood");

    // A fresh database is at the empty-trie root.
    assert_eq!(db.root(), ava_merkledb::firewood::empty_root());
    assert_eq!(db.get(b"alpha").expect("get on empty"), None);

    // --- Revision 1: write two keys. ---
    let proposal = db
        .propose(vec![put(b"alpha", b"one"), put(b"beta", b"two")])
        .expect("propose r1");

    // The proposal's root is available PRE-commit (consensus votes on it) and is
    // not the empty root.
    let root1 = proposal.root();
    assert_ne!(root1, Id::EMPTY);
    assert_ne!(root1, ava_merkledb::firewood::empty_root());
    // The proposal can be read before commit.
    assert_eq!(
        proposal.get(b"alpha").expect("proposal get"),
        Some(b"one".to_vec())
    );
    // The tip has NOT advanced yet (still empty).
    assert_eq!(db.root(), ava_merkledb::firewood::empty_root());

    // Commit advances the tip to the proposal's root.
    proposal.commit().expect("commit r1");
    assert_eq!(db.root(), root1);
    assert_eq!(db.get(b"alpha").expect("get alpha"), Some(b"one".to_vec()));
    assert_eq!(db.get(b"beta").expect("get beta"), Some(b"two".to_vec()));

    // --- Revision 2: update alpha, delete beta. ---
    let proposal2 = db
        .propose(vec![
            put(b"alpha", b"ONE"),
            BatchOp::Delete {
                key: b"beta".to_vec(),
            },
        ])
        .expect("propose r2");
    let root2 = proposal2.root();
    assert_ne!(root2, root1);
    proposal2.commit().expect("commit r2");
    assert_eq!(db.root(), root2);

    // Read-after-commit reflects revision 2.
    assert_eq!(
        db.get(b"alpha").expect("get alpha r2"),
        Some(b"ONE".to_vec())
    );
    assert_eq!(db.get(b"beta").expect("get beta r2"), None);

    // --- Historical read: revision 1 still reads the prior values. ---
    assert_eq!(
        db.get_at(&root1, b"alpha").expect("historical alpha"),
        Some(b"one".to_vec()),
        "historical revision must still see the original value"
    );
    assert_eq!(
        db.get_at(&root1, b"beta").expect("historical beta"),
        Some(b"two".to_vec()),
        "historical revision must still see the (later-deleted) key"
    );
}

#[test]
fn firewood_reopen_with_small_revision_window() {
    // A bounded revision window still serves the latest committed state.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = FirewoodDb::open_with_revisions(dir.path(), 4).expect("open firewood");
    let proposal = db.propose(vec![put(b"k", b"v")]).expect("propose");
    let root = proposal.root();
    proposal.commit().expect("commit");
    assert_eq!(db.root(), root);
    assert_eq!(db.get(b"k").expect("get"), Some(b"v".to_vec()));
}
