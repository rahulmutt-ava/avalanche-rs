// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The reusable `Database` conformance battery, mirroring
//! `database/dbtest/dbtest.go` (04 §6.1, 02 §7.2).
//!
//! This is a **library of helpers**, not a test binary (02 §3.3): each backend
//! invokes [`run_database_suite`] / [`run_database_proptests`] from its own
//! `tests/conformance_<backend>.rs`. It is gated behind the `testutil` feature
//! so production builds don't pull in `proptest`.
//!
//! Every Go `dbtest.Tests` / `dbtest.TestsBasic` case is ported as a private
//! function and driven through the trait surface only (backend-agnostic). The
//! proptest body asserts that any op sequence behaves like a `BTreeMap` oracle.
//!
//! Assertions here intentionally panic on failure (the call site is a `#[test]`
//! in a backend crate), so `unwrap`/`expect`/`panic!` are allowed in this
//! module despite being denied in ordinary library code.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::Error;
use crate::traits::{Batch, Database, Iterator, WriteDelete};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Runs the full deterministic conformance battery against the backend built
/// by `new`. Mirrors Go's `dbtest.Tests` + `dbtest.TestsBasic` run against
/// every KV backend (02 §7.2). Each case gets a fresh DB.
pub fn run_database_suite<D, F>(new: F)
where
    D: Database,
    F: Fn() -> D,
{
    // TestsBasic (KeyValueReaderWriterDeleter surface).
    simple_key_value(&new());
    overwrite_key_value(&new());
    empty_key(&new());
    key_empty_value(&new());
    memory_safety_database(&new());
    modify_value_after_put(&new());
    put_get_empty(&new());

    // Tests (full Database surface).
    simple_key_value_closed(new());
    new_batch_closed(new());
    batch_put(new());
    batch_delete(new());
    batch_reset(new());
    batch_reuse(new());
    batch_rewrite(new());
    batch_replay(new());
    batch_replay_propagate_error(new());
    batch_inner(new());
    batch_large_size(new());
    memory_safety_batch(new());
    iterator_snapshot(new());
    iterator(new());
    iterator_start(new());
    iterator_prefix(new());
    iterator_start_prefix(new());
    iterator_memory_safety(new());
    iterator_closed(new());
    iterator_error(new());
    iterator_error_after_release(new());
    compact_no_panic(new());
    atomic_clear(new());
    clear(new());
    atomic_clear_prefix(new());
    clear_prefix(new());
    modify_value_after_batch_put(new());
    modify_value_after_batch_put_replay(new());
    concurrent_batches(new());
    many_small_concurrent_kv_batches(new());
}

/// The property battery: any op sequence behaves like a `BTreeMap` oracle, and
/// a full scan of the DB equals the oracle (02 §7.2). This is the public
/// `prop::db_oracle_btreemap` entry the exit gate calls.
pub fn run_database_proptests<D, F>(new: F)
where
    D: Database,
    F: Fn() -> D + Clone,
{
    use proptest::prelude::*;

    let mut runner = proptest::test_runner::TestRunner::default();
    let strat = proptest::collection::vec(arb_db_op(), 0..256);
    runner
        .run(&strat, |ops| {
            let db = new();
            let mut oracle = BTreeMap::<Vec<u8>, Vec<u8>>::new();
            for op in &ops {
                apply(&db, &mut oracle, op);
            }
            prop_assert_eq!(dump(&db), oracle);
            Ok(())
        })
        .expect("db_oracle_btreemap property failed");
}

// ---------------------------------------------------------------------------
// Proptest oracle machinery
// ---------------------------------------------------------------------------

/// A randomly generated database operation.
#[derive(Clone, Debug)]
enum DbOp {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    Get(Vec<u8>),
    Has(Vec<u8>),
    Iterate,
}

