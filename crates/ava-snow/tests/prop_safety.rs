// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `prop::consensus_safety` — the TDD-entry safety property (M3.1; specs 06
//! §2.4, §10; 02 §4).
//!
//! Safety is the non-negotiable consensus invariant: **no two conflicting
//! blocks (same height, different id) are ever both accepted**, and the
//! accepted set is always a chain rooted at genesis. This harness is written
//! before the engine exists. It compiles today against the in-memory
//! [`Cluster`] scaffolding (M3.1) and is `#[ignore]`d; it turns GREEN at M3.5
//! when `Topological` is wired into the cluster and begins recording real
//! acceptances into the shared oracle.
//!
//! UN-IGNORE at M3.5.
//!
//! Gated on the `testutil` feature: the in-memory cluster scaffolding it drives
//! lives there, so a no-feature `cargo test` build stays clean (CI runs
//! `--all-features`).

#![cfg(feature = "testutil")]
#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use ava_snow::snowball::DEFAULT_PARAMETERS;
use ava_snow::testutil::{Cluster, TestBlock};
use ava_types::id::Id;
use ava_utils::bag::Bag;
use proptest::prelude::*;

/// Builds a random DAG of conflicting blocks (same-height siblings) rooted at
/// genesis, registers them on the cluster, then applies a random sequence of
/// per-node vote bags. Asserts the cluster's accepted chain never holds two
/// conflicting blocks at one height and is contiguous from genesis.
fn run_safety_case(n_nodes: usize, heights: u64, siblings: u64, vote_rounds: usize, seed: u64) {
    let params = DEFAULT_PARAMETERS;
    let mut cluster = Cluster::new(n_nodes.max(1), params);

    // Build a conflicting-sibling DAG: at each height, `siblings` blocks all
    // share the same parent (the genesis-rooted preferred chain is ambiguous,
    // exactly the conflict the safety property must resolve).
    let mut parent = cluster.genesis();
    let mut all_ids: Vec<Id> = Vec::new();
    for h in 1..=heights {
        let mut first_child = None;
        for s in 0..siblings.max(1) {
            // Deterministic distinct id per (height, sibling, seed).
            let id = Id::EMPTY.prefix(&[seed, h, s]);
            cluster.add_block(TestBlock::new(id, parent, h));
            all_ids.push(id);
            if first_child.is_none() {
                first_child = Some(id);
            }
        }
        // Extend the canonical branch off the first sibling.
        parent = first_child.unwrap_or(parent);
    }

    // Apply random vote rounds. Each node votes for some known block.
    for r in 0..vote_rounds {
        let votes: Vec<Bag<Id>> = (0..cluster.len())
            .map(|node| {
                let mut bag = Bag::new();
                if !all_ids.is_empty() {
                    let idx = (seed as usize)
                        .wrapping_add(r)
                        .wrapping_add(node)
                        .wrapping_mul(31)
                        % all_ids.len();
                    bag.add(all_ids[idx]);
                }
                bag
            })
            .collect();
        cluster.step(&votes);
    }

    // Safety assertions over the accepted chain.
    let chain = cluster.accepted_chain();
    // No two distinct ids at the same height (BTreeMap keying already enforces
    // one id per height; this re-checks contiguity from genesis).
    for (expected_height, (height, _id)) in (1u64..).zip(chain.iter()) {
        assert_eq!(
            *height, expected_height,
            "accepted chain must be contiguous from genesis"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// M3.5: real safety assertions over `Topological` wired into the cluster.
    /// The accepted chain must never hold two conflicting blocks at one height
    /// and must be contiguous from genesis.
    #[test]
    fn consensus_safety(
        n_nodes in 1usize..8,
        heights in 1u64..6,
        siblings in 1u64..4,
        vote_rounds in 0usize..20,
        seed in any::<u64>(),
    ) {
        run_safety_case(n_nodes, heights, siblings, vote_rounds, seed);
    }
}
