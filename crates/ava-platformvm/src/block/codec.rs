// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The block codec managers (specs 08 §2.1 / §4.1).
//!
//! Port of `vms/platformvm/block/codec.go`. The block codec and the tx codec
//! share **one** type-ID numbering space: the 5 Apricot block types occupy
//! IDs 0–4 and the 4 Banff block types occupy 29–32 (reserved in the tx
//! registry via `SkipRegistrations`, see [`crate::txs::codec`]). Because the
//! registries are identical, blocks are framed by the very same [`Manager`]s
//! the tx envelope uses — this module simply re-exports them under the
//! `block::` path to mirror Go's `block.Codec` / `block.GenesisCodec`.

use ava_codec::manager::Manager;

/// The process-wide default-max-size block codec manager (`block.Codec`).
///
/// Identical to [`crate::txs::Codec`]; used to parse and frame ordinary blocks.
#[must_use]
#[allow(non_snake_case)]
pub fn Codec() -> &'static Manager {
    crate::txs::codec::Codec()
}

/// The process-wide genesis block codec manager (`block.GenesisCodec`).
///
/// Identical to [`crate::txs::GenesisCodec`]; an `i32::MAX`-max-slice manager
/// for oversized genesis blocks. Per the Go doc comment it must **not** be used
/// to parse new, unverified blocks (those go through [`Codec`]).
#[must_use]
#[allow(non_snake_case)]
pub fn GenesisCodec() -> &'static Manager {
    crate::txs::codec::GenesisCodec()
}