/// A strategy for arbitrary DB ops over a small key/value alphabet so the
/// generator regularly produces overlapping keys (exercising overwrite/delete).
fn arb_db_op() -> impl proptest::strategy::Strategy<Value = DbOp> {
    use proptest::prelude::*;

    // Keys/values are short byte strings drawn from a small alphabet.
    let key = proptest::collection::vec(0u8..8, 0..4);
    let value = proptest::collection::vec(0u8..8, 0..6);
    prop_oneof![
        (key.clone(), value).prop_map(|(k, v)| DbOp::Put(k, v)),
        key.clone().prop_map(DbOp::Delete),
        key.clone().prop_map(DbOp::Get),
        key.prop_map(DbOp::Has),
        Just(DbOp::Iterate),
    ]
}

/// Applies `op` to both the DB and the oracle, asserting read results agree.
fn apply<D: Database>(db: &D, oracle: &mut BTreeMap<Vec<u8>, Vec<u8>>, op: &DbOp) {
    match op {
        DbOp::Put(k, v) => {
            db.put(k, v).unwrap();
            oracle.insert(k.clone(), v.clone());
        }
        DbOp::Delete(k) => {
            db.delete(k).unwrap();
            oracle.remove(k);
        }
        DbOp::Get(k) => match db.get(k) {
            Ok(v) => assert_eq!(Some(&v), oracle.get(k), "get mismatch for {k:?}"),
            Err(Error::NotFound) => assert!(!oracle.contains_key(k), "spurious NotFound for {k:?}"),
            Err(e) => panic!("unexpected get error: {e}"),
        },
        DbOp::Has(k) => {
            let has = db.has(k).unwrap();
            assert_eq!(has, oracle.contains_key(k), "has mismatch for {k:?}");
        }
        DbOp::Iterate => {
            assert_eq!(&dump(db), oracle, "iterate mismatch");
        }
    }
}

/// Full-scans the DB into a `BTreeMap` for oracle comparison.
fn dump<D: Database>(db: &D) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut it = db.new_iterator();
    while it.next() {
        let k = it.key().expect("key while Next() true").to_vec();
        let v = it.value().expect("value while Next() true").to_vec();
        out.insert(k, v);
    }
    it.error().expect("iterator error during dump");
    out
}

// ---------------------------------------------------------------------------
// TestsBasic
// ---------------------------------------------------------------------------

fn simple_key_value<D: Database>(db: &D) {
    let key = b"hello";
    let value = b"world";

    assert!(!db.has(key).unwrap());
    assert!(matches!(db.get(key), Err(Error::NotFound)));

    db.delete(key).unwrap();
    db.put(key, value).unwrap();

    assert!(db.has(key).unwrap());
    assert_eq!(db.get(key).unwrap(), value);

    db.delete(key).unwrap();
    assert!(!db.has(key).unwrap());
    assert!(matches!(db.get(key), Err(Error::NotFound)));
    db.delete(key).unwrap();
}

fn overwrite_key_value<D: Database>(db: &D) {
    let key = b"hello";
    db.put(key, b"world1").unwrap();
    db.put(key, b"world2").unwrap();
    assert_eq!(db.get(key).unwrap(), b"world2");
}

fn key_empty_value<D: Database>(db: &D) {
    let key = b"hello";
    assert!(matches!(db.get(key), Err(Error::NotFound)));
    db.put(key, &[]).unwrap();
    assert!(db.get(key).unwrap().is_empty());
}

fn empty_key<D: Database>(db: &D) {
    let nil_key: &[u8] = &[];
    let empty_key: &[u8] = b"";
    let val1 = b"hi";
    let val2 = b"hello";

    assert!(matches!(db.get(nil_key), Err(Error::NotFound)));
    db.put(nil_key, val1).unwrap();
    assert_eq!(db.get(empty_key).unwrap(), val1);

    db.put(empty_key, val2).unwrap();
    assert_eq!(db.get(nil_key).unwrap(), val2);
}

