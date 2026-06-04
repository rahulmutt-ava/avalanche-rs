// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: decode-every-type + round-trip differential for the linear codec.
//!
//! TODO(M0.24): drive `ava_codec::Manager::unmarshal` over arbitrary bytes
//! (`decode_never_panics`) and re-marshal decoded values for a round-trip
//! differential, per `specs/02-testing-strategy.md` §8. Scaffolded as a no-op.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|_data: &[u8]| {
    // TODO(M0.24): exercise ava_codec decode/round-trip against `_data`.
});
