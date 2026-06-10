// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Three-frontier (S/E/A) + settlement-driver tests (specs/11 §1.1/§1.2, §10
//! invariants 1/2/5/9).
//!
//! Mirrors the consensus-state half of the Go reference (`blocks/access.go::
//! Frontier`, `settlement.go`). The full VM lifecycle (build / verify / accept)
//! is exercised by M7.18, not here — these tests drive the frontiers and the
//! `settle()` driver directly over hand-built block chains.

// Readable reference arithmetic + small-index casts in the test chain builders;
// the loop counters are tiny constants, so truncation cannot occur here.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::sync::Arc;

use ava_evm_reth::{B256, Header, RethBlock, SealedBlock};
use ava_saevm_blocks::{Block, ExecutionArtefacts, LifeCycleStage};
use ava_saevm_core::{Frontier, settle};
use ava_saevm_params::TAU;
use ava_saevm_proxytime::Time;
use ava_saevm_types::ExecutionResults;
use ava_vm::components::gas::Price;

// ---------------------------------------------------------------------------
// Chain builders
// ---------------------------------------------------------------------------

/// Builds a sealed eth block at `number`/`timestamp` whose `parent_hash` points
/// at `parent_hash` (used to chain blocks).
fn eth_block(number: u64, timestamp: u64, parent_hash: B256) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        timestamp,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

/// A genesis (synchronous, self-settling) SAE block at height 0, timestamp 0.
fn genesis() -> Arc<Block> {
    let g = Arc::new(Block::new(eth_block(0, 0, B256::ZERO), None, None).expect("genesis"));
    g.mark_synchronous().expect("mark synchronous");
    g
}

/// Synthetic execution results whose gas-time resolves (via `as_time`) to
/// `epoch + at_unix` — a unit-rate clock pins `as_time` to the seconds field.
fn results_at(at_unix: u64) -> ExecutionResults {
    ExecutionResults {
        gas_time: Time::<u64>::new(at_unix, 0, 1),
        base_fee: Price(1),
        receipt_root: B256::ZERO,
        post_state_root: B256::repeat_byte(0x33),
    }
}

/// Marks `block` executed with a gas-time at `exec_unix`.
fn mark_executed_at(block: &Arc<Block>, exec_unix: u64) {
    let results = results_at(exec_unix);
    let artefacts = ExecutionArtefacts {
        interim_execution_time: results.gas_time.clone(),
        results,
    };
    block.mark_executed(artefacts, None).expect("mark executed");
}

/// A linear chain rooted at a synchronous genesis. Each non-genesis block `h`
/// is built at timestamp `h`, with `parent = chain[h-1]` and a chosen
/// `last_settled = chain[last_settled_at[h]]`. Returns the chain in increasing
/// height order (`chain[0]` is the genesis).
fn build_chain(count: u64, last_settled_at: &[u64]) -> Vec<Arc<Block>> {
    let mut chain: Vec<Arc<Block>> = Vec::new();
    for height in 0..count {
        if height == 0 {
            chain.push(genesis());
            continue;
        }
        let parent = Arc::clone(&chain[(height - 1) as usize]);
        let last_settled = Arc::clone(&chain[last_settled_at[height as usize] as usize]);
        let eth = eth_block(height, height, parent.hash());
        let b = Arc::new(Block::new(eth, Some(parent), Some(last_settled)).expect("block"));
        chain.push(b);
    }
    chain
}

// ---------------------------------------------------------------------------
// (1) Frontier ordering: height(S) <= height(E) <= height(A) — invariant 1.
// ---------------------------------------------------------------------------

#[test]
fn frontier_ordering_s_le_e_le_a() {
    // 0:sync; 1..5 each settle the block TAU(=5s) before their build time, i.e.
    // last_settled[h] settles to genesis until the gap reopens. We just need a
    // valid ancestry; the settle() call below decides what actually settles.
    let chain = build_chain(6, &[0, 0, 0, 0, 0, 0]);
    let frontier = Frontier::new(Arc::clone(&chain[0]));

    // Walk the chain: accept each block, execute it (E advances), then attempt
    // to settle. At every step the height ordering must hold.
    for h in 1..6u64 {
        let b = &chain[h as usize];
        frontier.advance_accepted(b);
        assert!(frontier.heights_ordered(), "after accept h={h}: S<=E<=A");

        // Execution lands ~Tau (5s) after the block's build time on the gas
        // clock, so block h is "finished" at gas-time h (its timestamp). Mark it
        // executed at its own timestamp so settle_at = BlockTime - Tau picks up
        // sufficiently-old ancestors.
        mark_executed_at(b, h);
        frontier.advance_executed(b);
        assert!(frontier.heights_ordered(), "after exec h={h}: S<=E<=A");

        // Try to settle on behalf of the freshly-accepted block.
        let _ = settle(&frontier, b);
        assert!(frontier.heights_ordered(), "after settle h={h}: S<=E<=A");
    }

    // After processing the whole chain the E/A frontiers are at the tip and the
    // ordering invariant holds end-to-end.
    assert!(frontier.heights_ordered(), "S<=E<=A at the tip");
    assert_eq!(frontier.last_executed().expect("E").height(), 5);
    assert_eq!(frontier.last_accepted().height(), 5);
}

