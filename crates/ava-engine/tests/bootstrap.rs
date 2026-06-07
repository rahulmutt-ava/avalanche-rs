// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Snowman bootstrapper integration (specs 06 §4.3): the bootstrapper discovers
//! the network frontier, fetches the ancestry into the interval tree, executes
//! the range in height order (firing the consensus acceptor before each block's
//! VM accept and toggling `ConsensusContext.executing`), and hands off to normal
//! operation. A fired halt token aborts the execute pass promptly.

mod support;

use std::collections::BTreeMap;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use ava_engine::snowman::bootstrap::{Bootstrapper, Config, Phase};
use ava_snow::acceptor::Acceptor;
use ava_snow::{ConsensusContext, EngineState};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::block::ChainVm;
use ava_vm::testutil::{TestVm, init_test_vm, test_chain_context};

use support::{RecordingSender, Sent, block_id, encode_block};

/// Records the order in which the consensus acceptor fires, and whether
/// `executing` was set when it fired.
#[derive(Default)]
struct RecordingAcceptor {
    accepted: StdMutex<Vec<(Id, bool)>>,
}

#[async_trait::async_trait]
impl Acceptor for RecordingAcceptor {
    async fn accept(
        &self,
        ctx: &ConsensusContext,
        container_id: Id,
        _bytes: &[u8],
    ) -> ava_snow::Result<()> {
        let executing = ctx.executing.load(Ordering::SeqCst);
        self.accepted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((container_id, executing));
        Ok(())
    }
}

/// Builds a `ConsensusContext` wired to the supplied block acceptor.
fn consensus_ctx(acceptor: Arc<RecordingAcceptor>) -> Arc<ConsensusContext> {
    Arc::new(ConsensusContext::new(
        test_chain_context(),
        "C".to_string(),
        acceptor,
        Arc::new(ava_snow::acceptor::NoOpAcceptor),
    ))
}

/// Builds a 3-block chain (heights 1..=3) rooted at `genesis`, returning the
/// encoded bytes (newest first, as a peer would reply to `GetAncestors`) and the
/// tip id.
fn build_chain(genesis: Id) -> (Vec<Vec<u8>>, Vec<Id>) {
    let b1 = encode_block(genesis, 1, b"b1");
    let id1 = block_id(&b1);
    let b2 = encode_block(id1, 2, b"b2");
    let id2 = block_id(&b2);
    let b3 = encode_block(id2, 3, b"b3");
    let id3 = block_id(&b3);
    // Ancestors reply: requested block first (tip), then ancestors oldest-last.
    (vec![b3.clone(), b2.clone(), b1.clone()], vec![id1, id2, id3])
}

