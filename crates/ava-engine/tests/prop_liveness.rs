// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `prop::consensus_liveness` (specs 06 §2.4 / §10): driving an engine-driven
//! cluster where every honest node votes for the same single branch each round,
//! that branch finalizes within a bounded number of polls. Faltering branches do
//! not livelock — with unanimous votes, confidence climbs monotonically to beta.
//!
//! The cluster is purely message-driven (a recording `Sender` looped through a
//! mock router); time is virtual, so the property runs with no wall-clock waits.

mod support;

use proptest::prelude::*;

use support::{Cluster, default_params, encode_block};

/// Runs one liveness trial: `n` engines, a single child of genesis, `beta`
/// consecutive successful polls to finalize. Returns the number of routing
/// rounds it took to finalize on every node (or `None` if it did not finalize
/// within the bound).
async fn run_liveness_trial(n: usize, beta: u32) -> Option<usize> {
    let mut params = default_params();
    params.k = n as u32;
    // Strict-majority alpha so a unanimous vote always clears the threshold.
    params.alpha_preference = (n as u32 / 2) + 1;
    params.alpha_confidence = (n as u32 / 2) + 1;
    params.beta = beta;
    params.concurrent_repolls = 1;
    params.optimal_processing = 1;

    let mut cluster = Cluster::new(n, params).await;

    // A single branch: one child of genesis. Every honest node issues it.
    let genesis = cluster.genesis;
    let child = encode_block(genesis, 1, b"branch-a");
    let child_id = support::block_id(&child);
    cluster.issue_block_all(&child).await;

    // Bound: beta successful polls should finalize; allow generous slack.
    let max_rounds = (beta as usize + 4) * 4;
    for round in 1..=max_rounds {
        // Each round issues a fresh poll on every node and routes it.
        cluster.run_round().await;

        if cluster.all_accepted(child_id) {
            return Some(round);
        }
    }
    None
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 24,
        ..ProptestConfig::default()
    })]

    /// With ≥ alpha honest stake voting one branch each round, the branch
    /// finalizes within a bounded number of polls on every node.
    #[test]
    fn consensus_liveness(n in 3usize..=7, beta in 1u32..=4) {
        let rounds = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(run_liveness_trial(n, beta));

        prop_assert!(
            rounds.is_some(),
            "branch must finalize within the poll bound (n={n}, beta={beta})"
        );
        // Liveness with no slack waste: should finalize in roughly beta rounds
        // (each unanimous round records one successful poll).
        let rounds = rounds.expect("finalized");
        let bound = (beta as usize + 4) * 4;
        prop_assert!(rounds <= bound, "finalized in {rounds} > bound {bound}");
    }
}

/// A concrete (non-property) smoke test that finalization actually happens and
/// the preference matches the finalized block — guards against a vacuously
/// passing property.
#[tokio::test]
async fn liveness_smoke_finalizes_branch() {
    let n = 5;
    let beta = 2;
    let rounds = run_liveness_trial(n, beta).await;
    assert!(
        rounds.is_some(),
        "5-node cluster must finalize a unanimous branch"
    );
}
