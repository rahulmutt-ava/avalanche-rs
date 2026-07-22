// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Snowman engine integration flows (specs 06 §4.2): the issue path requests a
//! missing parent, the vote path records a completed poll then sets VM
//! preference, and early-termination completes a poll before every sampled
//! validator responds.

mod support;

use tokio_util::sync::CancellationToken;

use support::{Sent, block_id, build_engine, default_params, encode_block, init_vm, validators};

/// `engine_requests_missing_block` — a `Put` for a block whose parent is unknown
/// triggers exactly one `Sender::send_get` to the providing node.
#[tokio::test]
async fn engine_requests_missing_block() {
    let token = CancellationToken::new();
    let (mgr, nodes) = validators(4);
    let (vm, genesis) = init_vm(&token).await;
    let sender = support::RecordingSender::new();
    let mut engine = build_engine(
        default_params(),
        vm,
        sender.clone(),
        mgr,
        genesis,
        token.clone(),
    );

    // Construct a height-2 block whose parent (a height-1 block) is unknown to
    // the engine/VM. The parent id is random (never registered).
    let unknown_parent = ava_types::id::Id::from([0xAB; 32]);
    let child_bytes = encode_block(unknown_parent, 2, b"orphan");
    let provider = nodes[0];

    engine
        .put(provider, 42, &child_bytes)
        .await
        .expect("put orphan");

    let sent = sender.snapshot();
    let gets: Vec<&Sent> = sent
        .iter()
        .filter(|s| matches!(s, Sent::Get { .. }))
        .collect();
    assert_eq!(gets.len(), 1, "expected exactly one Get, got {sent:?}");
    match gets[0] {
        Sent::Get { node, id, .. } => {
            assert_eq!(*node, provider, "Get must go to the providing node");
            assert_eq!(*id, unknown_parent, "Get must request the missing parent");
        }
        _ => unreachable!(),
    }
}

/// `engine_records_poll_on_chits` — a completed poll's votes are fed to
/// `record_poll`, then `set_preference(preference())` is called on the VM. We
/// observe this through the engine accepting the queried block once enough chits
/// arrive (preference advances off genesis).
#[tokio::test]
async fn engine_records_poll_on_chits() {
    let token = CancellationToken::new();
    // k=4, alpha=3, beta=1 so a single unanimous poll finalizes.
    let mut params = default_params();
    params.k = 4;
    params.alpha_preference = 3;
    params.alpha_confidence = 3;
    params.beta = 1;
    params.concurrent_repolls = 1;

    let (mgr, nodes) = validators(4);
    let (vm, genesis) = init_vm(&token).await;
    let sender = support::RecordingSender::new();
    let mut engine = build_engine(params, vm, sender.clone(), mgr, genesis, token.clone());

    // Build a child of genesis (height 1) and issue it via a Put from a peer.
    let child_bytes = encode_block(genesis, 1, b"c1");
    let child_id = block_id(&child_bytes);
    engine.put(nodes[0], 1, &child_bytes).await.expect("put c1");

    assert_eq!(
        engine.num_processing(),
        1,
        "the child must be issued into consensus"
    );
    assert!(engine.num_polls() > 0, "issuing a preferred block repolls");
    assert_eq!(engine.preference(), child_id, "child is the new preference");

    // The query carries a request id; reply with unanimous chits for the child.
    let req = engine.request_id();
    for &node in &nodes {
        engine
            .chits(node, req, child_id, child_id, child_id, 1)
            .await
            .expect("chits");
    }

    // With beta=1 the unanimous poll finalizes, accepting the child: the
    // consensus last-accepted advances and nothing remains processing.
    let (last_accepted, height) = engine.consensus_last_accepted();
    assert_eq!(last_accepted, child_id, "child accepted via record_poll");
    assert_eq!(height, 1);
    assert_eq!(engine.num_processing(), 0, "no processing blocks remain");
}

