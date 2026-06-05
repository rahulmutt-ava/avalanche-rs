// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! rocksdb conformance: the on-disk default backend must pass the full shared
//! `dbtest` battery and the BTreeMap-oracle proptest (04 §2.1, 02 §7.2).
//!
//! Each constructed `RocksDb` lives under a fresh `tempfile::TempDir`, owned by
//! the DB so the directory survives for the DB's lifetime and is cleaned up on
//! drop. The shared battery lives behind the `testutil` feature, so the test
//! bodies are gated on it. The `unused_crate_dependencies` allow is
//! unconditional (a known false-positive of that lint for integration-test
//! binaries).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(all(feature = "testutil", feature = "rocksdb"))]
use ava_database::dbtest::{run_database_proptests, run_database_suite};
#[cfg(all(feature = "testutil", feature = "rocksdb"))]
use ava_database::rocksdb::RocksDb;

#[cfg(all(feature = "testutil", feature = "rocksdb"))]
fn new_rocksdb() -> RocksDb {
    RocksDb::open_temp().unwrap()
}

#[cfg(all(feature = "testutil", feature = "rocksdb"))]
mod conformance {
    use super::*;

    /// The full deterministic conformance battery over a temp RocksDB.
    #[test]
    fn run_database_suite() {
        super::run_database_suite(new_rocksdb);
    }
}

#[cfg(all(feature = "testutil", feature = "rocksdb"))]
mod prop {
    use super::*;

    /// Any op sequence behaves like a `BTreeMap` oracle (full-scan equality).
    #[test]
    fn db_oracle_btreemap() {
        run_database_proptests(new_rocksdb);
    }
}
