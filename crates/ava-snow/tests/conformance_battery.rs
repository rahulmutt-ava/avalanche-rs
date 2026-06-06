// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `conformance::snow_battery` — runs the generic Snowman consensus suite
//! (`run_consensus_suite`) against the production [`Topological`]
//! implementation. Ported from Go `snow/consensus/snowman/consensus_test.go` +
//! `network_test.go` (add / record_poll / accept ordering, duplicate add,
//! unknown parent, linear acceptance, sibling rejection, preference walk).
//!
//! Requires the `testutil` feature (the battery + test blocks live there), so
//! it is gated on it to keep a no-feature `cargo test` build clean (CI runs
//! `--all-features`).

#![cfg(feature = "testutil")]
#![allow(unused_crate_dependencies, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use ava_snow::snowball::SnowballFactory;
use ava_snow::snowtest::run_consensus_suite;
use ava_snow::snowman::Topological;
use ava_snow::snowman::block::BlockAcceptor;

#[test]
fn snow_battery() {
    run_consensus_suite(&|params, last_id, last_height, acceptor: Arc<dyn BlockAcceptor>| {
        Topological::new(SnowballFactory, acceptor, params, last_id, last_height)
    });
}
