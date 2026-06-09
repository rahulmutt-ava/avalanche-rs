// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: decode-never-panics over arbitrary bytes for the SAE block
//! decoder (`specs/11` §4.1, M7.31).
//!
//! Drives `ava_saevm_blocks::parse_block` with arbitrary input, asserting only
//! that decoding never panics — errors are expected and ignored.
//!
//! Where decoding succeeds, a round-trip stability invariant is asserted:
//! re-RLP-encoding the decoded eth block and re-parsing it with the same `now`
//! must yield a block whose hash equals the first (`parse → re-encode →
//! re-parse` is hash-stable). This guards the property that RLP-encoding and
//! keccak-hashing are deterministic and that `parse_block` does not mutate the
//! wire representation.

#![no_main]

use std::time::{Duration, UNIX_EPOCH};

use ava_evm_reth::rlp_encode;
use ava_saevm_blocks::parse_block;
use libfuzzer_sys::fuzz_target;

/// A fixed "now" far enough in the future that the future-block check rarely
/// rejects well-formed blocks found by the fuzzer (year ~2100 as Unix seconds).
const NOW_SECS: u64 = 4_102_444_800;

fuzz_target!(|data: &[u8]| {
    let now = UNIX_EPOCH + Duration::from_secs(NOW_SECS);

    // Decode-never-panics: arbitrary bytes must not cause a panic, only errors.
    if let Ok(block) = parse_block(data, now) {
        // Round-trip stability (specs/11 §4.1): re-RLP-encoding the decoded eth
        // block and re-parsing it must yield the same block hash.
        let reencoded = rlp_encode(block.eth_block().clone_block());
        if let Ok(block2) = parse_block(&reencoded, now) {
            assert_eq!(
                block2.hash(),
                block.hash(),
                "parse → re-encode → re-parse must be hash-stable"
            );
        }
    }
});