/// `bootstrap_fetches_and_executes_range` — a beacon set serving
/// `AcceptedFrontier`/`Accepted`/`Ancestors` drives the bootstrapper to fetch the
/// ancestry, replay/accept in height order, and transition Bootstrapping →
/// NormalOp.
#[tokio::test]
async fn bootstrap_fetches_and_executes_range() {
    let token = CancellationToken::new();
    let vm: TestVm = init_test_vm(&token).await.expect("vm");
    let genesis = vm.last_accepted(&token).await.expect("genesis");

    let (chain_bytes, ids) = build_chain(genesis);
    let tip = *ids.last().expect("tip");

    let acceptor = Arc::new(RecordingAcceptor::default());
    let ctx = consensus_ctx(acceptor.clone());
    let sender = RecordingSender::new();

    // Two equally-weighted beacons.
    let beacon_a = NodeId::from([10u8; 20]);
    let beacon_b = NodeId::from([11u8; 20]);
    let mut beacons = BTreeMap::new();
    beacons.insert(beacon_a, 1u64);
    beacons.insert(beacon_b, 1u64);

    let cfg = Config {
        subnet_id: Id::EMPTY,
        ctx: ctx.clone(),
        vm: Arc::new(Mutex::new(vm)),
        sender: sender.clone(),
        beacons,
        token: token.clone(),
    };
    let mut boot = Bootstrapper::new(cfg);

    // Start: enters Bootstrapping + sends GetAcceptedFrontier.
    boot.start(0).await.expect("start");
    assert_eq!(boot.phase(), Phase::DiscoveringFrontier);
    assert_eq!(**ctx.state.load(), EngineState::Bootstrapping);
    let sent = sender.drain();
    assert!(
        sent.iter().any(|s| matches!(s, Sent::GetAcceptedFrontier { .. })),
        "expected GetAcceptedFrontier, got {sent:?}"
    );

    // Both beacons report the tip as their frontier.
    boot.accepted_frontier(beacon_a, 1, tip).await.expect("af a");
    boot.accepted_frontier(beacon_b, 1, tip).await.expect("af b");
    assert_eq!(boot.phase(), Phase::AgreeingFrontier);
    let sent = sender.drain();
    assert!(
        sent.iter().any(|s| matches!(s, Sent::GetAccepted { .. })),
        "expected GetAccepted, got {sent:?}"
    );

    // Both beacons accept the tip -> weight threshold met -> fetch ancestry.
    boot.accepted(beacon_a, 2, &[tip]).await.expect("acc a");
    boot.accepted(beacon_b, 2, &[tip]).await.expect("acc b");
    let sent = sender.drain();
    let ga_req = sent.iter().find_map(|s| match s {
        Sent::GetAncestors { node, req, id } if *id == tip => Some((*node, *req)),
        _ => None,
    });
    let (node, req) = ga_req.expect("expected GetAncestors for the tip");

    // Serve the full ancestry: connects back to genesis, so no further fetch.
    boot.ancestors(node, req, &chain_bytes).await.expect("ancestors");

    // The range executed in height order and the node handed off.
    assert!(boot.is_finished(), "bootstrapper must hand off");
    assert_eq!(boot.phase(), Phase::Finished);
    assert_eq!(**ctx.state.load(), EngineState::NormalOp);

    let accepted = acceptor.accepted.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let accepted_ids: Vec<Id> = accepted.iter().map(|(id, _)| *id).collect();
    assert_eq!(accepted_ids, ids, "blocks accepted in ascending height order");
    assert!(
        accepted.iter().all(|(_, executing)| *executing),
        "executing flag must be set during replay"
    );
    // executing is cleared after the pass.
    assert!(!ctx.executing.load(Ordering::SeqCst));
}

/// `halt_aborts_bootstrap` — cancelling the token aborts the execute pass
/// promptly (the bootstrapper returns `Halted` and does not hand off).
#[tokio::test]
async fn halt_aborts_bootstrap() {
    let token = CancellationToken::new();
    let vm: TestVm = init_test_vm(&token).await.expect("vm");
    let genesis = vm.last_accepted(&token).await.expect("genesis");
    let (chain_bytes, ids) = build_chain(genesis);
    let tip = *ids.last().expect("tip");

    let acceptor = Arc::new(RecordingAcceptor::default());
    let ctx = consensus_ctx(acceptor.clone());
    let sender = RecordingSender::new();

    let beacon = NodeId::from([10u8; 20]);
    let mut beacons = BTreeMap::new();
    beacons.insert(beacon, 1u64);

    let cfg = Config {
        subnet_id: Id::EMPTY,
        ctx: ctx.clone(),
        vm: Arc::new(Mutex::new(vm)),
        sender: sender.clone(),
        beacons,
        token: token.clone(),
    };
    let mut boot = Bootstrapper::new(cfg);

    boot.start(0).await.expect("start");
    boot.accepted_frontier(beacon, 1, tip).await.expect("af");
    boot.accepted(beacon, 2, &[tip]).await.expect("acc");
    let sent = sender.drain();
    let (node, req) = sent
        .iter()
        .find_map(|s| match s {
            Sent::GetAncestors { node, req, .. } => Some((*node, *req)),
            _ => None,
        })
        .expect("GetAncestors");

    // Cancel before delivering the ancestry: the execute pass must abort.
    token.cancel();
    let result = boot.ancestors(node, req, &chain_bytes).await;
    assert!(result.is_err(), "halt must abort execution");
    assert!(!boot.is_finished(), "a halted bootstrapper does not hand off");
    // No blocks were accepted.
    assert!(
        acceptor.accepted.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
        "no blocks accepted after halt"
    );
}
