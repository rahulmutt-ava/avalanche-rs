// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Tests for the eth-RPC → SAE frontier label mapping (specs/11 §1.1).
//!
//! Exercises `resolve_rpc_number` on all six label kinds: `pending`, `latest`,
//! `safe`, `finalized`, `earliest`, and `Number(n)` — including the two
//! sentinel error cases `ErrFutureBlockNotResolved` and `ErrNonCanonicalBlock`.
//!
//! Go reference: `vms/saevm/blocks/access.go::ResolveRPCNumber`.

// Small numeric arithmetic in fixture builders; counters are tiny constants.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::sync::Arc;

use ava_evm_reth::{B256, Header, RethBlock, SealedBlock};
use ava_saevm_blocks::{Block, ExecutionArtefacts};
use ava_saevm_core::Frontier;
use ava_saevm_core::rpc::{RpcBlockLabel, RpcError, resolve_rpc_number};
use ava_saevm_proxytime::Time;
use ava_saevm_types::ExecutionResults;
use ava_vm::components::gas::Price;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn eth_block(number: u64, timestamp: u64, parent_hash: B256) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        timestamp,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

fn genesis() -> Arc<Block> {
    let g = Arc::new(Block::new(eth_block(0, 0, B256::ZERO), None, None).expect("genesis"));
    g.mark_synchronous((
        ava_vm::components::gas::Gas(0),
        ava_saevm_gastime::GasPriceConfig::default(),
    ))
    .expect("mark synchronous");
    g
}

fn results_at(at_unix: u64) -> ExecutionResults {
    ExecutionResults {
        gas_time: Time::<u64>::new(at_unix, 0, 1),
        base_fee: Price(1),
        receipt_root: B256::ZERO,
        post_state_root: B256::repeat_byte(0x33),
    }
}

fn mark_executed_at(block: &Arc<Block>, at_unix: u64) {
    let results = results_at(at_unix);
    let artefacts = ExecutionArtefacts {
        interim_execution_time: results.gas_time.clone(),
        results,
    };
    block.mark_executed(artefacts, None).expect("mark executed");
}

/// A three-block frontier: genesis (S=0), block-1 (E=1), block-2 (A=2).
/// Returns `(frontier, [genesis, block1, block2])`.
fn three_block_frontier() -> (Frontier, Vec<Arc<Block>>) {
    let g = genesis();
    let b1 = Arc::new(
        Block::new(
            eth_block(1, 1, g.hash()),
            Some(Arc::clone(&g)),
            Some(Arc::clone(&g)),
        )
        .expect("block1"),
    );
    let b2 = Arc::new(
        Block::new(
            eth_block(2, 2, b1.hash()),
            Some(Arc::clone(&b1)),
            Some(Arc::clone(&g)),
        )
        .expect("block2"),
    );

    // Mark genesis+b1 executed so E can advance.
    mark_executed_at(&b1, 1);

    let frontier = Frontier::new(Arc::clone(&g));
    frontier.advance_accepted(&b1);
    frontier.advance_accepted(&b2);
    frontier.advance_executed(&b1);

    // Height index: 0 → g, 1 → b1, 2 → b2.
    let canonical: Vec<Arc<Block>> = vec![Arc::clone(&g), Arc::clone(&b1), Arc::clone(&b2)];
    (frontier, canonical)
}

// ---------------------------------------------------------------------------
// Test 1 — resolve_rpc_number table: pending/latest/safe/finalized/earliest
// ---------------------------------------------------------------------------

/// `pending → LastAccepted (A)`, `latest → LastExecuted (E)`,
/// `safe`/`finalized → LastSettled (S)`, `earliest → 0`.
#[test]
fn resolve_rpc_number_label_table() {
    let (frontier, canonical) = three_block_frontier();

    // Build the canonical height index (height → block).
    let height_index: Vec<Arc<Block>> = canonical.clone();
    let canonical_fn = |h: u64| -> Option<Arc<Block>> { height_index.get(h as usize).cloned() };

    // pending → A = height 2.
    assert_eq!(
        resolve_rpc_number(RpcBlockLabel::Pending, &frontier, canonical_fn).expect("pending"),
        2,
        "pending must resolve to LastAccepted (A)",
    );

    // latest → E = height 1.
    assert_eq!(
        resolve_rpc_number(RpcBlockLabel::Latest, &frontier, canonical_fn).expect("latest"),
        1,
        "latest must resolve to LastExecuted (E)",
    );

    // safe → S = height 0 (genesis).
    assert_eq!(
        resolve_rpc_number(RpcBlockLabel::Safe, &frontier, canonical_fn).expect("safe"),
        0,
        "safe must resolve to LastSettled (S)",
    );

    // finalized → S = height 0 (genesis).
    assert_eq!(
        resolve_rpc_number(RpcBlockLabel::Finalized, &frontier, canonical_fn).expect("finalized"),
        0,
        "finalized must resolve to LastSettled (S)",
    );

    // earliest → 0.
    assert_eq!(
        resolve_rpc_number(RpcBlockLabel::Earliest, &frontier, canonical_fn).expect("earliest"),
        0,
        "earliest must resolve to height 0",
    );

    // Number at A (absolute, canonical).
    assert_eq!(
        resolve_rpc_number(RpcBlockLabel::Number(2), &frontier, canonical_fn).expect("number(2)"),
        2,
        "Number(A.height) must resolve to A.height when canonical",
    );
}

// ---------------------------------------------------------------------------
// Test 2 — future block not resolved (n > A.height)
// ---------------------------------------------------------------------------

/// `Number(n)` where `n > A.height` must return `ErrFutureBlockNotResolved`.
#[test]
fn future_block_not_resolved_errors() {
    let (frontier, canonical) = three_block_frontier();
    let canonical_fn = |h: u64| -> Option<Arc<Block>> { canonical.get(h as usize).cloned() };

    // A is at height 2; height 3 is beyond the accepted frontier.
    let result = resolve_rpc_number(RpcBlockLabel::Number(3), &frontier, canonical_fn);
    assert!(
        matches!(result, Err(RpcError::FutureBlockNotResolved)),
        "Number(A+1) must error with FutureBlockNotResolved; got {result:?}",
    );

    // Large height also returns the same error.
    let result = resolve_rpc_number(RpcBlockLabel::Number(u64::MAX), &frontier, canonical_fn);
    assert!(
        matches!(result, Err(RpcError::FutureBlockNotResolved)),
        "Number(u64::MAX) must error with FutureBlockNotResolved; got {result:?}",
    );
}

// ---------------------------------------------------------------------------
// Test 3 — non-canonical block (n <= A.height but not in the canonical index)
// ---------------------------------------------------------------------------

/// `Number(n)` where `n ≤ A.height` but the height is absent from the canonical
/// index must return `ErrNonCanonicalBlock`.
#[test]
fn non_canonical_block_errors() {
    let (frontier, canonical) = three_block_frontier();

    // Height 1 exists canonically; return None for it to simulate a gap.
    let canonical_fn = |h: u64| -> Option<Arc<Block>> {
        if h == 1 {
            None // simulate non-canonical / pruned height
        } else {
            canonical.get(h as usize).cloned()
        }
    };

    // Height 1 is <= A.height=2 but absent from the canonical index.
    let result = resolve_rpc_number(RpcBlockLabel::Number(1), &frontier, canonical_fn);
    assert!(
        matches!(result, Err(RpcError::NonCanonicalBlock)),
        "Number(1) absent from canonical index must error with NonCanonicalBlock; got {result:?}",
    );
}
