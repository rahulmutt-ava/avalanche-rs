// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Behavioral tests for `ava-archivedb`: height-versioned read/write semantics
//! (specs/04 §5.2). Mirrors avalanchego `x/archivedb/db_test.go`.

use assert_matches::assert_matches;
use ava_archivedb::{ArchiveDb, Error};
use ava_database::MemDb;

fn db() -> ArchiveDb<MemDb> {
    ArchiveDb::new(MemDb::new())
}

#[test]
fn reads_newest_at_or_below() {
    let db = db();

    // h=10: foo=bar10, bar=bar10
    let mut b = db.new_batch(10);
    b.put(b"foo", b"bar10");
    b.put(b"bar", b"qux10");
    b.write().unwrap();

    // h=20: foo=bar20
    let mut b = db.new_batch(20);
    b.put(b"foo", b"bar20");
    b.write().unwrap();

    // h=30: delete foo
    let mut b = db.new_batch(30);
    b.delete(b"foo");
    b.write().unwrap();

    // Below the first write: not found.
    assert_matches!(db.open(9).get(b"foo"), Err(Error::NotFound));

    // At/above h=10, below h=20: original value, set-at height 10.
    assert_eq!(db.open(10).get(b"foo").unwrap(), b"bar10");
    assert_eq!(db.open(15).get(b"foo").unwrap(), b"bar10");
    assert_eq!(db.open(15).get_height(b"foo").unwrap(), 10);

    // At/above h=20, below the delete: updated value, set-at height 20.
    assert_eq!(db.open(20).get(b"foo").unwrap(), b"bar20");
    assert_eq!(db.open(29).get(b"foo").unwrap(), b"bar20");
    assert_eq!(db.open(29).get_height(b"foo").unwrap(), 20);

    // At/above the delete: tombstone ⇒ NotFound.
    assert_matches!(db.open(30).get(b"foo"), Err(Error::NotFound));
    assert_matches!(db.open(100).get(b"foo"), Err(Error::NotFound));
    assert_matches!(db.open(30).get_height(b"foo"), Err(Error::NotFound));

    // `bar` was never deleted: visible at every height ≥ 10.
    assert_eq!(db.open(10).get(b"bar").unwrap(), b"qux10");
    assert_eq!(db.open(1000).get(b"bar").unwrap(), b"qux10");
    assert_eq!(db.open(1000).get_height(b"bar").unwrap(), 10);

    // has() reflects existence.
    assert!(db.open(20).has(b"foo").unwrap());
    assert!(!db.open(30).has(b"foo").unwrap());
    assert!(!db.open(9).has(b"foo").unwrap());
}

#[test]
fn height_tracks_last_written_batch() {
    let db = db();

    // No batch written yet ⇒ height key absent.
    assert_matches!(db.height(), Err(Error::Database(_)));

    let mut b = db.new_batch(7);
    b.put(b"k", b"v");
    b.write().unwrap();
    assert_eq!(db.height().unwrap(), 7);

    let mut b = db.new_batch(42);
    b.put(b"k", b"v2");
    b.write().unwrap();
    assert_eq!(db.height().unwrap(), 42);
}

#[test]
fn empty_value_is_distinct_from_tombstone() {
    let db = db();

    // An explicit empty value still "exists".
    let mut b = db.new_batch(1);
    b.put(b"k", b"");
    b.write().unwrap();
    assert_eq!(db.open(1).get(b"k").unwrap(), b"");
    assert!(db.open(1).has(b"k").unwrap());

    // After a delete, it does not.
    let mut b = db.new_batch(2);
    b.delete(b"k");
    b.write().unwrap();
    assert_matches!(db.open(2).get(b"k"), Err(Error::NotFound));
    // ...but reading at the prior height still sees the empty value.
    assert_eq!(db.open(1).get(b"k").unwrap(), b"");
}

#[test]
fn reset_drops_buffered_ops() {
    let db = db();
    let mut b = db.new_batch(5);
    b.put(b"k", b"v");
    assert_eq!(b.len(), 1);
    b.reset();
    assert!(b.is_empty());
    b.write().unwrap();

    // Only the height marker was written; the key is absent.
    assert_eq!(db.height().unwrap(), 5);
    assert_matches!(db.open(5).get(b"k"), Err(Error::NotFound));
}

#[test]
fn missing_key_not_found() {
    let db = db();
    let mut b = db.new_batch(1);
    b.put(b"present", b"x");
    b.write().unwrap();
    assert_matches!(db.open(1).get(b"absent"), Err(Error::NotFound));
}
