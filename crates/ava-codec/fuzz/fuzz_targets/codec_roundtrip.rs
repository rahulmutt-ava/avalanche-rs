// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: decode-every-byte + round-trip differential for the linear codec.
//!
//! Drives `ava_codec::Manager::unmarshal` over arbitrary bytes
//! (`decode_never_panics`) for a concrete registered type; when a decode
//! succeeds, re-marshals the decoded value and asserts the bytes round-trip
//! identically (the manager's trailing-byte check guarantees a clean decode
//! consumed the whole input, so the canonical re-encoding must equal the input).
//! See `specs/02-testing-strategy.md` §8.

#![no_main]

use std::sync::Arc;

use ava_codec::AvaCodec;
use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use libfuzzer_sys::fuzz_target;

const VERSION: u16 = 0;

#[derive(AvaCodec, Default, Debug, PartialEq, Eq, Clone)]
struct Pair {
    #[codec]
    a: u16,
    #[codec]
    b: u32,
}

/// A type covering several wire kinds: a tagged int, a fixed array, a `Vec<u8>`,
/// a `String`, and a `Vec<T>` of a non-`u8` element.
#[derive(AvaCodec, Default, Debug, PartialEq, Eq, Clone)]
struct Repr {
    #[codec]
    tag: u32,
    #[codec]
    id: [u8; 8],
    #[codec]
    blob: Vec<u8>,
    #[codec]
    name: String,
    #[codec]
    items: Vec<Pair>,
}

fuzz_target!(|data: &[u8]| {
    let m = Manager::with_default_max_size();
    // `register` only fails on a duplicate version; a fresh manager never does.
    let _ = m.register(VERSION, Arc::new(LinearCodec::new()));

    let mut decoded = Repr::default();
    // decode-never-panics: this must return Ok/Err, never panic.
    if m.unmarshal(data, &mut decoded).is_ok() {
        // round-trip differential: a clean decode (whole input consumed) must
        // re-marshal to byte-identical output.
        let re = m
            .marshal(VERSION, &decoded)
            .expect("re-marshal of a decoded value must succeed");
        assert_eq!(re.as_slice(), data, "codec round-trip mismatch");
    }
});
