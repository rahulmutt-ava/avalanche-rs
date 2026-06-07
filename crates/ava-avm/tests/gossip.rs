// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for M5.18: tx gossip handler + atomic app-handler switch.
//!
//! Covers:
//! * `Gossipable` impl for `Tx` — `gossip_id()` returns the tx id.
//! * `TxMarshaller` round-trip (marshal → unmarshal == same id).
//! * Inbound valid tx is admitted; re-gossip is deduped.
//! * Invalid/uninitialized tx is dropped with `DropReason::Verification`.
//! * Mempool-conflict is dropped with `DropReason::Mempool`.
//! * `AtomicAppHandler` swap routes calls to the currently-loaded handler.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use ava_avm::mempool::Mempool;
use ava_avm::network::atomic::{AppGossipHandler, AtomicAppHandler};
use ava_avm::network::gossip::{Gossipable, TxGossipHandler, TxMarshaller};
use ava_avm::network::tx_verifier::SyntacticTxVerifier;
use ava_avm::txs::Tx;
use ava_avm::txs::codec::codec;
use ava_avm::txs::components::AvaxBaseTx;
use ava_avm::{BaseTx, UnsignedTx};
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use ava_avm::network::gossip::{DropReason, HandleOutcome};

fn make_tx(tag: u32) -> Tx {
    let c = codec().expect("codec");
    let base = BaseTx::new(AvaxBaseTx {
        network_id: 1,
        blockchain_id: Id::EMPTY,
        outs: vec![],
        ins: vec![],
        memo: tag.to_be_bytes().to_vec(),
    });
    let mut tx = Tx::new(UnsignedTx::Base(base));
    tx.initialize(&c).expect("initialize");
    tx
}

// ---------------------------------------------------------------------------
// Gossipable
// ---------------------------------------------------------------------------

#[test]
fn gossipable_id_matches_tx_id() {
    let tx = make_tx(1);
    assert_eq!(tx.gossip_id(), tx.id());
    assert_ne!(tx.gossip_id(), Id::EMPTY);
}

// ---------------------------------------------------------------------------
// TxMarshaller round-trip
// ---------------------------------------------------------------------------

#[test]
fn marshaller_roundtrip() {
    let tx = make_tx(42);
    let m = TxMarshaller::new();
    let bytes = m.marshal(&tx);
    let tx2 = m.unmarshal(&bytes).expect("unmarshal");
    assert_eq!(tx2.id(), tx.id());
    assert_eq!(tx2.bytes(), tx.bytes());
}

// ---------------------------------------------------------------------------
// TxGossipHandler + SyntacticTxVerifier
// ---------------------------------------------------------------------------

#[test]
fn admits_valid_tx_then_dedupes() {
    let h = TxGossipHandler::new();
    let v = SyntacticTxVerifier;
    let mut m = Mempool::new();

    let tx = make_tx(1);
    let out = h.handle_gossiped_tx(&mut m, &v, tx.clone());
    assert_eq!(out, HandleOutcome::Added);
    assert_eq!(m.len(), 1);

    // Re-gossip of same tx → deduped, mempool unchanged.
    let out2 = h.handle_gossiped_tx(&mut m, &v, tx.clone());
    assert_eq!(out2, HandleOutcome::Dropped(DropReason::Duplicate));
    assert_eq!(m.len(), 1);
}

#[test]
fn order_independent_convergence() {
    // Two nodes receiving the same three txs in opposite orders admit the same set.
    let h = TxGossipHandler::new();
    let v = SyntacticTxVerifier;
    let txs = [make_tx(1), make_tx(2), make_tx(3)];

    let mut a = Mempool::new();
    h.handle_gossiped_txs(&mut a, &v, txs.iter().cloned());

    let mut b = Mempool::new();
    h.handle_gossiped_txs(&mut b, &v, txs.iter().rev().cloned());

    let mut ids_a: Vec<Id> = a.snapshot().iter().map(Tx::id).collect();
    let mut ids_b: Vec<Id> = b.snapshot().iter().map(Tx::id).collect();
    ids_a.sort();
    ids_b.sort();
    assert_eq!(ids_a, ids_b);
    assert_eq!(ids_a.len(), 3);
}

#[test]
fn drops_uninitialized_tx() {
    let h = TxGossipHandler::new();
    let v = SyntacticTxVerifier;
    let mut m = Mempool::new();

    // An uninitialized tx has id == EMPTY and bytes == [].
    let uninit = Tx::new(UnsignedTx::default());
    let out = h.handle_gossiped_tx(&mut m, &v, uninit);
    assert!(matches!(
        out,
        HandleOutcome::Dropped(DropReason::Verification(_))
    ));
    assert!(m.is_empty());
}

#[test]
fn drops_mempool_full() {
    let h = TxGossipHandler::new();
    let v = SyntacticTxVerifier;

    // Build a pool whose byte budget is exactly zero — any tx will fail with
    // MempoolFull, which maps to DropReason::Mempool.
    let mut m = Mempool::with_budget(0);

    let tx = make_tx(10);
    let out = h.handle_gossiped_tx(&mut m, &v, tx);

    // The handler must produce Dropped(Mempool(_)) — the arm that was never
    // exercised before this fix.
    assert!(
        matches!(out, HandleOutcome::Dropped(DropReason::Mempool(_))),
        "expected Dropped(Mempool(_)), got {out:?}",
    );
    // Divergence-free: the pool must remain untouched.
    assert!(m.is_empty());
}

// ---------------------------------------------------------------------------
// AtomicAppHandler swap
// ---------------------------------------------------------------------------

/// A fake handler that increments a shared counter when called.
struct CountingHandler {
    count: Arc<AtomicUsize>,
}

impl AppGossipHandler for CountingHandler {
    fn handle_app_gossip(&self, _node: NodeId, _msg: &[u8]) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn atomic_handler_routes_to_current_handler() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));

    let handler_a: Arc<dyn AppGossipHandler> = Arc::new(CountingHandler {
        count: Arc::clone(&count_a),
    });
    let handler_b: Arc<dyn AppGossipHandler> = Arc::new(CountingHandler {
        count: Arc::clone(&count_b),
    });

    let atomic = AtomicAppHandler::new(Arc::clone(&handler_a));

    // Before swap: calls go to handler_a.
    atomic.handle_app_gossip(NodeId::default(), &[]);
    assert_eq!(count_a.load(Ordering::SeqCst), 1);
    assert_eq!(count_b.load(Ordering::SeqCst), 0);

    // Swap to handler_b.
    atomic.swap(Arc::clone(&handler_b));

    // After swap: calls go to handler_b.
    atomic.handle_app_gossip(NodeId::default(), &[]);
    assert_eq!(count_a.load(Ordering::SeqCst), 1); // unchanged
    assert_eq!(count_b.load(Ordering::SeqCst), 1);

    // Swap back to handler_a.
    atomic.swap(handler_a);
    atomic.handle_app_gossip(NodeId::default(), &[]);
    assert_eq!(count_a.load(Ordering::SeqCst), 2);
    assert_eq!(count_b.load(Ordering::SeqCst), 1);
}
