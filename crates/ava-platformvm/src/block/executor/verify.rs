// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The block verifier (`vms/platformvm/block/executor/verifier.go`, specs 08
//! §4.2). [`verify`] dispatches on the block variant, executes its txs against a
//! fresh [`Diff`] layered over the parent, and caches the resulting diff(s) so
//! [`accept`](super::acceptor::accept) / [`options`](super::options::options) can
//! consume them.
//!
//! ## Scope (M4.20, read-only sync)
//!
//! The reference port covers the post-Banff oracle that linear sync exercises:
//! [`BanffStandardBlock`], [`BanffProposalBlock`] (commit/abort pair), and the
//! [`BanffCommitBlock`]/[`BanffAbortBlock`] options, plus the Apricot proposal /
//! commit / abort / standard variants for completeness. The legacy
//! `ApricotAtomicBlock` path needs the real `chains/atomic` `SharedMemory` (still
//! a deferred seam — see `txs/executor/atomic_tx_executor.rs`) and is rejected
//! here with [`Error::WrongTxType`]; the block builder (M4.25) and full warp
//! re-verify (M4.21/M4.22) are out of scope.

use std::sync::Arc;

use ava_database::Database;
use ava_types::id::Id;

use crate::block::executor::{BlockManager, BlockState};
use crate::block::{Block, BlockBody};
use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::diff::Diff;
use crate::txs::executor::{ProposalTxExecutor, RewardedStakerTx, StandardTxExecutor};
use crate::txs::{Tx, UnsignedTx};

/// Verifies `block` and caches its resulting diff(s) (Go `verifier.Visit`).
pub(crate) fn verify<D: Database + 'static>(
    mgr: &mut BlockManager<D>,
    block: &Block,
) -> Result<()> {
    let parent_id = block.parent_id();
    let parent = mgr
        .get_state_for_verify(parent_id)
        .ok_or(Error::Database(ava_database::error::Error::NotFound))?;

    // Block height must be parent height + 1.
    let expected_height = mgr
        .parent_height(parent_id)?
        .checked_add(1)
        .ok_or(Error::Overflow)?;
    if block.height() != expected_height {
        return Err(Error::WrongTxType);
    }

    match block.body() {
        BlockBody::BanffStandard(_) | BlockBody::ApricotStandard(_) => {
            verify_standard(mgr, block, parent_id, parent)
        }
        BlockBody::BanffProposal(_) | BlockBody::ApricotProposal(_) => {
            verify_proposal(mgr, block, parent_id, parent)
        }
        BlockBody::BanffCommit(_) | BlockBody::ApricotCommit(_) => {
            verify_option(mgr, block, parent_id, /* commit */ true)
        }
        BlockBody::BanffAbort(_) | BlockBody::ApricotAbort(_) => {
            verify_option(mgr, block, parent_id, /* commit */ false)
        }
        // Legacy ApricotAtomicBlock path requires the real shared-memory seam.
        BlockBody::ApricotAtomic(_) => Err(Error::WrongTxType),
    }
}

/// `verifier.standardBlock` — execute the decision txs against a single accept
/// diff and cache it.
fn verify_standard<D: Database + 'static>(
    mgr: &mut BlockManager<D>,
    block: &Block,
    parent_id: Id,
    _parent: Arc<dyn Chain>,
) -> Result<()> {
    let mut diff = mgr.new_diff(parent_id)?;
    advance_to_block_time(mgr, block, &mut diff)?;

    let codec = mgr.codec();
    {
        let backend = mgr.backend();
        for tx in block.txs() {
            let unsigned_bytes = codec
                .marshal(crate::CODEC_VERSION, &tx.unsigned)
                .map_err(Error::Codec)?;
            let mut exec = StandardTxExecutor::new(backend, &mut diff, tx, unsigned_bytes);
            tx.unsigned.visit(&mut exec)?;
            // The standard executor records its diff mutations directly; the
            // deferred on-accept callbacks (create-chain) and atomic requests are
            // the block-builder / shared-memory seams (M4.18/M4.25), not needed by
            // read-only sync.
            drop(exec.into_outputs());
            // Record the tx into the diff's tx store so the reward path can
            // resolve a staker later.
            diff.add_tx(tx.id(), tx.bytes().to_vec());
        }
    }

    let timestamp = diff_timestamp(&diff);
    mgr.cache(
        block.id(),
        BlockState {
            height: block.height(),
            on_accept: Some(Arc::new(diff)),
            on_commit: None,
            on_abort: None,
            timestamp,
            prefers_commit: true,
        },
    );
    Ok(())
}

