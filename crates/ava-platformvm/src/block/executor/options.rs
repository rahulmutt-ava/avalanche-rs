// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Oracle-block option generation (`vms/platformvm/block/executor/options.go`,
//! specs 08 §4.2).
//!
//! [`options`] produces the `(commit, abort)` children of a verified
//! `*ProposalBlock`: a `*CommitBlock` and an `*AbortBlock` over the same parent
//! (the proposal block id), at the proposal's height + 1, carrying the proposal's
//! timestamp for Banff blocks. The pair is returned `(preferred, alternate)` —
//! the commit block is preferred when the proposal executor reported
//! `prefers_commit` (the validator's measured uptime / advance-time always-commit),
//! else the abort block is preferred (Go falls back to commit on error to err on
//! the side of over-rewarding; here the preference is fixed at verify time).

use ava_database::Database;

use crate::block::executor::BlockManager;
use crate::block::{Block, BlockBody};
use crate::error::{Error, Result};

/// `Options(block)` — see [`BlockManager::options`](super::BlockManager::options).
pub(crate) fn options<D: Database + 'static>(
    mgr: &BlockManager<D>,
    block: &Block,
) -> Result<(Block, Block)> {
    if !block.is_proposal() {
        return Err(Error::WrongTxType);
    }
    let st = mgr
        .cached(block.id())
        .ok_or(Error::Database(ava_database::error::Error::NotFound))?;

    let blk_id = block.id();
    let next_height = block.height().checked_add(1).ok_or(Error::Overflow)?;
    let c = mgr.codec();

    let (commit, abort) = match block.body() {
        BlockBody::BanffProposal(b) => {
            let commit = Block::new_banff_commit(c, b.time, blk_id, next_height)?;
            let abort = Block::new_banff_abort(c, b.time, blk_id, next_height)?;
            (commit, abort)
        }
        BlockBody::ApricotProposal(_) => {
            let commit = Block::new_apricot_commit(c, blk_id, next_height)?;
            let abort = Block::new_apricot_abort(c, blk_id, next_height)?;
            (commit, abort)
        }
        _ => return Err(Error::WrongTxType),
    };

    if st.prefers_commit {
        Ok((commit, abort))
    } else {
        Ok((abort, commit))
    }
}