/// `engine_poll_bag_counts_sample_multiplicity` — the poll bag must weight each
/// node by how many times it appears in the k-sample (Go `bag.Of(vdrIDs...)`),
/// NOT by the validator's full stake weight. With 2 validators of weight 2 and
/// `k == total weight == 4`, the sampler draws a full permutation of the 4
/// weight units, so each validator appears EXACTLY twice. A single chit from
/// ONE validator therefore contributes multiplicity 2 — below alpha_confidence
/// 3 — so the block must NOT finalize until the second validator also replies.
///
/// Red-first regression: the pre-fix `send_query` built the poll bag from
/// `get_weight` (full stake) per sampled occurrence, inflating one chit to
/// `weight * multiplicity = 4 >= alpha`, which wrongly accepted the block off a
/// single reply.
#[tokio::test]
async fn engine_poll_bag_counts_sample_multiplicity() {
    let token = CancellationToken::new();
    let mut params = default_params();
    params.k = 4;
    params.alpha_preference = 3;
    params.alpha_confidence = 3;
    params.beta = 1;
    params.concurrent_repolls = 1;

    // 2 validators, weight 2 each: total weight 4 == k, so sample(4) draws a
    // full permutation of the weight units — each validator appears twice.
    let mgr = std::sync::Arc::new(ava_validators::DefaultManager::new());
    let node_a = ava_types::node_id::NodeId::from([1u8; 20]);
    let node_b = ava_types::node_id::NodeId::from([2u8; 20]);
    ava_validators::ValidatorManager::add_staker(
        mgr.as_ref(),
        ava_types::id::Id::EMPTY,
        node_a,
        None,
        ava_types::id::Id::EMPTY,
        2,
    )
    .expect("add validator a");
    ava_validators::ValidatorManager::add_staker(
        mgr.as_ref(),
        ava_types::id::Id::EMPTY,
        node_b,
        None,
        ava_types::id::Id::EMPTY,
        2,
    )
    .expect("add validator b");

    let (vm, genesis) = init_vm(&token).await;
    let sender = support::RecordingSender::new();
    let mut engine = build_engine(params, vm, sender.clone(), mgr, genesis, token.clone());

    let child_bytes = encode_block(genesis, 1, b"c1");
    let child_id = block_id(&child_bytes);
    engine.put(node_a, 1, &child_bytes).await.expect("put c1");
    assert_eq!(engine.num_processing(), 1, "child issued into consensus");

    let req = engine.request_id();

    // ONE chit from validator A: multiplicity 2 < alpha_confidence 3, so the
    // block must remain processing (NOT accepted off a single reply).
    engine
        .chits(node_a, req, child_id, child_id, child_id, 1)
        .await
        .expect("chits a");
    assert_eq!(
        engine.consensus_last_accepted().0,
        genesis,
        "one chit (multiplicity 2 < alpha 3) must NOT finalize the block"
    );
    assert_eq!(
        engine.num_processing(),
        1,
        "block still processing after a single chit"
    );

    // Second chit from B: total votes 2 + 2 = 4 >= alpha, beta 1 → accepted.
    engine
        .chits(node_b, req, child_id, child_id, child_id, 1)
        .await
        .expect("chits b");
    assert_eq!(
        engine.consensus_last_accepted().0,
        child_id,
        "both chits (total multiplicity 4 >= alpha) finalize the block"
    );
    assert_eq!(engine.num_processing(), 0, "no processing blocks remain");
}

/// `early_term_completes_poll` — a poll completes once outstanding responses can
/// no longer change the alpha outcome (before every validator responds).
#[tokio::test]
async fn early_term_completes_poll() {
    let token = CancellationToken::new();
    let mut params = default_params();
    params.k = 4;
    params.alpha_preference = 3;
    params.alpha_confidence = 3;
    params.beta = 2; // so the first poll does NOT finalize (stays processing)
    params.concurrent_repolls = 1;

    let (mgr, nodes) = validators(4);
    let (vm, genesis) = init_vm(&token).await;
    let sender = support::RecordingSender::new();
    let mut engine = build_engine(params, vm, sender.clone(), mgr, genesis, token.clone());

    let child_bytes = encode_block(genesis, 1, b"c1");
    let child_id = block_id(&child_bytes);
    engine.put(nodes[0], 1, &child_bytes).await.expect("put c1");

    let req = engine.request_id();
    let polls_before = engine.num_polls();

    // Three unanimous chits reach alpha_confidence(3); the 4th validator never
    // responds. The poll must still complete (early-term case 4), recording a
    // poll and repolling — observable as the original poll no longer pending and
    // the preference still on the child.
    for &node in nodes.iter().take(3) {
        engine
            .chits(node, req, child_id, child_id, child_id, 1)
            .await
            .expect("chits");
    }

    assert!(
        !engine.poll_pending(req),
        "early-term must complete the poll after 3/4 unanimous chits"
    );
    assert!(polls_before > 0);
    assert_eq!(engine.preference(), child_id);
    // beta=2 not yet reached, so the block is still processing (not accepted).
    assert_eq!(engine.num_processing(), 1);
}
