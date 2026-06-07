// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-Chain block builder (`vms/platformvm/block/builder/builder.go`,
//! specs 08 §4.3).
//!
//! Mirrors Go's `buildBlock`: given the preferred block's parent `Chain` view,
//! its `(id, height)`, and the resolved next block time, decide what block (if
//! any) to issue:
//!
//! 1. **Reward proposal** — if a non-permissioned current staker's `next_time`
//!    (= its `end_time`) equals the new chain time, issue a [`BanffProposalBlock`]
//!    carrying a [`RewardValidatorTx`] for that staker (this advances the chain
//!    time + removes/rewards the staker via the oracle commit/abort). This is
//!    prioritized first so the timestamp advances as quickly as possible.
//! 2. **Standard block** — otherwise, if there are pending decision txs *or* the
//!    time needs advancing, issue a [`BanffStandardBlock`] of as many mempool
//!    decision txs as fit under the size/gas cap.
//! 3. **`ErrNoPendingBlocks`** — if neither applies (no txs and the time does not
//!    need advancing), decline to build.
//!
//! ## Scope (M4.25, read-only sync)
//!
//! The full Etna gas-aware `packEtnaBlockTxs` / Durango `packDurangoBlockTxs`
//! mempool packing (`builder.go`) and the gossip mempool itself are **M4.26**.
//! This builder takes the decision txs to pack as an explicit slice (the VM
//! supplies them from its minimal in-VM queue, empty during read-only sync) and
//! caps them by [`TARGET_BLOCK_SIZE`]; the reward-proposal / advance-time /
//! `ErrNoPendingBlocks` control flow is faithful to Go.

use std::time::SystemTime;

use ava_codec::manager::Manager;
use ava_types::id::Id;

use crate::block::apricot::{ApricotProposalBlock, ApricotStandardBlock, CommonBlock};
use crate::block::banff::{BanffProposalBlock, BanffStandardBlock};
use crate::block::{Block, BlockBody};
use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::txs::{Priority, RewardValidatorTx, Tx, UnsignedTx};

/// `targetBlockSize` — the soft cap (in bytes of serialized txs) a standard
/// block packs before stopping (Go `builder.targetBlockSize`, 128 KiB).
pub const TARGET_BLOCK_SIZE: usize = 128 * 1024;

/// The outcome of [`build_block`]: the block to issue, or
/// [`Error::NoPendingBlocks`] when the VM should decline.
///
/// `parent_state` is the resolved [`Chain`] view of the preferred block (its
/// on-accept diff / the base snapshot); `timestamp` is the already-resolved new
/// block time (`min(max(now, parent_ts), next_staker_change)`, clamped by the
/// sync bound — computed by the caller, [`next_block_time`]).
///
/// `decision_txs` are the mempool decision txs to pack (empty during read-only
/// sync); they are capped by [`TARGET_BLOCK_SIZE`] here.
///
/// # Errors
/// Returns [`Error::NoPendingBlocks`] if there is nothing to do (no reward due,
/// no decision txs, and the time does not need advancing), or a codec error if
/// the block fails to initialize.
pub fn build_block(
    codec: &Manager,
    parent_id: Id,
    height: u64,
    timestamp: SystemTime,
    force_advance_time: bool,
    parent_state: &dyn Chain,
    decision_txs: Vec<Tx>,
) -> Result<Block> {
    let block_txs = pack_decision_txs(decision_txs, TARGET_BLOCK_SIZE);
    let time_secs = unix_secs(timestamp);

    // 1) Try rewarding a staker whose period ends at the new chain time. Done
    //    first to prioritize advancing the timestamp (Go `buildBlock`).
    if let Some(staker_tx_id) = next_staker_to_reward(timestamp, parent_state) {
        let reward_tx = Tx::new(UnsignedTx::RewardValidator(RewardValidatorTx {
            tx_id: staker_tx_id,
        }));
        let mut blk = Block::new(BlockBody::BanffProposal(BanffProposalBlock {
            time: time_secs,
            transactions: block_txs,
            apricot: ApricotProposalBlock {
                common: CommonBlock { parent_id, height },
                tx: reward_tx,
            },
        }));
        blk.initialize(codec)?;
        return Ok(blk);
    }

    // 2) If there is no reason to build a block, don't (Go `ErrNoPendingBlocks`).
    if block_txs.is_empty() && !force_advance_time {
        return Err(Error::NoPendingBlocks);
    }

    // 3) Issue a standard block with as many decision txs as fit.
    let mut blk = Block::new(BlockBody::BanffStandard(BanffStandardBlock {
        time: time_secs,
        apricot: ApricotStandardBlock {
            common: CommonBlock { parent_id, height },
            transactions: block_txs,
        },
    }));
    blk.initialize(codec)?;
    Ok(blk)
}

