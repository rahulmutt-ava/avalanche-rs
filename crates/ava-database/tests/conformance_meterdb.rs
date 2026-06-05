// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! meterdb conformance: a `MeterDb` over `MemDb` must pass the full shared
//! `dbtest` battery and the BTreeMap-oracle proptest (04 §2.5, 02 §7.2).
//!
//! The shared battery lives behind the `testutil` feature, so the test bodies
//! are gated on it. The `unused_crate_dependencies` allow is unconditional (a
//! known false-positive of that lint for integration-test binaries).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(feature = "testutil")]
use ava_database::MemDb;
#[cfg(feature = "testutil")]
use ava_database::dbtest::{run_database_proptests, run_database_suite};
#[cfg(feature = "testutil")]
use ava_database::meterdb::MeterDb;

// Each constructed `MeterDb` registers under a fresh `prometheus::Registry`, so
// the suite (which builds many DBs) never hits a duplicate-registration error.
#[cfg(feature = "testutil")]
fn new_meterdb() -> MeterDb<MemDb> {
    MeterDb::new(&prometheus::Registry::new(), MemDb::new()).unwrap()
}

#[cfg(feature = "testutil")]
mod conformance {
    use super::*;

    /// The full deterministic conformance battery over a metered MemDb.
    #[test]
    fn run_database_suite() {
        super::run_database_suite(new_meterdb);
    }
}

#[cfg(feature = "testutil")]
mod prop {
    use super::*;

    /// Any op sequence behaves like a `BTreeMap` oracle (full-scan equality).
    #[test]
    fn db_oracle_btreemap() {
        run_database_proptests(new_meterdb);
    }
}
