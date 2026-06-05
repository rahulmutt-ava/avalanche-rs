// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: CB58 encode/decode round-trip (specs/02 §8).
//!
//! Mirrors Go `formatting.FuzzEncodeDecode`: encoding then decoding any byte
//! string is the identity, and `cb58_decode` never panics on arbitrary input
//! (arbitrary UTF-8 strings are also fed through `cb58_decode` directly).

#![no_main]

use ava_utils::cb58::{cb58_decode, cb58_encode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Round-trip: decode(encode(data)) == data.
    if let Ok(encoded) = cb58_encode(data) {
        let decoded = cb58_decode(&encoded).expect("decoding our own encoding must succeed");
        assert_eq!(decoded.as_slice(), data, "cb58 round-trip mismatch");
    }

    // Decoding arbitrary (possibly non-base58) input must never panic.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = cb58_decode(s);
    }
});
