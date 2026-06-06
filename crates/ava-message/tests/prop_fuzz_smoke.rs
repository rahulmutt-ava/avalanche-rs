// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.6 — stable proptest smoke harness for the `decode_never_overreads` fuzz
//! target (specs/02 §8, M1.25 pattern). Calls the same
//! `ava_message::fuzz_support` body as the nightly libfuzzer target, so the
//! "unmarshal never panics / over-reads" invariant is checked on every PR under
//! the pinned stable toolchain (`cargo nextest`).
//!
//! Requires the `fuzzing` feature (the `--all-features` test gate enables it).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    unused_crate_dependencies
)]
#![cfg(feature = "fuzzing")]

use proptest::prelude::*;

use ava_message::fuzz_support::decode_never_overreads;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2048,
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::SourceParallel("proptest-regressions"),
        )),
        ..ProptestConfig::default()
    })]

    // Arbitrary bytes: unmarshal must never panic / over-read (Ok or Err both ok).
    #[test]
    fn fuzz_decode_never_overreads(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let _ = decode_never_overreads(&data);
    }

    // Structurally-plausible proto-ish prefixes (tag bytes followed by junk):
    // exercises the proto + zstd decode branches more deeply.
    #[test]
    fn fuzz_decode_tagged(tag in any::<u8>(), body in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let mut data = vec![tag];
        data.extend_from_slice(&body);
        let _ = decode_never_overreads(&data);
    }
}
