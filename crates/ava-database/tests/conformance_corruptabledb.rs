// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! corruptabledb conformance: a `CorruptableDb` over `MemDb` must pass the full
//! shared `dbtest` battery and the BTreeMap-oracle proptest (04 §2.6, 02 §7.2).
//!
//! The poison-latch unit tests (`poison_latches_on_other`,
//! `closed_and_not_found_do_not_latch`) live inline in `src/corruptabledb.rs`
//! with a failpoint inner DB. This binary drives the backend-agnostic suite.
//! The `unused_crate_dependencies` allow is unconditional (a known false-positive
//! of that lint for integration-test binaries).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(feature = "testutil")]
use ava_database::CorruptableDb;
#[cfg(feature = "testutil")]
use ava_database::MemDb;
#[cfg(feature = "testutil")]
use ava_database::dbtest::{run_database_proptests, run_database_suite};

#[cfg(feature = "testutil")]
fn new_corruptabledb() -> CorruptableDb<MemDb> {
    CorruptableDb::new(MemDb::new())
}

#[cfg(feature = "testutil")]
mod conformance {
    use super::*;

    /// The full deterministic conformance battery over a CorruptableDb on MemDb.
    #[test]
    fn run_database_suite() {
        super::run_database_suite(new_corruptabledb);
    }
}

#[cfg(feature = "testutil")]
mod prop {
    use super::*;

    /// Any op sequence behaves like a `BTreeMap` oracle (full-scan equality).
    #[test]
    fn db_oracle_btreemap() {
        run_database_proptests(new_corruptabledb);
    }
}
