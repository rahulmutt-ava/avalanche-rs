// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Lifecycle state-machine tests (specs/11 §4.2 / §10 invariants 3/5/8).
//!
//! Mirrors `vms/saevm/blocks/execution_test.go` + `settlement_test.go`.

#![allow(clippy::arithmetic_side_effects)] // readable reference arithmetic in tests.

use std::sync::Arc;

use ava_evm_reth::{B256, EMPTY_ROOT_HASH, Header, RethBlock, SealedBlock};
use ava_saevm_blocks::{Block, ExecutionArtefacts, LifeCycleStage, in_memory_block_count};
use ava_saevm_proxytime::Time;
use ava_saevm_types::ExecutionResults;
use ava_vm::components::gas::Price;

fn eth_block(number: u64, parent_hash: B256) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        timestamp: number,
        transactions_root: EMPTY_ROOT_HASH,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

fn sample_results() -> ExecutionResults {
    ExecutionResults {
        gas_time: Time::<u64>::new(7, 0, 2),
        base_fee: Price(1_000),
        receipt_root: B256::repeat_byte(0xab),
        post_state_root: B256::repeat_byte(0xcd),
    }
}

fn artefacts() -> ExecutionArtefacts {
    ExecutionArtefacts {
        results: sample_results(),
        interim_execution_time: Time::<u64>::new(7, 0, 2),
    }
}

/// Builds a genesis + child pair; genesis is synchronous.
fn genesis_and_child() -> (Arc<Block>, Arc<Block>) {
    let g = Arc::new(Block::new(eth_block(0, B256::ZERO), None, None).expect("genesis"));
    g.mark_synchronous().expect("sync");
    let child = Arc::new(
        Block::new(
            eth_block(1, g.hash()),
            Some(Arc::clone(&g)),
            Some(Arc::clone(&g)),
        )
        .expect("child"),
    );
    (g, child)
}

#[test]
fn mark_executed_then_mark_settled_clears_ancestry() {
    let (_g, child) = genesis_and_child();
    assert_eq!(child.stage(), LifeCycleStage::NotExecuted);
    assert!(
        child.parent_block().is_some(),
        "ancestry present pre-settle"
    );

    child
        .mark_executed(artefacts(), None)
        .expect("mark_executed");
    assert_eq!(child.stage(), LifeCycleStage::Executed);
    assert!(
        child.parent_block().is_some(),
        "ancestry present post-execute"
    );

    child.mark_settled(None).expect("mark_settled");
    assert_eq!(child.stage(), LifeCycleStage::Settled);
    // CAS ancestry -> None severs parent links for GC.
    assert!(
        child.parent_block().is_none(),
        "ancestry cleared after settle"
    );
    assert!(
        child.last_settled().is_none(),
        "last_settled cleared after settle"
    );
}

#[test]
fn mark_executed_is_idempotent() {
    let (_g, child) = genesis_and_child();
    child
        .mark_executed(artefacts(), None)
        .expect("first mark_executed");
    // Second call must fail (once-only).
    let err = child.mark_executed(artefacts(), None);
    assert!(err.is_err(), "second mark_executed must error");
    assert_eq!(child.stage(), LifeCycleStage::Executed);
}

#[test]
fn mark_settled_is_once_only() {
    let (_g, child) = genesis_and_child();
    child
        .mark_executed(artefacts(), None)
        .expect("mark_executed");
    child.mark_settled(None).expect("first mark_settled");
    let err = child.mark_settled(None);
    assert!(err.is_err(), "second mark_settled must error");
}

#[test]
fn in_memory_block_count_returns_to_baseline() {
    let baseline = in_memory_block_count();
    {
        let g = Block::new(eth_block(0, B256::ZERO), None, None).expect("genesis");
        let child = Block::new(eth_block(1, g.hash()), None, None).expect("child");
        assert_eq!(
            in_memory_block_count(),
            baseline + 2,
            "two live blocks bump the counter"
        );
        drop(child);
        drop(g);
    }
    assert_eq!(
        in_memory_block_count(),
        baseline,
        "Drop decrements the GC counter back to baseline"
    );
}

#[test]
fn executed_artefacts_readable_after_mark() {
    let (_g, child) = genesis_and_child();
    child
        .mark_executed(artefacts(), None)
        .expect("mark_executed");
    assert_eq!(child.post_execution_state_root(), B256::repeat_byte(0xcd));
    assert_eq!(child.executed_base_fee(), Price(1_000));
}

#[test]
fn last_executed_pointer_updated_on_mark() {
    use arc_swap::ArcSwapOption;
    let (g, child) = genesis_and_child();
    let last_executed: ArcSwapOption<Block> = ArcSwapOption::from(Some(Arc::clone(&g)));
    child
        .mark_executed(artefacts(), Some(&last_executed))
        .expect("mark_executed");
    let le = last_executed.load_full().expect("some");
    assert_eq!(le.height(), 1, "last_executed advanced to child");
}
