// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Structure-aware fuzz target: an arbitrary `Vec<DbOp>` op stream applied to
//! an in-memory merkledb trie and a `BTreeMap` oracle (M1.25, spec 02 §8 —
//! "ava-merkledb: arbitrary op stream against the oracle, structure-aware").
//!
//! The op-stream model + the oracle-agreement check live in
//! `ava_merkledb::fuzz_support` so this nightly target and the stable
//! `tests/prop_fuzz_smoke.rs` proptest exercise identical logic.
//!
//! Requires a nightly toolchain + LLVM sanitizers (see `../README.md`):
//!   cargo +nightly fuzz run op_stream

#![no_main]

use ava_merkledb::fuzz_support::{DbOp, run_op_stream};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|ops: Vec<DbOp>| {
    run_op_stream(&ops);
});
