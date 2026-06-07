// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M3.27 `differential::testvm_finalizes` (specs 07 §8.2, 06 §10) — the
//! simulated-cluster exit gate.
//!
//! Boots an in-memory N-node cluster where **each node runs the full
//! `create_snowman_chain` VM pipeline** (`inner → tracedvm → proposervm →
//! metervm → tracedvm → change-notifier`) around the in-memory test VM, with a
//! looped-back `Sender`/router and a `MockClock` (virtual time). It issues a
//! block, drives the poll waves to finalization, and asserts that **every node
//! reaches the same stable last-accepted height/ID** — there is no fork.
//!
//! This re-asserts `prop::consensus_safety`/`liveness` end-to-end through the
//! production chain-creation pipeline, reusing the `ava-engine` cluster
//! mechanics (here a local copy in `support`, driving the wrapped VM).

// Test-fixture arithmetic on known-small bounds is clearer than checked math.
#![allow(clippy::arithmetic_side_effects)]

// Crate deps linked by the lib/support but not named directly by this target.
use assert_matches as _;
use ava_codec as _;
use ava_crypto as _;
use ava_version as _;
use rcgen as _;
use ring as _;
use rustls_pemfile as _;
use serde_json as _;
use thiserror as _;
use tokio as _;

use proptest::prelude::*;

mod support;
use support::{Cluster, block_id_of, encode_block};

use ava_snow::snowball::DEFAULT_PARAMETERS;

/// Runs one finalization trial: `n` pipeline nodes, a single child of genesis,
/// `beta` consecutive successful polls to finalize. Returns the agreed
/// last-accepted `(id, height)` once every node finalizes the block, or `None`
/// if it did not finalize within the bound.
async fn run_trial(n: usize, beta: u32) -> Option<(ava_types::id::Id, u64)> {
    let mut params = DEFAULT_PARAMETERS;
    params.k = n as u32;
    params.alpha_preference = (n as u32 / 2) + 1;
    params.alpha_confidence = (n as u32 / 2) + 1;
    params.beta = beta;
    params.concurrent_repolls = 1;
    params.optimal_processing = 1;

    let mut cluster = Cluster::new(n, params).await;

    let genesis = cluster.genesis;
    let child = encode_block(genesis, 1, b"branch-a");
    let child_id = block_id_of(&child);
    cluster.issue_block_all(&child).await;

    let max_rounds = (beta as usize + 4) * 4;
    for _round in 1..=max_rounds {
        cluster.run_round().await;
        if cluster.all_accepted(child_id) {
            // Every node must agree on the same last-accepted (no fork).
            let agreed = cluster.agreed_last_accepted()?;
            assert_eq!(agreed.0, child_id, "agreed last-accepted is the issued block");
            return Some(agreed);
        }
        // Safety invariant every round: nodes never disagree on last-accepted.
        assert!(
            cluster.agreed_last_accepted().is_some(),
            "no fork: every node agrees on last-accepted each round"
        );
    }
    None
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

    /// Every node in the pipeline-driven cluster finalizes the same block at the
    /// same height within the poll bound — no fork.
    #[test]
    fn testvm_finalizes(n in 3usize..=5, beta in 1u32..=3) {
        let agreed = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(run_trial(n, beta));
        prop_assert!(agreed.is_some(), "cluster must finalize (n={n}, beta={beta})");
        let (_, height) = agreed.expect("finalized");
        prop_assert_eq!(height, 1, "the finalized block is the height-1 child of genesis");
    }
}

/// A concrete smoke test guarding against a vacuously-passing property: a
/// 5-node pipeline cluster finalizes a unanimous branch and agrees on it.
#[tokio::test]
async fn testvm_finalizes_smoke() {
    let agreed = run_trial(5, 2).await;
    assert!(agreed.is_some(), "5-node pipeline cluster finalizes");
    assert_eq!(agreed.expect("finalized").1, 1, "finalized at height 1");
}
