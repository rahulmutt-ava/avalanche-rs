// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Codec conformance suite (M0.16, EXIT-GATE).
//!
//! Invokes the generic `ava_codec::codectest::run_codec_suite()` (the Go
//! `codectest.RunAll` analogue). This is the primary correctness anchor: it does
//! NOT need Go-extracted vectors.
//!
//! The `codectest` module is gated behind the `testutil` feature, so this suite
//! only runs when the crate is tested with `--features testutil` (or
//! `--all-features`). Run with: `cargo test -p ava-codec --features testutil`.

#[cfg(feature = "testutil")]
mod conformance {
    #[test]
    fn run_codec_suite() {
        ava_codec::codectest::run_codec_suite();
    }
}
