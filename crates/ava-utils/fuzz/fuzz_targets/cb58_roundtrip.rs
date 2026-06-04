// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: CB58 encode/decode round-trip.
//!
//! TODO(M0.24): assert `cb58_decode(cb58_encode(data)) == data` and that
//! `cb58_decode` never panics on arbitrary input, per
//! `specs/02-testing-strategy.md` §8. Scaffolded as a no-op.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|_data: &[u8]| {
    // TODO(M0.24): exercise ava_utils::cb58 round-trip against `_data`.
});
