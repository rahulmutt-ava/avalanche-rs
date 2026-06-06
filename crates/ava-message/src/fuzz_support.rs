// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared fuzz logic for `decode_never_overreads` (specs/02 §8).
//!
//! The fuzzed operation is defined **once** here so both the libfuzzer target
//! (`fuzz/fuzz_targets/decode_never_overreads.rs`) and the stable proptest smoke
//! harness (`tests/prop_fuzz_smoke.rs`) exercise identical code. Gated behind the
//! `fuzzing` feature (enabled by the `fuzz/` crate and the `--all-features`
//! verification gate).

use crate::codec::MsgBuilder;

/// Feeds arbitrary bytes to the unmarshal path. Must **never** panic, never read
/// past the buffer, and never allocate more than `MAX_MESSAGE_SIZE` (the zstd
/// decode path is bounded). Returns whether the bytes decoded to a valid message
/// (informational; the contract is "doesn't crash").
pub fn decode_never_overreads(data: &[u8]) -> bool {
    let mb = MsgBuilder::default();
    mb.unmarshal(data).is_ok()
}
