// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: decode-never-panics over arbitrary bytes for the X-Chain block,
//! tx, and operation decoders (`specs/02` §8, §13.5).
//!
//! Drives `block::Block::parse` and `txs::Tx::parse` (which themselves recurse
//! through the unsigned-tx, fx-output/input, operation, and credential decoders)
//! with arbitrary input, asserting only that decoding never panics — errors are
//! expected and ignored. The block parser is exercised with both the standard and
//! genesis (oversized-max) codec managers.
//!
//! Where decoding succeeds, a cheap round-trip is asserted (`parse → bytes`),
//! guarded behind `if let Ok(..)`: `parse` re-caches the exact input as the
//! decoded value's bytes, so the cached bytes must equal the fuzz input.

#![no_main]

use libfuzzer_sys::fuzz_target;

use ava_avm::block::Block;
use ava_avm::txs::Tx;
use ava_avm::txs::codec::{Codec, GenesisCodec};

fuzz_target!(|data: &[u8]| {
    // Decode-never-panics over the block parser (default-max-size codec). The
    // block decoder recurses into the contained-tx / fx-op decoders.
    if let Ok(blk) = Block::parse(Codec(), data) {
        // `parse` caches the input bytes verbatim; round-trip must agree.
        assert_eq!(blk.bytes(), data);
    }

    // ...and the genesis (oversized-max) codec manager.
    let _ = Block::parse(GenesisCodec(), data);

    // ...and over the signed-tx parser (recurses into the unsigned-tx, output,
    // input, operation, and credential decoders).
    if let Ok(tx) = Tx::parse(Codec(), data) {
        assert_eq!(tx.bytes(), data);
    }
    let _ = Tx::parse(GenesisCodec(), data);
});
