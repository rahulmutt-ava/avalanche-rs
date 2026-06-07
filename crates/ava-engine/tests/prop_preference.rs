// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `prop::preference_monotone` (specs 06 §2.4 / §10): across any vote sequence,
//! once the engine reports a preference at a given height it never regresses to
//! an already-rejected sibling without a higher-height reason. Equivalently:
//! - the accepted block at a height never changes once set (safety / linearity);
//! - a node's preference is never a block consensus has rejected.
//!
//! Two conflicting siblings (A, B) are issued at height 1; the honest cluster
//! votes its own preference each round. The winner finalizes; the loser is
//! rejected. We sample the preference at height 1 every round and assert the
//! monotonicity invariant on every node.

mod support;

use proptest::prelude::*;

use ava_types::id::Id;

use support::{Cluster, block_id, default_params, encode_block};

/// Runs one preference-monotonicity trial with `n` nodes, beta=`beta`, issuing
/// siblings A then B in the order selected by `b_first`. Returns
/// `Ok(())` if the invariant held throughout, else `Err(reason)`.
async fn run_preference_trial(n: usize, beta: u32, b_first: bool) -> Result<(), String> {
    let mut params = default_params();
    params.k = n as u32;
    params.alpha_preference = (n as u32 / 2) + 1;
    params.alpha_confidence = (n as u32 / 2) + 1;
    params.beta = beta;
    params.concurrent_repolls = 1;
    params.optimal_processing = 2;

    let mut cluster = Cluster::new(n, params).await;
    let genesis = cluster.genesis;

    let a = encode_block(genesis, 1, b"sibling-a");
    let a_id = block_id(&a);
    let b = encode_block(genesis, 1, b"sibling-b");
    let b_id = block_id(&b);

    // Issue both siblings to every node (order under test).
    if b_first {
        cluster.issue_block_all(&b).await;
        cluster.issue_block_all(&a).await;
    } else {
        cluster.issue_block_all(&a).await;
        cluster.issue_block_all(&b).await;
    }

    // Track, per node, the last non-genesis preference seen at height 1.
    let mut settled: Vec<Option<Id>> = vec![None; n];
    let mut accepted_at_1: Vec<Option<Id>> = vec![None; n];

    let max_rounds = (beta as usize + 4) * 6;
    for _round in 0..max_rounds {
        cluster.run_round().await;

        for (i, node) in cluster.nodes.iter().enumerate() {
            // Invariant 1: a node's preference is never a rejected sibling.
            // After a sibling is accepted, the other is rejected (not processing,
            // not the accepted id). The preference must be the accepted one.
            let (la_id, la_h) = node.engine.consensus_last_accepted();
            if la_h >= 1 {
                // Height 1 decided: the accepted block must be A or B and stable.
                if la_id != a_id && la_id != b_id {
                    return Err(format!("node {i} accepted an unknown block at h1"));
                }
                match accepted_at_1[i] {
                    Some(prev) if prev != la_id => {
                        return Err(format!(
                            "node {i} accepted block at h1 changed {prev} -> {la_id}"
                        ));
                    }
                    _ => accepted_at_1[i] = Some(la_id),
                }
                // The loser sibling must never be the preference once decided.
                let loser = if la_id == a_id { b_id } else { a_id };
                if node.engine.preference() == loser {
                    return Err(format!("node {i} prefers rejected sibling {loser}"));
                }
            }

            // Invariant 2: preference_at_height(1) must not flip between the two
            // siblings once it has settled (no regression to an abandoned sib).
            if let Some(pref_h1) = node.engine.preference_at_height(1)
                && (pref_h1 == a_id || pref_h1 == b_id)
            {
                match settled[i] {
                    Some(prev) if prev != pref_h1 => {
                        // A flip is only acceptable if the new pref is the one
                        // that ultimately gets accepted (consensus converging),
                        // never a flip *to* an already-rejected block.
                        let decided = accepted_at_1[i];
                        if decided == Some(prev) {
                            return Err(format!(
                                "node {i} preference at h1 regressed from {prev} to {pref_h1}"
                            ));
                        }
                        settled[i] = Some(pref_h1);
                    }
                    _ => settled[i] = Some(pref_h1),
                }
            }
        }

        if cluster.nodes.iter().all(|nd| nd.engine.consensus_last_accepted().1 >= 1) {
            break;
        }
    }

    // Every node must have decided height 1 on the *same* block (agreement).
    let decided: Vec<Id> = cluster
        .nodes
        .iter()
        .map(|nd| nd.engine.consensus_last_accepted().0)
        .collect();
    let first = decided[0];
    if !decided.iter().all(|&d| d == first) {
        return Err("nodes disagreed on the accepted block at h1".to_string());
    }
    if first != a_id && first != b_id {
        return Err("height 1 not decided on a sibling".to_string());
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 24,
        ..ProptestConfig::default()
    })]

    /// Once a preference is reported at a height, it never regresses to an
    /// already-rejected sibling; the accepted block at a height is stable.
    #[test]
    fn preference_monotone(n in 3usize..=7, beta in 1u32..=3, b_first in any::<bool>()) {
        let res = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(run_preference_trial(n, beta, b_first));
        prop_assert!(res.is_ok(), "{}", res.err().unwrap_or_default());
    }
}

/// A concrete smoke test: with two siblings and a unanimous cluster, exactly one
/// is accepted on every node and the other is never preferred afterward.
#[tokio::test]
async fn preference_smoke_no_regression() {
    let res = run_preference_trial(5, 2, false).await;
    assert!(res.is_ok(), "{}", res.err().unwrap_or_default());
}
