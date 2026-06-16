// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-upgrade` — the Go→Rust rolling-upgrade test harness (specs/02 §10.4;
//! specs/16 §5(8); specs/26 §7 moving min-compatible floor; specs/00 §4.4;
//! M9.17).
//!
//! Models a rolling upgrade: an N-node network starts on the previous-released
//! **Go** binary, advances to just before an activation height, and is rolled
//! one node at a time onto the **Rust** binary across the activation height.
//! Each swap imports the node's Go data dir → RocksDB (the M9.16 facade) and
//! asserts state continuity; the whole roll is checked for chain continuity /
//! **no fork** and that the moving min-compatible floor (specs/26 §7) keeps Go
//! and Rust peers connected.
//!
//! Two layers, mirroring the established M9 offline/live split:
//!
//! * [`plan`] — the swap / import orchestration. [`plan::RollingUpgrade`] drives
//!   a per-node [`plan::RollingUpgrade::swap`] through the REAL
//!   [`import_source_into_rocksdb`](ava_database::migrate::import::import_source_into_rocksdb)
//!   facade (on-disk RocksDB write path), then re-opens the imported `v1.4.5/`
//!   dir and byte-verifies the migrated KV set against the source.
//! * [`continuity`] — the no-fork assertions over
//!   [`Observation`](ava_differential::Observation)
//!   ([`continuity::assert_no_fork`]) and the specs/26 §7 moving-floor model
//!   ([`continuity::MovingFloor`]) over the real [`ava_version`] compatibility
//!   checker.
//!
//! The pure-Rust offline arms (in `tests/`) run every CI run; the live arm
//! (`go_to_rust`, gated behind the `live` feature + `#[ignore]`) boots a live
//! previous-Go tmpnet via the `ava-differential` two-binary `Network` driver and
//! is never run in CI / this sandbox.

#![forbid(unsafe_code)]

pub mod continuity;
pub mod plan;

pub use continuity::{CutoverStep, ForkError, MovingFloor, assert_no_fork};
pub use plan::{GoNodeData, Node, RollingUpgrade, Running, SwapError, SwapReport};

// `tempfile` is consumed only by the integration-test targets (each `swap()` is
// driven into a `TempDir` destination root there), and `tokio` / `serde_json`
// only by the gated live arm. The lib-test build links every dev-dependency, so
// `unused_crate_dependencies` would flag them here; reference them in a test-only
// block (the established workspace idiom — see `tests/differential/src/lib.rs`).
//
// (`ava-database`, `ava-differential`, `ava-version`, `anyhow`, `thiserror` are
// genuine lib deps used by `plan`/`continuity`, so they are NOT listed.)
#[cfg(test)]
mod dev_dep_uses {
    use serde_json as _;
    use tempfile as _;
    use tokio as _;
}