fn memory_safety_database<D: Database>(db: &D) {
    // In Rust the byte-slice args are copied by `put`/returned owned by `get`,
    // so this property holds structurally; we still exercise the scenario.
    let mut key = b"1key".to_vec();
    let key2 = b"2key";
    let value = b"value";
    let value2 = b"value2";

    db.put(&key, value).unwrap();
    key[0] = key2[0];
    db.put(&key, value2).unwrap();
    key[0] = b'1';

    assert_eq!(db.get(&key).unwrap(), value);
    key[0] = key2[0];
    assert_eq!(db.get(&key).unwrap(), value2);
    key[0] = b'1';
    assert_eq!(db.get(&key).unwrap(), value);
}

fn modify_value_after_put<D: Database>(db: &D) {
    let key = &[1u8];
    let mut value = vec![1u8, 2];
    let original = value.clone();
    db.put(key, &value).unwrap();
    value[0] = 2;
    assert_eq!(db.get(key).unwrap(), original);
}

fn put_get_empty<D: Database>(db: &D) {
    let key = b"hello";
    db.put(key, &[]).unwrap();
    assert!(db.get(key).unwrap().is_empty());
    db.put(key, b"").unwrap();
    assert!(db.get(key).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Tests (full Database surface)
// ---------------------------------------------------------------------------

fn simple_key_value_closed<D: Database>(db: D) {
    let key = b"hello";
    let value = b"world";

    assert!(!db.has(key).unwrap());
    assert!(matches!(db.get(key), Err(Error::NotFound)));
    db.delete(key).unwrap();
    db.put(key, value).unwrap();
    assert!(db.has(key).unwrap());
    assert_eq!(db.get(key).unwrap(), value);

    db.close().unwrap();

    assert!(matches!(db.has(key), Err(Error::Closed)));
    assert!(matches!(db.get(key), Err(Error::Closed)));
    assert!(matches!(db.put(key, value), Err(Error::Closed)));
    assert!(matches!(db.delete(key), Err(Error::Closed)));
    assert!(matches!(db.close(), Err(Error::Closed)));
}

fn new_batch_closed<D: Database>(db: D) {
    db.close().unwrap();
    let mut batch = db.new_batch();
    batch.put(b"hello", b"world").unwrap();
    assert!(batch.size() > 0);
    assert!(matches!(batch.write(), Err(Error::Closed)));
}

fn batch_put<D: Database>(db: D) {
    let key = b"hello";
    let value = b"world";

    let mut batch = db.new_batch();
    batch.put(key, value).unwrap();
    assert!(batch.size() > 0);
    batch.write().unwrap();

    assert!(db.has(key).unwrap());
    assert_eq!(db.get(key).unwrap(), value);
    db.delete(key).unwrap();

    let mut batch = db.new_batch();
    batch.put(key, value).unwrap();
    db.close().unwrap();
    assert!(matches!(batch.write(), Err(Error::Closed)));
}

fn batch_delete<D: Database>(db: D) {
    let key = b"hello";
    db.put(key, b"world").unwrap();

    let mut batch = db.new_batch();
    batch.delete(key).unwrap();
    batch.write().unwrap();

    assert!(!db.has(key).unwrap());
    assert!(matches!(db.get(key), Err(Error::NotFound)));
    db.delete(key).unwrap();
}

fn batch_reset<D: Database>(db: D) {
    let key = b"hello";
    let value = b"world";
    db.put(key, value).unwrap();

    let mut batch = db.new_batch();
    batch.delete(key).unwrap();
    batch.reset();
    assert_eq!(batch.size(), 0);
    batch.write().unwrap();

    assert!(db.has(key).unwrap());
    assert_eq!(db.get(key).unwrap(), value);
}

fn batch_reuse<D: Database>(db: D) {
    let key1 = b"hello1";
    let value1 = b"world1";
    let key2 = b"hello2";
    let value2 = b"world2";

    let mut batch = db.new_batch();
    batch.put(key1, value1).unwrap();
    batch.write().unwrap();
    db.delete(key1).unwrap();
    assert!(!db.has(key1).unwrap());

    batch.reset();
    assert_eq!(batch.size(), 0);
    batch.put(key2, value2).unwrap();
    batch.write().unwrap();

    assert!(!db.has(key1).unwrap());
    assert!(db.has(key2).unwrap());
    assert_eq!(db.get(key2).unwrap(), value2);
}

fn batch_rewrite<D: Database>(db: D) {
    let key = b"hello1";
    let value = b"world1";

    let mut batch = db.new_batch();
    batch.put(key, value).unwrap();
    batch.write().unwrap();
    db.delete(key).unwrap();
    assert!(!db.has(key).unwrap());

    batch.write().unwrap();
    assert!(db.has(key).unwrap());
    assert_eq!(db.get(key).unwrap(), value);
}

/// A `WriteDelete` recorder used to assert `Batch::replay` order.
#[derive(Default)]
struct ReplayRecorder {
    ops: Vec<(bool, Vec<u8>, Vec<u8>)>, // (is_delete, key, value)
}

impl WriteDelete for ReplayRecorder {
    fn put(&mut self, key: &[u8], value: &[u8]) -> crate::error::Result<()> {
        self.ops.push((false, key.to_vec(), value.to_vec()));
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> crate::error::Result<()> {
        self.ops.push((true, key.to_vec(), Vec::new()));
        Ok(())
    }
}

fn batch_replay<D: Database>(db: D) {
    let key1 = b"hello1".to_vec();
    let value1 = b"world1".to_vec();
    let key2 = b"hello2".to_vec();
    let value2 = b"world2".to_vec();

    let mut batch = db.new_batch();
    batch.put(&key1, &value1).unwrap();
    batch.put(&key2, &value2).unwrap();
    batch.delete(&key1).unwrap();
    batch.delete(&key2).unwrap();
    batch.put(&key1, &value2).unwrap();

    let expected = vec![
        (false, key1.clone(), value1.clone()),
        (false, key2.clone(), value2.clone()),
        (true, key1.clone(), Vec::new()),
        (true, key2.clone(), Vec::new()),
        (false, key1.clone(), value2.clone()),
    ];

    // Replay is idempotent / re-runnable (Go runs it twice).
    for _ in 0..2 {
        let mut rec = ReplayRecorder::default();
        batch.replay(&mut rec).unwrap();
        assert_eq!(rec.ops, expected);
    }
}

/// A recorder whose `put` fails on the first call, to assert error propagation.
struct FailingRecorder {
    err: Option<Error>,
}

impl WriteDelete for FailingRecorder {
    fn put(&mut self, _key: &[u8], _value: &[u8]) -> crate::error::Result<()> {
        match self.err.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn delete(&mut self, _key: &[u8]) -> crate::error::Result<()> {
        Ok(())
    }
}

fn batch_replay_propagate_error<D: Database>(db: D) {
    let mut batch = db.new_batch();
    batch.put(b"hello1", b"world1").unwrap();
    batch.put(b"hello2", b"world2").unwrap();

    let mut rec = FailingRecorder {
        err: Some(Error::Closed),
    };
    assert!(matches!(batch.replay(&mut rec), Err(Error::Closed)));

    let mut rec = FailingRecorder {
        err: Some(Error::Other(anyhow::anyhow!("io: closed pipe"))),
    };
    assert!(matches!(batch.replay(&mut rec), Err(Error::Other(_))));
}

fn batch_inner<D: Database>(db: D) {
    let key1 = b"hello1";
    let value1 = b"world1";
    let key2 = b"hello2";
    let value2 = b"world2";

    let mut first = db.new_batch();
    first.put(key1, value1).unwrap();

    let mut second = db.new_batch();
    second.put(key2, value2).unwrap();

    // Replay first's inner ops onto second's inner, then write second.
    let inner_first = first.inner();
    {
        let inner_second = second.inner();
        inner_first.replay(inner_second_as_wd(inner_second)).unwrap();
    }
    second.write().unwrap();

    assert!(db.has(key1).unwrap());
    assert_eq!(db.get(key1).unwrap(), value1);
    assert!(db.has(key2).unwrap());
    assert_eq!(db.get(key2).unwrap(), value2);
}

/// Adapts a `&mut dyn Batch` to `&mut dyn WriteDelete` for `replay`.
fn inner_second_as_wd(b: &mut dyn Batch) -> &mut dyn WriteDelete {
    b
}

fn batch_large_size<D: Database>(db: D) {
    // 8 MiB total, 4 KiB elements => 8 KiB pairs (mirrors Go TestBatchLargeSize,
    // shrunk to keep the suite fast while still exercising large batches).
    let total = 1 << 20; // 1 MiB is plenty for conformance.
    let element = 4 * 1024;
    let bytes = pseudo_random_bytes(total);

    let mut batch = db.new_batch();
    let mut i = 0usize;
    while i + 2 * element <= bytes.len() {
        let key = &bytes[i..i + element];
        let value = &bytes[i + element..i + 2 * element];
        batch.put(key, value).unwrap();
        i += 2 * element;
    }
    batch.write().unwrap();
}

fn memory_safety_batch<D: Database>(db: D) {
    let mut key = b"hello".to_vec();
    let key_copy = key.clone();
    let value = b"world".to_vec();

    let mut batch = db.new_batch();
    batch.put(&key, &value).unwrap();
    assert!(batch.size() > 0);

    // Mutate the key after Put; the batch must have copied it.
    key[0] = b'j';
    batch.write().unwrap();

    assert!(db.has(&key_copy).unwrap());
    assert_eq!(db.get(&key_copy).unwrap(), value);
    assert!(!db.has(&key).unwrap());
}

fn iterator_snapshot<D: Database>(db: D) {
    let key1 = b"hello1";
    let value1 = b"world1";
    let key2 = b"hello2";
    let value2 = b"world2";

    db.put(key1, value1).unwrap();
    let mut it = db.new_iterator();
    db.put(key2, value2).unwrap();

    assert!(it.next());
    assert_eq!(it.key(), Some(key1.as_slice()));
    assert_eq!(it.value(), Some(value1.as_slice()));

    assert!(!it.next());
    assert_eq!(it.key(), None);
    assert_eq!(it.value(), None);
    it.error().unwrap();
}

fn iterator<D: Database>(db: D) {
    let key1 = b"hello1";
    let value1 = b"world1";
    let key2 = b"hello2";
    let value2 = b"world2";

    db.put(key1, value1).unwrap();
    db.put(key2, value2).unwrap();

    let mut it = db.new_iterator();
    assert!(it.next());
    assert_eq!(it.key(), Some(key1.as_slice()));
    assert_eq!(it.value(), Some(value1.as_slice()));
    assert!(it.next());
    assert_eq!(it.key(), Some(key2.as_slice()));
    assert_eq!(it.value(), Some(value2.as_slice()));
    assert!(!it.next());
    assert_eq!(it.key(), None);
    assert_eq!(it.value(), None);
    it.error().unwrap();
}

fn iterator_start<D: Database>(db: D) {
    let key1 = b"hello1";
    let value1 = b"world1";
    let key2 = b"hello2";
    let value2 = b"world2";

    db.put(key1, value1).unwrap();
    db.put(key2, value2).unwrap();

    let mut it = db.new_iterator_with_start(key2);
    assert!(it.next());
    assert_eq!(it.key(), Some(key2.as_slice()));
    assert_eq!(it.value(), Some(value2.as_slice()));
    assert!(!it.next());
    it.error().unwrap();
}

fn iterator_prefix<D: Database>(db: D) {
    db.put(b"hello", b"world1").unwrap();
    db.put(b"goodbye", b"world2").unwrap();
    db.put(b"joy", b"world3").unwrap();

    let mut it = db.new_iterator_with_prefix(b"h");
    assert!(it.next());
    assert_eq!(it.key(), Some(b"hello".as_slice()));
    assert_eq!(it.value(), Some(b"world1".as_slice()));
    assert!(!it.next());
    it.error().unwrap();
}

fn iterator_start_prefix<D: Database>(db: D) {
    db.put(b"hello1", b"world1").unwrap();
    db.put(b"z", b"world2").unwrap();
    db.put(b"hello3", b"world3").unwrap();

    let mut it = db.new_iterator_with_start_and_prefix(b"hello1", b"h");
    assert!(it.next());
    assert_eq!(it.key(), Some(b"hello1".as_slice()));
    assert_eq!(it.value(), Some(b"world1".as_slice()));
    assert!(it.next());
    assert_eq!(it.key(), Some(b"hello3".as_slice()));
    assert_eq!(it.value(), Some(b"world3".as_slice()));
    assert!(!it.next());
    it.error().unwrap();
}

fn iterator_memory_safety<D: Database>(db: D) {
    db.put(b"hello1", b"world1").unwrap();
    db.put(b"z", b"world2").unwrap();
    db.put(b"hello3", b"world3").unwrap();

    let mut keys = Vec::new();
    let mut values = Vec::new();
    let mut it = db.new_iterator();
    while it.next() {
        keys.push(it.key().unwrap().to_vec());
        values.push(it.value().unwrap().to_vec());
    }
    it.error().unwrap();

    let expected_keys: Vec<&[u8]> = vec![b"hello1", b"hello3", b"z"];
    let expected_values: Vec<&[u8]> = vec![b"world1", b"world3", b"world2"];
    assert_eq!(keys, expected_keys);
    assert_eq!(values, expected_values);
}

fn iterator_closed<D: Database>(db: D) {
    db.put(b"hello1", b"world1").unwrap();
    db.close().unwrap();

    for make in [0, 1, 2, 3] {
        let mut it = match make {
            0 => db.new_iterator(),
            1 => db.new_iterator_with_prefix(&[]),
            2 => db.new_iterator_with_start(&[]),
            _ => db.new_iterator_with_start_and_prefix(&[], &[]),
        };
        assert!(!it.next());
        assert_eq!(it.key(), None);
        assert_eq!(it.value(), None);
        assert!(matches!(it.error(), Err(Error::Closed)));
    }
}

fn iterator_error<D: Database>(db: D) {
    let key1 = b"hello1";
    let value1 = b"world1";
    db.put(key1, value1).unwrap();
    db.put(b"hello2", b"world2").unwrap();

    let mut it = db.new_iterator();
    // Advance once, then close: the iterator can still serve the current pair.
    assert!(it.next());
    db.close().unwrap();
    assert_eq!(it.key(), Some(key1.as_slice()));
    assert_eq!(it.value(), Some(value1.as_slice()));

    // Subsequent Next() reports closed.
    assert!(!it.next());
    assert_eq!(it.key(), None);
    assert_eq!(it.value(), None);
    assert!(matches!(it.error(), Err(Error::Closed)));
}

fn iterator_error_after_release<D: Database>(db: D) {
    db.put(b"hello1", b"world1").unwrap();
    db.close().unwrap();

    let mut it = db.new_iterator();
    it.release();
    assert!(!it.next());
    assert_eq!(it.key(), None);
    assert_eq!(it.value(), None);
    assert!(matches!(it.error(), Err(Error::Closed)));
}

fn compact_no_panic<D: Database>(db: D) {
    db.put(b"hello1", b"world1").unwrap();
    db.put(b"z", b"world2").unwrap();
    db.put(b"hello3", b"world3").unwrap();

    db.compact(None, None).unwrap();
    db.compact(Some(&[2]), Some(&[1])).unwrap();
    db.compact(Some(&[255]), None).unwrap();

    db.close().unwrap();
    assert!(matches!(db.compact(None, None), Err(Error::Closed)));
}

fn count<D: Database>(db: &D) -> usize {
    crate::helpers::count(db).unwrap()
}

fn atomic_clear<D: Database>(db: D) {
    seed_clear(&db);
    assert_eq!(count(&db), 3);
    crate::helpers::atomic_clear(&db, &db).unwrap();
    assert_eq!(count(&db), 0);
    db.close().unwrap();
}

fn clear<D: Database>(db: D) {
    seed_clear(&db);
    assert_eq!(count(&db), 3);
    crate::helpers::clear(&db, usize::MAX).unwrap();
    assert_eq!(count(&db), 0);
    db.close().unwrap();
}

fn atomic_clear_prefix<D: Database>(db: D) {
    seed_clear(&db);
    assert_eq!(count(&db), 3);
    crate::helpers::atomic_clear_prefix(&db, &db, b"hello").unwrap();
    assert_eq!(count(&db), 1);
    assert!(!db.has(b"hello1").unwrap());
    assert!(db.has(b"z").unwrap());
    assert!(!db.has(b"hello3").unwrap());
    db.close().unwrap();
}

fn clear_prefix<D: Database>(db: D) {
    seed_clear(&db);
    assert_eq!(count(&db), 3);
    crate::helpers::clear_prefix(&db, b"hello", usize::MAX).unwrap();
    assert_eq!(count(&db), 1);
    assert!(!db.has(b"hello1").unwrap());
    assert!(db.has(b"z").unwrap());
    assert!(!db.has(b"hello3").unwrap());
    db.close().unwrap();
}

fn seed_clear<D: Database>(db: &D) {
    db.put(b"hello1", b"world1").unwrap();
    db.put(b"z", b"world2").unwrap();
    db.put(b"hello3", b"world3").unwrap();
}

fn modify_value_after_batch_put<D: Database>(db: D) {
    let key = &[1u8];
    let mut value = vec![1u8, 2];
    let original = value.clone();

    let mut batch = db.new_batch();
    batch.put(key, &value).unwrap();
    value[0] = 2;
    batch.write().unwrap();

    assert_eq!(db.get(key).unwrap(), original);
}

fn modify_value_after_batch_put_replay<D: Database>(db: D) {
    let key = &[1u8];
    let mut value = vec![1u8, 2];
    let original = value.clone();

    let mut batch = db.new_batch();
    batch.put(key, &value).unwrap();
    value[0] = 2;

    let mut replay = db.new_batch();
    batch.replay(inner_second_as_wd(replay.as_mut())).unwrap();
    replay.write().unwrap();

    assert_eq!(db.get(key).unwrap(), original);
}

fn concurrent_batches<D: Database>(db: D) {
    run_concurrent_batches(db, 10, 50, 32, 1024);
}

fn many_small_concurrent_kv_batches<D: Database>(db: D) {
    run_concurrent_batches(db, 100, 10, 10, 10);
}

fn run_concurrent_batches<D: Database>(
    db: D,
    num_batches: usize,
    keys_per_batch: usize,
    key_size: usize,
    value_size: usize,
) {
    let db = Arc::new(db);
    // Pre-build batches' contents (own data), then write concurrently.
    let mut all: Vec<Vec<(Vec<u8>, Vec<u8>)>> = Vec::with_capacity(num_batches);
    let mut seed = 0x1234_5678u64;
    for _ in 0..num_batches {
        let mut pairs = Vec::with_capacity(keys_per_batch);
        for _ in 0..keys_per_batch {
            pairs.push((
                next_random_bytes(&mut seed, key_size),
                next_random_bytes(&mut seed, value_size),
            ));
        }
        all.push(pairs);
    }

    std::thread::scope(|scope| {
        for pairs in &all {
            let db = Arc::clone(&db);
            scope.spawn(move || {
                let mut batch = db.new_batch();
                for (k, v) in pairs {
                    batch.put(k, v).unwrap();
                }
                batch.write().unwrap();
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Deterministic pseudo-random bytes (no rand dep; xorshift64).
// ---------------------------------------------------------------------------

fn pseudo_random_bytes(n: usize) -> Vec<u8> {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    next_random_bytes(&mut seed, n)
}

fn next_random_bytes(seed: &mut u64, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // xorshift64
        let mut x = *seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *seed = x;
        out.push((x & 0xff) as u8);
    }
    out
}
