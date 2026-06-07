// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Block (de)serialization entrypoint (specs 08 §4.1).
//!
//! Port of `vms/platformvm/block/parse.go`. `Parse(codec, bytes)` unmarshals the
//! type-tagged block bytes into a [`Block`] then `initialize`s it (recomputing
//! `block_id = sha256(bytes)` and caching the raw bytes); see [`Block::parse`].

use ava_codec::error::Result as CodecResult;
use ava_codec::manager::Manager;

use crate::block::Block;

/// `block.Parse` — decode a type-tagged block from `bytes` and initialize its
/// derived caches (`block_id`, raw bytes).
///
/// The caller passes the codec explicitly because genesis blocks may exceed the
/// default [`Codec`](crate::block::codec::Codec) max size and must be parsed
/// with [`GenesisCodec`](crate::block::codec::GenesisCodec).
///
/// # Errors
/// Returns a [`ava_codec::error::CodecError`] if the bytes fail to decode.
pub fn parse(c: &Manager, bytes: &[u8]) -> CodecResult<Block> {
    Block::parse(c, bytes)
}