// ---------------------------------------------------------------------------
// (2) Stage causality: settled => executed => accepted — invariant 2.
// ---------------------------------------------------------------------------

#[test]
fn stage_causality_settle_implies_exec_implies_accept() {
    let chain = build_chain(8, &[0, 0, 0, 0, 0, 0, 0, 0]);
    let frontier = Frontier::new(Arc::clone(&chain[0]));

    for h in 1..8u64 {
        let b = &chain[h as usize];
        frontier.advance_accepted(b);
        mark_executed_at(b, h);
        frontier.advance_executed(b);
        let _ = settle(&frontier, b);
    }

    // Every block at or below S must be Settled (=> Executed => Accepted).
    let s = frontier.last_settled().height();
    let e = frontier.last_executed().expect("E").height();
    let a = frontier.last_accepted().height();
    for h in 0..8u64 {
        let b = &chain[h as usize];
        if h <= s {
            assert_eq!(b.stage(), LifeCycleStage::Settled, "h={h} <= S is settled");
        }
        if b.stage() == LifeCycleStage::Settled {
            assert!(b.executed(), "settled => executed (h={h})");
            assert!(h <= a, "settled => accepted (h={h})");
        }
        if b.executed() {
            assert!(h <= a, "executed => accepted (h={h})");
        }
    }
    assert!(s <= e && e <= a, "S<=E<=A heights");
}

// ---------------------------------------------------------------------------
// (3) settle marks Σ_n in increasing height — invariant 5.
// ---------------------------------------------------------------------------

#[test]
fn settle_in_increasing_height() {
    // A chain where blocks become settle-eligible all at once: build them all,
    // execute them all at low gas-times, then accept a far-future tip so its
    // settle_at sweeps the whole range in one call.
    let chain = build_chain(6, &[0, 0, 0, 0, 0, 0]);
    let frontier = Frontier::new(Arc::clone(&chain[0]));

    // Accept + execute 1..=4 at gas-time = their height (all well in the past).
    for h in 1..=4u64 {
        let b = &chain[h as usize];
        frontier.advance_accepted(b);
        mark_executed_at(b, h);
        frontier.advance_executed(b);
    }

    // Record the order in which mark_settled is observed by snapshotting heights
    // before/after a single settle() that sweeps multiple blocks. We accept a
    // tip (height 5) at a build time far enough that all of 1..=4 are eligible.
    // chain[5] is built at timestamp 5; its last_settled is genesis, so settle()
    // will choose the last ancestor finished <= (5 - Tau)=0 ... that's only
    // genesis. To actually sweep, build a tip far in the future:
    let tip_ts = 100u64;
    let parent = Arc::clone(&chain[4]);
    let tip = Arc::new(
        Block::new(
            eth_block(5, tip_ts, parent.hash()),
            Some(Arc::clone(&parent)),
            // The tip's last_settled is the last block known-settled at build:
            // pick block 4 so the candidate window is (genesis, 4].
            Some(Arc::clone(&chain[4])),
        )
        .expect("tip"),
    );
    frontier.advance_accepted(&tip);
    mark_executed_at(&tip, tip_ts);
    frontier.advance_executed(&tip);

    let before = frontier.last_settled().height();
    let newly = settle(&frontier, &tip).expect("settle sweeps eligible ancestors");
    let after = frontier.last_settled().height();

    assert!(after >= before, "S advanced monotonically");
    // The returned newly-settled range is in strictly increasing height order.
    let heights: Vec<u64> = newly.iter().map(|b| b.height()).collect();
    let mut sorted = heights.clone();
    sorted.sort_unstable();
    assert_eq!(heights, sorted, "Σ settled in increasing height order");
    assert!(
        heights.windows(2).all(|w| w[0] < w[1]),
        "strictly increasing (no repeats)",
    );
    // Several blocks settled at once (1..=4 are all old enough).
    assert_eq!(heights, vec![1, 2, 3, 4], "swept the whole eligible window");
    assert_eq!(after, 4, "S advanced to block 4");
}

// ---------------------------------------------------------------------------
// (4) consensus-critical map holds exactly the A..S window.
// ---------------------------------------------------------------------------

