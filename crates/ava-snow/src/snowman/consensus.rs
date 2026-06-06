// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`SnowmanConsensus`] trait (specs 06 §2.4; Go
//! `snow/consensus/snowman/consensus.go`).

use std::sync::Arc;

use ava_types::id::Id;
use ava_utils::bag::Bag;

use super::block::Block;
use crate::error::Result;

/// A general Snowman instance processing a series of dependent decisions
/// (linear chain). Mirrors Go `snowman.Consensus` minus the metrics-only
/// `Initialize`/health surface, which is folded into the concrete
/// [`Topological`](super::topological::Topological) constructor and
/// `health_check`.
pub trait SnowmanConsensus {
    /// The number of blocks currently processing (excludes the last accepted).
    fn num_processing(&self) -> usize;

    /// Adds a new block. Must not be called twice with the same block; the
    /// parent must be the last accepted block or a processing block.
    ///
    /// # Errors
    /// [`Error::DuplicateAdd`](crate::error::Error::DuplicateAdd) if the block
    /// is already processing;
    /// [`Error::UnknownParentBlock`](crate::error::Error::UnknownParentBlock) if
    /// the parent is unknown.
    fn add(&mut self, block: Arc<dyn Block>) -> Result<()>;

    /// Whether `block_id` is currently processing.
    fn processing(&self, block_id: Id) -> bool;

    /// Whether `block_id` is preferred (the last accepted block or a processing
    /// block on the preferred branch).
    fn is_preferred(&self, block_id: Id) -> bool;

    /// The id and height of the last accepted decision.
    fn last_accepted(&self) -> (Id, u64);

    /// The id of the tail of the strongly preferred sequence of decisions.
    fn preference(&self) -> Id;

    /// The strongly preferred decision at `height`, if tracked.
    fn preference_at_height(&self, height: u64) -> Option<Id>;

    /// Records the results of a network poll. Assumes all decisions have been
    /// previously added.
    ///
    /// # Errors
    /// Propagates any critical accept/reject error from a finalized block.
    fn record_poll(&mut self, votes: &Bag<Id>) -> Result<()>;

    /// The parent of `id`, if known.
    fn get_parent(&self, id: Id) -> Option<Id>;
}
