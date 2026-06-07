// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The block acceptor (`vms/platformvm/block/executor/acceptor.go`, specs 08
//! §4.2) and the bootstrap accept-without-verify path (19 §2).
//!
//! [`accept`] flushes a verified block's selected diff down to the persisted
//! [`State`](crate::state::state::State), writes the staker weight/public-key
//! diffs at the block height, records the block + its txs, advances the
//! last-accepted / height singletons, and fires the validator-manager
//! notification — see [`BlockManager::commit_accept`].
//!
//! A `*ProposalBlock` is **not** written on accept: only its accepted child
//! (a commit/abort option block) writes state, so a node that shuts down between
//! the proposal and its child does not persist a non-decision block (Go
//! `acceptor.proposalBlock`).

use std::sync::Arc;

use ava_database::Database;

use crate::block::executor::BlockManager;
use crate::block::{Block, BlockBody};
use crate::error::{Error, Result};
use crate::state::diff::Diff;

/// `Accept(block)` — see [`BlockManager::accept`].
pub(crate) fn accept<D: Database + 'static>(
    mgr: &mut BlockManager<D>,
    block: &Block,
) -> Result<()> {
    let block_id = block.id();
    match block.body() {
        // A proposal block defers its write to its accepted child.
        BlockBody::ApricotProposal(_) | BlockBody::BanffProposal(_) => {
            // Confirm it was verified (its diffs are cached) before noting it.
            if mgr.cached(block_id).is_none() {
                return Err(Error::Database(ava_database::error::Error::NotFound));
            }
            mgr.note_proposal_accept(block_id);
            Ok(())
        }
        // An option block accepts its parent proposal first (which is a no-op
        // write), then applies its own (commit/abort) diff.
        BlockBody::ApricotCommit(_)
        | BlockBody::BanffCommit(_)
        | BlockBody::ApricotAbort(_)
        | BlockBody::BanffAbort(_) => accept_option(mgr, block),
        // Standard / atomic blocks apply their single accept diff.
        BlockBody::ApricotStandard(_) | BlockBody::BanffStandard(_) => accept_decision(mgr, block),
        BlockBody::ApricotAtomic(_) => accept_decision(mgr, block),
    }
}

/// Applies a decision (standard) block's cached accept diff.
fn accept_decision<D: Database + 'static>(mgr: &mut BlockManager<D>, block: &Block) -> Result<()> {
    let block_id = block.id();
    let diff = cached_accept_diff(mgr, block_id)?;
    mgr.commit_accept(block, diff.as_ref())?;
    mgr.free(block_id);
    Ok(())
}

/// Applies an option (commit/abort) block's chosen diff, freeing the parent
/// proposal afterwards.
fn accept_option<D: Database + 'static>(mgr: &mut BlockManager<D>, block: &Block) -> Result<()> {
    let block_id = block.id();
    let parent_id = block.parent_id();

    let diff = cached_accept_diff(mgr, block_id)?;
    mgr.commit_accept(block, diff.as_ref())?;

    // Both the parent proposal and this option are no longer needed.
    mgr.free(parent_id);
    mgr.free(block_id);
    Ok(())
}

/// Clones the cached on-accept diff for `block_id`, or errors if the block was
/// not verified.
fn cached_accept_diff<D: Database + 'static>(
    mgr: &BlockManager<D>,
    block_id: ava_types::id::Id,
) -> Result<Arc<Diff>> {
    mgr.cached(block_id)
        .and_then(|s| s.on_accept.clone())
        .ok_or(Error::Database(ava_database::error::Error::NotFound))
}

/// `accept_non_verifying(block)` — see [`BlockManager::accept_non_verifying`].
pub(crate) fn accept_non_verifying<D: Database + 'static>(
    mgr: &mut BlockManager<D>,
    block: &Block,
) -> Result<()> {
    // Re-run verification to materialize the accept diff, then accept normally.
    // For an option block this binds the parent proposal's commit/abort diff, so
    // bootstrap must feed the proposal before its child (the linear order it
    // fetches blocks in).
    mgr.verify(block)?;
    mgr.accept(block)
}
