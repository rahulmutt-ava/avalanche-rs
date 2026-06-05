// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: `decode_db_node` over arbitrary bytes must never panic or
//! over-read (M1.25, spec 02 §8 — the "decoding never panics" invariant).
//!
//! The check lives in `ava_merkledb::fuzz_support::run_node_codec`, shared with
//! the stable `tests/prop_fuzz_smoke.rs` proptest.
//!
//! Requires a nightly toolchain + LLVM sanitizers (see `../README.md`):
//!   cargo +nightly fuzz run node_codec

#![no_main]

use ava_merkledb::fuzz_support::run_node_codec;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    run_node_codec(data);
});
