// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Settlement-range and `last_to_settle_at` tests (specs/11 §1.2/§4.2).
//!
//! Mirrors `vms/saevm/blocks/settlement_test.go` (`TestSettles`,
//! `TestLastToSettleAt`).

// Readable reference arithmetic + small-index casts in test chain builders; the
// loop counters are tiny constants, so truncation cannot occur here.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_evm_reth::{Header, RethBlock, SealedBlock};
use ava_saevm_blocks::{Block, Range, last_to_settle_at};
use ava_saevm_params::TAU;

/// Builds a sealed eth block at `number`/`timestamp` whose `parent_hash` points
/// at `parent` (used to chain blocks).
fn eth_block(
    number: u64,
    timestamp: u64,
    parent_hash: ava_evm_reth::B256,
) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        timestamp,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

/// Constructs a chain of `count` blocks (heights `0..count`), each settling the
/// block at `last_settled_height[height]`. Block 0 is synchronous (the genesis
/// / last pre-SAE block); the rest are normal SAE blocks.
fn build_chain(count: u64, last_settled_at: &[u64]) -> Vec<Arc<Block>> {
    let mut chain: Vec<Arc<Block>> = Vec::new();
    for height in 0..count {
        let parent_hash = if height == 0 {
            ava_evm_reth::B256::ZERO
        } else {
            chain[(height - 1) as usize].hash()
        };
        let eth = eth_block(height, height, parent_hash);
        if height == 0 {
            // Synchronous (self-settling) genesis.
            let b = Block::new(eth, None, None).expect("genesis");
            let b = Arc::new(b);
            b.mark_synchronous().expect("mark_synchronous");
            chain.push(b);
        } else {
            let parent = Arc::clone(&chain[(height - 1) as usize]);
            let last_settled = Arc::clone(&chain[last_settled_at[height as usize] as usize]);
            let b = Block::new(eth, Some(parent), Some(last_settled)).expect("block");
            chain.push(Arc::new(b));
        }
    }
    chain
}

#[test]
fn range_identical_blocks_is_empty() {
    let chain = build_chain(4, &[0, 0, 1, 2]);
    let b = &chain[3];
    let got = Range::between(b.last_settled(), b.last_settled());
    assert!(got.is_empty(), "Range(x, x) must be empty");
}

#[test]
fn settles_returns_half_open_range_to_last_settled() {
    // last_settled_at[h]: which height block h settles up to.
    // 0:sync, 1->0, 2->0, 3->1, 4->2, 5->3
    let chain = build_chain(6, &[0, 0, 0, 1, 2, 3]);

    // Block 4 settles (parent.last_settled=block1, self.last_settled=block2] = {2}
    let settled4 = chain[4].settles();
    let heights4: Vec<u64> = settled4.iter().map(|b| b.height()).collect();
    assert_eq!(heights4, vec![2], "block 4 settles (1,2] = {{2}}");

    // Block 5 settles (parent.last_settled=block2, self.last_settled=block3] = {3}
    let settled5 = chain[5].settles();
    let heights5: Vec<u64> = settled5.iter().map(|b| b.height()).collect();
    assert_eq!(heights5, vec![3], "block 5 settles (2,3] = {{3}}");
}

#[test]
fn settles_synchronous_block_is_self() {
    let chain = build_chain(2, &[0, 0]);
    let settled = chain[0].settles();
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].height(), 0, "synchronous block settles itself");
}

#[test]
fn range_spans_multiple_blocks_in_height_order() {
    let chain = build_chain(8, &[0, 0, 0, 0, 0, 0, 0, 0]);
    // Range(block3, block7] = {4,5,6,7}
    let got = Range::between(Some(Arc::clone(&chain[3])), Some(Arc::clone(&chain[7])));
    let heights: Vec<u64> = got.iter().map(|b| b.height()).collect();
    assert_eq!(heights, vec![4, 5, 6, 7]);
}

#[test]
fn last_to_settle_known_when_execution_caught_up() {
    // genesis(sync) + 2 normal blocks, none executed yet.
    let chain = build_chain(3, &[0, 0, 0]);
    let parent = Arc::clone(&chain[2]);

    // settleAt long in the past => the only block that can settle is the
    // synchronous genesis (always settled). known=true (we walked to a settled
    // block).
    let settle_at = UNIX_EPOCH; // earlier than any block time.
    let (b, ok) = last_to_settle_at(settle_at, &parent).expect("last_to_settle_at");
    assert!(ok, "settled-genesis terminus => known");
    assert_eq!(b.expect("some block").height(), 0);
}

#[test]
fn last_to_settle_unknown_when_execution_lags() {
    // genesis(sync) at t=0, blocks 1 & 2 at t=1,2. Neither 1 nor 2 executed.
    let chain = build_chain(3, &[0, 0, 0]);
    let parent = Arc::clone(&chain[2]);

    // settleAt is AFTER block 1's build time but block 1 has not executed and
    // has no interim time => we cannot know it has settled => ok=false.
    let settle_at = UNIX_EPOCH + Duration::from_secs(5);
    let (_b, ok) = last_to_settle_at(settle_at, &parent).expect("last_to_settle_at");
    assert!(!ok, "execution lagging => settlement not yet known");
}

#[test]
fn last_to_settle_uses_block_time_minus_tau_discipline() {
    // Smoke: building settle_at via Duration ops (TAU) compiles & runs; the
    // BlockInstant type forbids raw-second arithmetic so this is the only path.
    let chain = build_chain(2, &[0, 0]);
    let parent = Arc::clone(&chain[1]);
    let block_time = SystemTime::now();
    let settle_at = block_time.checked_sub(TAU).unwrap_or(UNIX_EPOCH);
    // Genesis is settled and earlier than settle_at, so this resolves.
    let (b, _ok) = last_to_settle_at(settle_at, &parent).expect("last_to_settle_at");
    assert!(b.is_some());
}
