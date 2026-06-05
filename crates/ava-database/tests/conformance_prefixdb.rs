// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! prefixdb conformance: a `PrefixDb` over `MemDb` must pass the full shared
//! `dbtest` battery and the BTreeMap-oracle proptest (04 §2.3, 02 §7.2).
//!
//! The shared battery lives behind the `testutil` feature, so the test bodies
//! are gated on it. The `unused_crate_dependencies` allow is unconditional (a
//! known false-positive of that lint for integration-test binaries).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(feature = "testutil")]
use ava_database::MemDb;
#[cfg(feature = "testutil")]
use ava_database::PrefixDb;
#[cfg(feature = "testutil")]
use ava_database::dbtest::{run_database_proptests, run_database_suite};

#[cfg(feature = "testutil")]
fn new_prefixdb() -> PrefixDb<MemDb> {
    PrefixDb::new(b"test", MemDb::new())
}

#[cfg(feature = "testutil")]
mod conformance {
    use super::*;

    /// The full deterministic conformance battery over a prefixed MemDb.
    #[test]
    fn run_database_suite() {
        super::run_database_suite(new_prefixdb);
    }
}

#[cfg(feature = "testutil")]
mod prop {
    use super::*;

    /// Any op sequence behaves like a `BTreeMap` oracle (full-scan equality).
    #[test]
    fn db_oracle_btreemap() {
        run_database_proptests(new_prefixdb);
    }
}
