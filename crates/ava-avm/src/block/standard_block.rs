// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain `StandardBlock` body (specs 09 §7).
//!
//! Port of `vms/avm/block/standard_block.go`. The serialized fields are, in
//! order: `PrntID` (parent id, 32 fixed bytes), `Hght` (`u64`), `Time` (`u64`),
//! `Root` (merkle root id, currently unused/zero), `Transactions` (`[]*txs.Tx`).
//! The Go `BlockID`/`bytes` cache fields are **not** serialized; the block ID is
//! derived by hashing the full codec bytes (see [`crate::block::Block`]).

use ava_codec::AvaCodec;
use ava_codec::error::Result as CodecResult;
use ava_codec::manager::Manager;
use ava_types::id::Id;

use crate::block::{Block, BlockBody};
use crate::txs::Tx;

/// `block.StandardBlock` — block `type_id` 20: a post-linearization Snowman block
/// carrying a list of transactions (specs 09 §7).
///
/// Field order matches `standard_block.go` byte-for-byte: `parent_id`, `height`,
/// `time`, `root`, `transactions`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct StandardBlock {
    /// `PrntID` — the parent block's ID.
    #[codec]
    pub parent_id: Id,
    /// `Hght` — this block's height (genesis is `0`).
    #[codec]
    pub height: u64,
    /// `Time` — the block's proposed wall-clock time, in Unix seconds.
    #[codec]
    pub time: u64,
    /// `Root` — the merkle root. Currently unused; always the zero id.
    #[codec]
    pub root: Id,
    /// `Transactions` — the transactions contained in this block.
    #[codec]
    pub transactions: Vec<Tx>,
}

impl StandardBlock {
    /// `block.NewStandardBlock` — builds a fresh, initialized X-Chain
    /// `StandardBlock` wrapped in the [`Block`] envelope (specs 09 §7).
    ///
    /// The block is marshaled as the `Block` interface (type-id-prefix 20) and its
    /// derived caches (`block_id = sha256(bytes)`, raw bytes) are populated; the
    /// `root` (merkle root) is left as the zero id, mirroring Go `NewStandardBlock`.
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if marshalling fails.
    pub fn new_block(
        c: &Manager,
        parent_id: Id,
        height: u64,
        time: u64,
        transactions: Vec<Tx>,
    ) -> CodecResult<Block> {
        let mut blk = Block::new(BlockBody::Standard(StandardBlock {
            parent_id,
            height,
            time,
            root: Id::EMPTY,
            transactions,
        }));
        blk.initialize(c)?;
        Ok(blk)
    }
}
