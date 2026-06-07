// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: decode-never-panics over arbitrary bytes for the P-Chain block
//! and tx parsers.
//!
//! Stub for M4.1 — wired to `Block::parse` / `Tx::parse` once those land in
//! M4.5 / M4.2 (then re-marshal + round-trip per `specs/02` §8). For now it only
//! exercises that arbitrary input does not panic the (empty) parse surface.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Touch the codec version so the harness links the crate; replaced by
    // `Block::parse(data)` / `Tx::parse(data)` decode-never-panics drivers in
    // M4.5 / M4.2.
    let _ = (ava_platformvm::CODEC_VERSION, data);
});
