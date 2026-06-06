// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! In-memory test-VM cluster scaffolding (feature `testutil`).
//!
//! Used by the `prop::consensus_safety` harness (M3.1) and, once `Topological`
//! lands (M3.5), the consensus battery. At M3.1 the cluster internals are
//! **placeholder**: `step` records votes and `accepted_chain` reports the
//! genesis-rooted chain accepted so far. The real Snowman wiring is added at
//! M3.5 — at which point the safety proptest is un-ignored and asserts against
//! genuine consensus output. The public surface (`Cluster::new`/`step`/
//! `accepted_chain`, `TestVm`, `TestBlock`) is stable now so the proptest body
//! compiles today.

mod cluster;
mod test_block;
mod test_vm;

pub use cluster::Cluster;
pub use test_block::TestBlock;
pub use test_vm::{AcceptanceOracle, TestVm};