#[test]
fn consensus_critical_map_holds_a_to_s() {
    let chain = build_chain(8, &[0, 0, 0, 0, 0, 0, 0, 0]);
    let frontier = Frontier::new(Arc::clone(&chain[0]));

    // Accept + execute 1..=7. None are settled yet (nothing old enough relative
    // to their own build times under the static gas-times we hand them).
    for h in 1..=7u64 {
        let b = &chain[h as usize];
        frontier.advance_accepted(b);
        // Execute at a gas-time *equal to* the block's build time, so nothing is
        // Tau-old relative to the latest accept and the map stays full.
        mark_executed_at(b, h);
        frontier.advance_executed(b);
    }

    // Genesis is the LastSettled floor; the map holds (S, A] plus S itself,
    // i.e. every block from S up through A.
    let s = frontier.last_settled().height();
    let a = frontier.last_accepted().height();
    assert_eq!(s, 0, "nothing settled yet beyond genesis");
    assert_eq!(a, 7, "A at the tip");

    // The consensus-critical map holds exactly the blocks in [S, A].
    for h in 0..8u64 {
        let want = h >= s && h <= a;
        let hash = chain[h as usize].hash();
        assert_eq!(
            frontier.consensus_critical_block(hash).is_some(),
            want,
            "h={h} in A..S window? {want}",
        );
    }
    assert_eq!(
        frontier.consensus_critical_len(),
        (a - s + 1) as usize,
        "map size == window width",
    );

    // Now settle a far-future tip so S advances; settled blocks below the new S
    // drop out of the consensus-critical map.
    let parent = Arc::clone(&chain[7]);
    let tip = Arc::new(
        Block::new(
            eth_block(8, 100, parent.hash()),
            Some(Arc::clone(&parent)),
            Some(Arc::clone(&chain[5])),
        )
        .expect("tip"),
    );
    frontier.advance_accepted(&tip);
    mark_executed_at(&tip, 100);
    frontier.advance_executed(&tip);
    settle(&frontier, &tip).expect("settle");

    let new_s = frontier.last_settled().height();
    assert!(new_s > 0, "S advanced");
    // Blocks below the new S are no longer consensus-critical.
    for h in 0..new_s {
        assert!(
            frontier
                .consensus_critical_block(chain[h as usize].hash())
                .is_none(),
            "h={h} below S evicted from the consensus-critical map",
        );
    }
    // S itself and everything above through A remain.
    assert!(
        frontier
            .consensus_critical_block(frontier.last_settled().hash())
            .is_some(),
        "S retained in the map",
    );
}

// ---------------------------------------------------------------------------
// (5) settle() surfaces ErrExecutionLagging when known=false.
// ---------------------------------------------------------------------------

#[test]
fn settle_when_known_false_reports_execution_lagging() {
    // genesis(sync) at t=0; blocks 1 & 2 at t=1,2. Accept block 1 but DO NOT
    // execute it. Accept a tip whose settle_at is after block 1's build time —
    // last_to_settle_at cannot decide (block 1 might have settled but hasn't
    // executed) => known=false => ErrExecutionLagging.
    let chain = build_chain(3, &[0, 0, 0]);
    let frontier = Frontier::new(Arc::clone(&chain[0]));

    frontier.advance_accepted(&chain[1]);
    frontier.advance_accepted(&chain[2]);
    // chain[1] is NOT executed.

    // A tip built at timestamp Tau+1 (=6): settle_at = 6 - 5 = 1, which is >=
    // block 1's build time (1), so the candidate (block 1) "might have settled"
    // but has no execution result => lagging.
    let tip_ts = ava_saevm_params::TAU_SECONDS + 1;
    let parent = Arc::clone(&chain[2]);
    let tip = Arc::new(
        Block::new(
            eth_block(3, tip_ts, parent.hash()),
            Some(Arc::clone(&parent)),
            Some(Arc::clone(&chain[2])),
        )
        .expect("tip"),
    );
    frontier.advance_accepted(&tip);

    match settle(&frontier, &tip) {
        Err(ava_saevm_core::SettleError::ExecutionLagging) => {}
        Err(other) => panic!("expected ExecutionLagging, got {other}"),
        Ok(_) => panic!("expected ExecutionLagging, settle returned Ok"),
    }
    // S did not advance prematurely.
    assert_eq!(frontier.last_settled().height(), 0, "S stayed at genesis");

    // Sanity: TAU is the discipline (5s) used for the settle_at computation.
    assert_eq!(TAU.as_secs(), 5);
}

// ---------------------------------------------------------------------------
// (gauge) `sae` last_settled_height tracks S (specs/18 §2.11, Go 844535b313).
// ---------------------------------------------------------------------------

#[test]
fn last_settled_height_gauge_tracks_s_frontier() {
    let chain = build_chain(6, &[0, 0, 0, 0, 0, 1]);
    let frontier = Frontier::new(Arc::clone(&chain[0]));

    // Starts at the genesis (recovered S frontier) height.
    assert_eq!(frontier.last_settled_height(), 0, "gauge starts at genesis");

    for h in 1..6u64 {
        let b = &chain[h as usize];
        frontier.advance_accepted(b);
        mark_executed_at(b, h);
        frontier.advance_executed(b);
        let _ = settle(&frontier, b);
        // The gauge equals the S frontier height at every step.
        assert_eq!(
            frontier.last_settled_height(),
            frontier.last_settled().height(),
            "gauge == S height after settle h={h}",
        );
    }
}
