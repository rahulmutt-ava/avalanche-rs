// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! heightindexdb conformance: the memdb- and meterdb-backed `HeightIndex`
//! backends must pass the dedicated height-index battery (04 §2.9, 02 §7.2),
//! the Rust port of `database/heightindexdb/dbtest`.
//!
//! The shared battery lives behind the `testutil` feature, so the test bodies
//! are gated on it. The `unused_crate_dependencies` allow is unconditional (a
//! known false-positive of that lint for integration-test binaries).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(feature = "testutil")]
use ava_database::dbtest::run_heightindex_suite;
#[cfg(feature = "testutil")]
use ava_database::{HeightIndexMemDb, HeightIndexMeterDb};

#[cfg(feature = "testutil")]
mod conformance {
    use super::*;

    /// The full height-index battery over the in-memory backend.
    #[test]
    fn run_heightindex_suite_memdb() {
        run_heightindex_suite(HeightIndexMemDb::new);
    }

    /// The same battery over the Prometheus-metered backend (each instance under
    /// a fresh registry so the battery can build many DBs).
    #[test]
    fn run_heightindex_suite_meterdb() {
        run_heightindex_suite(|| {
            HeightIndexMeterDb::new(&prometheus::Registry::new(), "", HeightIndexMemDb::new())
                .unwrap()
        });
    }
}