/// `getNextStakerToReward` — the tx id of the next current staker to remove via a
/// [`RewardValidatorTx`], iff its `end_time` equals `chain_timestamp` (Go
/// `getNextStakerToReward`).
///
/// Walks the current stakers in canonical `(next_time, priority, tx_id)` order
/// (the `state.Chain` iterator order); the first **non-permissioned** staker is
/// the next reward candidate (permissioned subnet validators leave via an
/// advance-time change, not a reward). Returns `Some(tx_id)` only when that
/// staker's `next_time` (= `end_time` for a current staker) equals the new chain
/// time.
fn next_staker_to_reward(chain_timestamp: SystemTime, parent_state: &dyn Chain) -> Option<Id> {
    for staker in parent_state.current_stakers() {
        if staker.priority == Priority::SubnetPermissionedValidatorCurrent {
            continue;
        }
        // The first non-permissioned current staker: rewardable iff its removal
        // time is exactly the new chain time.
        if staker.next_time == chain_timestamp {
            return Some(staker.tx_id);
        }
        return None;
    }
    None
}

/// `state.NextBlockTime` (the builder's clamped form): the new block timestamp is
/// `min(max(now, parent_ts), next_staker_change)`, then clamped so it is no more
/// than `sync_bound` ahead of `now`.
///
/// Returns `(timestamp, time_was_capped)` where `time_was_capped` is `true` iff
/// the next staker change time forced the timestamp earlier than `now` would
/// allow — Go uses this to know the time *must* advance (`force_advance_time`).
#[must_use]
pub fn next_block_time(
    now: SystemTime,
    parent_timestamp: SystemTime,
    next_staker_change: SystemTime,
    sync_bound: std::time::Duration,
) -> (SystemTime, bool) {
    // The earliest valid next time is no earlier than the parent's time.
    let mut next = now.max(parent_timestamp);

    // It may not exceed the next staker change time (the chain must stop there
    // to process that staker). When it does, the time was capped.
    let mut time_was_capped = false;
    if next > next_staker_change {
        next = next_staker_change;
        time_was_capped = true;
    }

    // Clamp to at most `sync_bound` ahead of `now` (don't propose a block too
    // far in the future), but never earlier than the parent timestamp.
    let upper = now
        .checked_add(sync_bound)
        .unwrap_or(next)
        .max(parent_timestamp);
    if next > upper {
        next = upper;
    }

    (next, time_was_capped)
}

/// Packs `txs` into a block, stopping once the cumulative serialized size would
/// exceed `cap` (Go `packDurangoBlockTxs`'s size honoring). The byte size of a
/// tx is its cached codec `bytes` length.
fn pack_decision_txs(txs: Vec<Tx>, cap: usize) -> Vec<Tx> {
    let mut packed = Vec::with_capacity(txs.len());
    let mut size = 0usize;
    for tx in txs {
        let next = size.saturating_add(tx.bytes().len());
        if !packed.is_empty() && next > cap {
            break;
        }
        size = next;
        packed.push(tx);
    }
    packed
}

/// Seconds since the Unix epoch for `t` (saturating; `0` for pre-epoch).
fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
