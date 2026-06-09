// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Stable-toolchain smoke harness mirroring the nightly `cargo-fuzz` target
//! at `crates/ava-saevm/blocks/fuzz` (specs/11 §4.1, M7.31).
//!
//! The fuzz target in `fuzz/fuzz_targets/decode_block.rs` requires a nightly
//! compiler and LLVM sanitizers, so it cannot run on the pinned stable nix
//! shell. This proptest exercises the **same** invariants over proptest-generated
//! bytes:
//!
//! 1. **Decode-never-panics**: `parse_block` must not panic on any input.
//! 2. **Round-trip hash stability**: when decoding succeeds, re-RLP-encoding the
//!    decoded eth block and re-parsing it must yield the same block hash
//!    (`parse → re-encode → re-parse` is hash-stable).

#![allow(clippy::arithmetic_side_effects)] // test-only, SAE deny does not apply here.

use std::time::{Duration, UNIX_EPOCH};

use ava_evm_reth::rlp_encode;
use ava_saevm_blocks::parse_block;
use proptest::collection::vec;
use proptest::prelude::*;

/// Fixed "now" matching the fuzz target: year ~2100, so well-formed blocks
/// with past-ish timestamps pass the future-block guard.
const NOW_SECS: u64 = 4_102_444_800;

proptest! {
    /// Decode-never-panics over arbitrary bytes (mirrors the `decode_block` fuzz
    /// target). An `Err` is fine; a panic is not.
    #[test]
    fn parse_block_never_panics(data in vec(any::<u8>(), 0..512)) {
        let now = UNIX_EPOCH + Duration::from_secs(NOW_SECS);
        // Ignore the result: we are asserting no panic.
        let _ = parse_block(&data, now);
    }

    /// Round-trip hash stability: when `parse_block` succeeds, re-encoding the
    /// eth block and re-parsing must produce the same hash.
    #[test]
    fn parse_block_round_trip_hash_stable(data in vec(any::<u8>(), 0..512)) {
        let now = UNIX_EPOCH + Duration::from_secs(NOW_SECS);
        if let Ok(block) = parse_block(&data, now) {
            let reencoded = rlp_encode(block.eth_block().clone_block());
            if let Ok(block2) = parse_block(&reencoded, now) {
                prop_assert_eq!(
                    block2.hash(),
                    block.hash(),
                    "parse → re-encode → re-parse must be hash-stable"
                );
            }
        }
    }
}