/// `verifier.proposalBlock` — build the commit/abort diff pair, execute the
/// proposal tx against both, and cache them (08 §4.2).
fn verify_proposal<D: Database + 'static>(
    mgr: &mut BlockManager<D>,
    block: &Block,
    parent_id: Id,
    parent: Arc<dyn Chain>,
) -> Result<()> {
    // Banff proposal blocks carry decision txs executed against a decision diff
    // shared by both option diffs; post-Banff the only proposal tx is
    // RewardValidatorTx and there are no decision txs, so the decision diff is the
    // parent for the commit/abort pair.
    let mut on_commit = mgr.new_diff(parent_id)?;
    let mut on_abort = mgr.new_diff(parent_id)?;
    advance_to_block_time(mgr, block, &mut on_commit)?;
    advance_to_block_time(mgr, block, &mut on_abort)?;

    let proposal_tx = proposal_tx(block).ok_or(Error::WrongTxType)?;

    let resolver_parent = Arc::clone(&parent);
    let num_creds = proposal_tx.creds.len();
    let prefers_commit = {
        let backend = mgr.backend();
        let codec = mgr.codec();
        let resolver = BlockManager::<D>::staker_tx_resolver(codec, &resolver_parent);
        let mut exec =
            ProposalTxExecutor::new(backend, &mut on_commit, &mut on_abort, num_creds, &resolver);
        proposal_tx.unsigned.visit(&mut exec)?;
        exec.prefers_commit()
    };

    let timestamp = diff_timestamp(&on_abort);
    mgr.cache(
        block.id(),
        BlockState {
            height: block.height(),
            on_accept: None,
            on_commit: Some(Arc::new(on_commit)),
            on_abort: Some(Arc::new(on_abort)),
            timestamp,
            prefers_commit,
        },
    );
    Ok(())
}

/// `verifier.commitBlock` / `verifier.abortBlock` — bind the option block to the
/// parent proposal's commit/abort diff (08 §4.2).
fn verify_option<D: Database + 'static>(
    mgr: &mut BlockManager<D>,
    block: &Block,
    parent_id: Id,
    commit: bool,
) -> Result<()> {
    let parent_state = mgr
        .cached(parent_id)
        .ok_or(Error::Database(ava_database::error::Error::NotFound))?;
    let chosen = if commit {
        parent_state.on_commit.clone()
    } else {
        parent_state.on_abort.clone()
    }
    .ok_or(Error::Database(ava_database::error::Error::NotFound))?;
    let timestamp = parent_state.timestamp;

    mgr.cache(
        block.id(),
        BlockState {
            height: block.height(),
            on_accept: Some(chosen),
            on_commit: None,
            on_abort: None,
            timestamp,
            prefers_commit: true,
        },
    );
    Ok(())
}

/// Advances the diff's chain time to the Banff block's `Time` (Apricot blocks
/// carry no time; their chain time is the parent's, already inherited).
fn advance_to_block_time<D: Database + 'static>(
    mgr: &BlockManager<D>,
    block: &Block,
    diff: &mut Diff,
) -> Result<()> {
    if let Some(t) = block.banff_timestamp() {
        let new_time = std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(t))
            .ok_or(Error::Overflow)?;
        crate::txs::executor::advance_time::advance_time_to(mgr.backend(), diff, new_time)?;
    }
    Ok(())
}

/// The single proposal tx of a proposal block (Apricot `tx` / Banff `apricot.tx`).
fn proposal_tx(block: &Block) -> Option<&Tx> {
    match block.body() {
        BlockBody::ApricotProposal(b) => Some(&b.tx),
        BlockBody::BanffProposal(b) => Some(&b.apricot.tx),
        _ => None,
    }
}

/// The diff's chain time as seconds since the Unix epoch.
fn diff_timestamp(diff: &Diff) -> u64 {
    diff.timestamp()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Projects a (parsed) staker tx into the reward-relevant fields the proposal
/// executor needs (Go `txs.ValidatorTx` accessors). Returns `None` for a tx that
/// is not a permissionless/legacy staker.
pub(crate) fn rewarded_staker_tx(tx: &Tx) -> Option<RewardedStakerTx> {
    match &tx.unsigned {
        UnsignedTx::AddValidator(v) => Some(RewardedStakerTx {
            outputs: v.base.outputs().to_vec(),
            stake: v.stake_outs.clone(),
            validation_rewards_owner: v.rewards_owner.clone(),
        }),
        UnsignedTx::AddPermissionlessValidator(v) => Some(RewardedStakerTx {
            outputs: v.base.outputs().to_vec(),
            stake: v.stake_outs.clone(),
            validation_rewards_owner: v.validator_rewards_owner.clone(),
        }),
        _ => None,
    }
}
