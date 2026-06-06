// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: `MsgBuilder::unmarshal` over arbitrary bytes must never panic,
//! never read past the buffer, and never allocate more than `MAX_MESSAGE_SIZE`
//! (M2.6, spec 02 §8 — "parse arbitrary wire frames; must never panic or
//! over-read").
//!
//! The check lives in `ava_message::fuzz_support::decode_never_overreads`,
//! shared with the stable `tests/prop_fuzz_smoke.rs` proptest.
//!
//! Requires a nightly toolchain + LLVM sanitizers (see `../README.md`):
//!   cargo +nightly fuzz run decode_never_overreads

#![no_main]

use ava_message::fuzz_support::decode_never_overreads;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode_never_overreads(data);
});
