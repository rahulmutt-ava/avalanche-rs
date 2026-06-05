// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! versiondb conformance: a `VersionDb` over `MemDb` must pass the full shared
//! `dbtest` battery and the BTreeMap-oracle proptest (04 §2.4, 02 §7.2).
//!
//! The merge-iterator state-machine unit tests live inline in `src/versiondb.rs`
//! (`tests::merge_iterator_*`). This binary drives the backend-agnostic suite.
//! The `unused_crate_dependencies` allow is unconditional (a known false-positive
//! of that lint for integration-test binaries).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(feature = "testutil")]
use ava_database::MemDb;
#[cfg(feature = "testutil")]
use ava_database::VersionDb;
#[cfg(feature = "testutil")]
use ava_database::dbtest::{run_database_proptests, run_database_suite};

#[cfg(feature = "testutil")]
fn new_versiondb() -> VersionDb<MemDb> {
    VersionDb::new(MemDb::new())
}

#[cfg(feature = "testutil")]
mod conformance {
    use super::*;

    /// The full deterministic conformance battery over a VersionDb on MemDb.
    #[test]
    fn run_database_suite() {
        super::run_database_suite(new_versiondb);
    }
}

#[cfg(feature = "testutil")]
mod prop {
    use super::*;

    /// Any op sequence behaves like a `BTreeMap` oracle (full-scan equality).
    #[test]
    fn db_oracle_btreemap() {
        run_database_proptests(new_versiondb);
    }
}
