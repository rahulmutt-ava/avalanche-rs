// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M1.15 integration tests for `View`/`TrieView`, the node stores and the
//! `cleanShutdown` rebuild (spec 04 §3.5, 27 §4.1).

use std::sync::Arc;

use assert_matches::assert_matches;

use ava_database::MemDb;
use ava_merkledb::node::prefix;
use ava_merkledb::{BatchOp, BranchFactor, Error, MerkleDb, merkle_root};

fn op(key: &[u8], value: &[u8]) -> BatchOp {
    BatchOp::put(key, value)
}

/// Committing a view invalidates sibling views and their descendants, a view
/// commits only if its parent is the DB, and only once.
#[test]
fn commit_invalidates_siblings() {
    let base = Arc::new(MemDb::new());
    let db = MerkleDb::new(base, BranchFactor::TwoFiftySix).unwrap();

    // Two sibling views off the DB.
    let v1 = db.new_view(vec![op(b"a", b"1")]).unwrap();
    let v2 = db.new_view(vec![op(b"b", b"2")]).unwrap();

    // A child view layered on v1 (a descendant of v1, not of v2).
    let v1_child = v1.new_view(vec![op(b"c", b"3")]).unwrap();

    // Commit v1: this should invalidate the sibling v2, but NOT v1's own
    // descendant v1_child (which is re-parented onto the DB).
    v1.commit().unwrap();

    // The committed view itself reports already-committed on a second commit.
    assert_matches!(v1.commit(), Err(Error::Committed));

    // The sibling is now invalid.
    assert_matches!(v2.get_merkle_root(), Err(Error::Invalid));
    assert_matches!(v2.commit(), Err(Error::Invalid));

    // v1's descendant survives and is now committable (parent == DB).
    assert!(v1_child.get_merkle_root().is_ok());
    v1_child.commit().unwrap();

    // The DB now reflects a, c (from v1 + v1_child); b never landed.
    assert_eq!(db.get_value(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    assert_eq!(db.get_value(b"c").unwrap().as_deref(), Some(&b"3"[..]));
    assert_eq!(db.get_value(b"b").unwrap(), None);
}

/// A view that is not a direct child of the DB cannot be committed directly.
#[test]
fn commit_requires_db_parent() {
    let base = Arc::new(MemDb::new());
    let db = MerkleDb::new(base, BranchFactor::TwoFiftySix).unwrap();

    let parent = db.new_view(vec![op(b"a", b"1")]).unwrap();
    let child = parent.new_view(vec![op(b"b", b"2")]).unwrap();

    assert_matches!(child.commit(), Err(Error::ParentNotDatabase));
}

/// Applying changes through a (layered) view equals direct application of the
/// merged key/value set — the root matches the in-memory `merkle_root`.
#[test]
fn view_layering_equals_direct() {
    let base = Arc::new(MemDb::new());
    let db = MerkleDb::new(base, BranchFactor::TwoFiftySix).unwrap();

    // Layer three views, each adding keys.
    let v1 = db
        .new_view(vec![op(b"dog", b"woof"), op(b"cat", b"meow")])
        .unwrap();
    let v2 = v1.new_view(vec![op(b"do", b"verb")]).unwrap();
    let v3 = v2.new_view(vec![op(b"cat", b"purr")]).unwrap(); // overwrite cat

    let layered_root = v3.get_merkle_root().unwrap();

    // The equivalent direct application of the merged set.
    let direct = merkle_root(
        BranchFactor::TwoFiftySix,
        &ava_merkledb::DefaultHasher,
        &[(b"dog", b"woof"), (b"cat", b"purr"), (b"do", b"verb")],
    );
    assert_eq!(layered_root, direct);

    // Committing the chain to the DB yields the same root on the DB.
    v1.commit().unwrap();
    v2.commit().unwrap();
    v3.commit().unwrap();
    assert_eq!(db.get_merkle_root().unwrap(), direct);
    assert_eq!(db.get_value(b"cat").unwrap().as_deref(), Some(&b"purr"[..]));
}

/// On open with the `cleanShutdown` flag missing/false, the DB rebuilds
/// intermediate nodes from value nodes and serves the same root (27 §4.1).
#[test]
fn clean_shutdown_rebuild() {
    let base = Arc::new(MemDb::new());

    // Populate a DB and commit some values, then drop it WITHOUT a clean
    // shutdown (we simulate the unclean case by clobbering the flag below).
    let expected_root = {
        let db = MerkleDb::new(base.clone(), BranchFactor::TwoFiftySix).unwrap();
        let v = db
            .new_view(vec![
                op(b"dog", b"woof"),
                op(b"cat", b"meow"),
                op(b"do", b"verb"),
            ])
            .unwrap();
        v.commit().unwrap();
        db.get_merkle_root().unwrap()
    };

    // Simulate an unclean shutdown: delete every intermediate node and mark the
    // shutdown flag as unclean. Value nodes remain durable.
    {
        use ava_database::{Iteratee, Iterator as _, KeyValueDeleter, KeyValueWriter};
        let mut it = base.new_iterator_with_prefix(&[prefix::INTERMEDIATE_NODE]);
        let mut to_delete = Vec::new();
        while it.next() {
            if let Some(k) = it.key() {
                to_delete.push(k.to_vec());
            }
        }
        drop(it);
        for k in to_delete {
            base.delete(&k).unwrap();
        }
        // cleanShutdown -> false (0x00).
        let mut flag_key = vec![prefix::METADATA];
        flag_key.extend_from_slice(b"cleanShutdown");
        base.put(&flag_key, &[0x00]).unwrap();
    }

    // Re-open: the rebuild must reconstruct intermediate nodes and the root.
    let db = MerkleDb::new(base.clone(), BranchFactor::TwoFiftySix).unwrap();
    assert_eq!(db.get_merkle_root().unwrap(), expected_root);
    assert_eq!(db.get_value(b"dog").unwrap().as_deref(), Some(&b"woof"[..]));

    // After the rebuild, intermediate nodes exist again in the base DB.
    {
        use ava_database::{Iteratee, Iterator as _};
        let mut it = base.new_iterator_with_prefix(&[prefix::INTERMEDIATE_NODE]);
        let mut count = 0usize;
        while it.next() {
            count += 1;
        }
        assert!(count > 0, "intermediate nodes should be rebuilt");
    }
}
