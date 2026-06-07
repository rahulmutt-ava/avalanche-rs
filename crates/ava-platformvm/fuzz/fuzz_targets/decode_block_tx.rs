// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: decode-never-panics over arbitrary bytes for the P-Chain block
//! and tx parsers (`specs/02` §8).
//!
//! Drives `block::Block::parse` and `txs::Tx::parse` with arbitrary input,
//! asserting only that decoding never panics (errors are expected and ignored).

#![no_main]

use libfuzzer_sys::fuzz_target;

use ava_platformvm::block::Block;
use ava_platformvm::txs::{self, Tx};

fuzz_target!(|data: &[u8]| {
    // Decode-never-panics over the block parser (default-max-size codec).
    let _ = Block::parse(txs::Codec(), data);
    // ...and over the signed-tx parser.
    let _ = Tx::parse(txs::Codec(), data);
});
