// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Block (de)serialization entrypoint (specs 09 §7).
//!
//! Port of `vms/avm/block/parser.go`. `parse(codec, bytes)` unmarshals the
//! type-tagged block bytes into a [`Block`] then initializes it (recomputing
//! `block_id = sha256(bytes)`, caching the raw bytes, and re-deriving each
//! contained tx's `tx_id`); see [`Block::parse`].
//!
//! Go's parser exposes `ParseBlock` (standard codec) and `ParseGenesisBlock`
//! (genesis codec); both delegate to the same `parse` with a different
//! [`Manager`]. The codec is passed explicitly here for the same reason.

use ava_codec::error::Result as CodecResult;
use ava_codec::manager::Manager;

use crate::block::Block;

/// `block.parse` / `ParseBlock` — decode a type-tagged block from `bytes` and
/// initialize its derived caches (`block_id`, raw bytes, per-tx `tx_id`).
///
/// Pass [`Codec`](crate::txs::codec::Codec) for ordinary blocks and
/// [`GenesisCodec`](crate::txs::codec::GenesisCodec) for the genesis block.
///
/// # Errors
/// Returns a [`ava_codec::error::CodecError`] if the bytes fail to decode or a
/// contained tx fails to (re-)initialize.
pub fn parse(c: &Manager, bytes: &[u8]) -> CodecResult<Block> {
    Block::parse(c, bytes)
}
