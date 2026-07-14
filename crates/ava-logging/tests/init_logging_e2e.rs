// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! End-to-end regression test for the PRODUCTION subscriber assembly.
//!
//! The M9.15 live mixed_network runs surfaced a node whose captured output
//! contained ONLY log-crate-bridged events (dynamically dispatched, so they
//! bypass callsite Interest caching) while every NATIVE `tracing` event was
//! silent — the empty/non-empty chain-slot layer had cached all static
//! callsites as permanently disabled (fixed in `ChainSlotVec`, commits
//! f7e2f43 + 32ff8e8). The existing unit tests replicate the layer stack by
//! hand; none drives the real [`ava_logging::init_logging`] entrypoint that
//! the node boots through (`ava-node::logging::init`). This test pins the
//! full production path: install the global subscriber exactly as the node
//! does (default config levels), then assert native events emitted AFTER init
//! — both before and after a per-chain logger is added at runtime — land in
//! `main.log`, and chain-tagged events land in the per-chain file.
//!
//! It lives in its own integration-test binary because `init_logging`
//! installs the process-global dispatcher; keep it the ONLY `#[test]` here.

// Crate deps linked by the lib/support but not named directly by this target.
use assert_matches as _;
use chrono as _;
use flate2 as _;
use parking_lot as _;
use serde_json as _;
use thiserror as _;
use tracing_appender as _;
use tracing_subscriber as _;

use ava_logging::{LogConfig, init_logging};

#[test]
fn native_events_after_init_reach_main_log_and_chain_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = LogConfig {
        directory: dir.path().to_path_buf(),
        ..LogConfig::default()
    };

    // The exact production entrypoint (ava-node::logging::init → init_logging).
    let mut handles = init_logging(&cfg).expect("init_logging()");

    // A native tracing event whose callsite is first hit AFTER the subscriber
    // is installed — the startup-banner shape ("initializing node").
    tracing::info!("native event after init");

    // Add a per-chain logger at runtime (Go LogFactory.MakeChain shape). This
    // exercises the non-empty chain-slot rebuild path (`reload::modify` →
    // `rebuild_interest_cache`), which re-silenced the node pre-32ff8e8.
    let _c_handle = handles
        .add_chain_logger("C")
        .expect("LogHandles::add_chain_logger(C)");

    tracing::info!("native event after chain logger added");
    tracing::info!(chain = "C", "chain-tagged event after chain logger added");

    // Dropping the handles drops the non-blocking WorkerGuards, flushing the
    // rolling appenders.
    drop(handles);

    let main_log = std::fs::read_to_string(dir.path().join("main.log")).expect("read main.log");
    assert!(
        main_log.contains("native event after init"),
        "native event emitted after init_logging() must reach main.log; got: {main_log:?}"
    );
    assert!(
        main_log.contains("native event after chain logger added"),
        "native event emitted after add_chain_logger() must reach main.log; got: {main_log:?}"
    );

    let c_log = std::fs::read_to_string(dir.path().join("C.log")).expect("read C.log");
    assert!(
        c_log.contains("chain-tagged event after chain logger added"),
        "chain-tagged event must be routed to C.log; got: {c_log:?}"
    );
}
