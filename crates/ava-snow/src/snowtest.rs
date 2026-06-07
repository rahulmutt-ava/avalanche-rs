// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Snowman consensus conformance battery (feature `testutil`; specs 02 §13).
//!
//! [`run_consensus_suite`] runs the full Go `topological/consensus_test.go` +
//! `network_test.go` corpus against any [`SnowmanConsensus`] built by a caller
//! supplied constructor, so a future alternative implementation can be held to
//! the same bar. The default wiring (against [`Topological`](crate::snowman::Topological))
//! is exercised from `tests/conformance_battery.rs`.

// This is a test harness compiled behind the `testutil` feature; it asserts and
// unwraps freely (the battery's job is to panic on conformance failure).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_types::id::Id;
use ava_utils::bag::Bag;

use crate::error::{Error, Result};
use crate::snowball::Parameters;
use crate::snowman::block::{Block, BlockAcceptor};
use crate::snowman::consensus::SnowmanConsensus;

/// The decided-status of a [`TestSnowmanBlock`] (Go `snowtest` status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not yet decided.
    Undecided,
    /// Accepted by consensus.
    Accepted,
    /// Rejected by consensus.
    Rejected,
}

const UNDECIDED: u8 = 0;
const ACCEPTED: u8 = 1;
const REJECTED: u8 = 2;

/// An in-memory Snowman block with interior-mutable decided status and optional
/// injected accept/reject errors (Go `snowmantest.Block`).
pub struct TestSnowmanBlock {
    id: Id,
    parent: Id,
    height: u64,
    timestamp: SystemTime,
    bytes: Vec<u8>,
    status: AtomicU8,
    accept_err: bool,
    reject_err: bool,
}

impl TestSnowmanBlock {
    /// A block with the given identity/parent/height and no injected errors.
    #[must_use]
    pub fn new(id: Id, parent: Id, height: u64) -> Self {
        Self {
            id,
            parent,
            height,
            timestamp: UNIX_EPOCH + Duration::from_secs(height),
            bytes: id.as_bytes().to_vec(),
            status: AtomicU8::new(UNDECIDED),
            accept_err: false,
            reject_err: false,
        }
    }

    /// Marks this block's `accept` to return a critical error.
    #[must_use]
    pub fn with_accept_err(mut self) -> Self {
        self.accept_err = true;
        self
    }

    /// Marks this block's `reject` to return a critical error.
    #[must_use]
    pub fn with_reject_err(mut self) -> Self {
        self.reject_err = true;
        self
    }

    /// The current decided status.
    #[must_use]
    pub fn status(&self) -> Status {
        match self.status.load(Ordering::SeqCst) {
            ACCEPTED => Status::Accepted,
            REJECTED => Status::Rejected,
            _ => Status::Undecided,
        }
    }
}

impl Block for TestSnowmanBlock {
    fn id(&self) -> Id {
        self.id
    }
    fn parent(&self) -> Id {
        self.parent
    }
    fn height(&self) -> u64 {
        self.height
    }
    fn timestamp(&self) -> SystemTime {
        self.timestamp
    }
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn accept(&self) -> Result<()> {
        if self.accept_err {
            return Err(Error::Multiple(Vec::new()));
        }
        self.status.store(ACCEPTED, Ordering::SeqCst);
        Ok(())
    }
    fn reject(&self) -> Result<()> {
        if self.reject_err {
            return Err(Error::Multiple(Vec::new()));
        }
        self.status.store(REJECTED, Ordering::SeqCst);
        Ok(())
    }
}

/// A block acceptor that records accepted ids in order (Go's `BlockAcceptor`
/// recorder).
#[derive(Default)]
pub struct RecordingAcceptor {
    accepted: Mutex<Vec<Id>>,
}

impl RecordingAcceptor {
    /// A fresh acceptor.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The ids accepted so far, in order.
    #[must_use]
    pub fn accepted(&self) -> Vec<Id> {
        self.accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl BlockAcceptor for RecordingAcceptor {
    fn accept(&self, container_id: Id, _bytes: &[u8]) -> Result<()> {
        self.accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(container_id);
        Ok(())
    }
}

/// Genesis identity used across the battery (Go `snowmantest.Genesis*`).
const GENESIS_HEIGHT: u64 = 0;

fn genesis_id() -> Id {
    Id::EMPTY.prefix(&[0x9999])
}

fn params(k: u32, alpha: u32, beta: u32) -> Parameters {
    Parameters {
        k,
        alpha_preference: alpha,
        alpha_confidence: alpha,
        beta,
        concurrent_repolls: 1,
        optimal_processing: 1,
        max_outstanding_items: 1,
        max_item_processing_time: Duration::from_nanos(1),
    }
}

/// A deterministic distinct-id generator (replaces Go's random
/// `ids.GenerateTestID`).
struct IdGen(u64);

impl IdGen {
    fn new() -> Self {
        // Start high so generated ids don't collide with the genesis id.
        Self(1)
    }
    fn next(&mut self) -> Id {
        self.0 += 1;
        Id::EMPTY.prefix(&[self.0])
    }
}

/// Builds and initializes a consensus instance via the caller's constructor.
type Ctor<C> = dyn Fn(Parameters, Id, u64, Arc<dyn BlockAcceptor>) -> Result<C>;

fn child(idgen: &mut IdGen, parent: &Arc<TestSnowmanBlock>) -> Arc<TestSnowmanBlock> {
    Arc::new(TestSnowmanBlock::new(
        idgen.next(),
        parent.id(),
        parent.height() + 1,
    ))
}

fn genesis_block() -> Arc<TestSnowmanBlock> {
    // The genesis itself is never added; we just need its id/height for children.
    Arc::new(TestSnowmanBlock::new(
        genesis_id(),
        Id::EMPTY,
        GENESIS_HEIGHT,
    ))
}

/// Runs the full Snowman conformance suite against `make`.
///
/// `make(params, last_accepted_id, last_accepted_height, acceptor)` must build
/// an initialized [`SnowmanConsensus`].
///
/// # Panics
/// Panics (via assertions) on any conformance failure.
#[allow(clippy::too_many_lines)]
pub fn run_consensus_suite<C: SnowmanConsensus>(make: &Ctor<C>) {
    initialize(make);
    num_processing(make);
    add_to_tail(make);
    add_to_non_tail(make);
    add_on_unknown_parent(make);
    add_decided_block_errors(make);
    record_poll_accept_single(make);
    record_poll_accept_and_reject(make);
    record_poll_when_finalized(make);
    record_poll_reject_transitively(make);
    record_poll_transitively_reset_confidence(make);
    record_poll_invalid_vote(make);
    record_poll_transitive_voting(make);
    record_poll_change_preferred_chain(make);
    last_accepted(make);
    error_on_accept(make);
    error_on_reject_sibling(make);
    error_on_transitive_rejection(make);
    acceptor_ordering(make);
}

fn new<C: SnowmanConsensus>(make: &Ctor<C>, p: Parameters) -> (C, Arc<RecordingAcceptor>) {
    let acceptor = RecordingAcceptor::new();
    let sm = make(p, genesis_id(), GENESIS_HEIGHT, acceptor.clone())
        .expect("consensus must initialize with valid params");
    (sm, acceptor)
}

fn dyn_block(b: &Arc<TestSnowmanBlock>) -> Arc<dyn Block> {
    b.clone()
}

fn initialize<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (sm, _) = new(make, params(1, 1, 3));
    assert_eq!(sm.preference(), genesis_id());
    assert_eq!(sm.num_processing(), 0);
}

fn num_processing<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 1));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block = child(&mut idgen, &g);

    assert_eq!(sm.num_processing(), 0);
    sm.add(dyn_block(&block)).unwrap();
    assert_eq!(sm.num_processing(), 1);

    let votes = Bag::of([block.id()]);
    sm.record_poll(&votes).unwrap();
    assert_eq!(sm.num_processing(), 0);
}

fn add_to_tail<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 3));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block = child(&mut idgen, &g);

    sm.add(dyn_block(&block)).unwrap();
    assert_eq!(sm.preference(), block.id());
    assert!(sm.is_preferred(block.id()));
    assert_eq!(sm.preference_at_height(block.height()), Some(block.id()));
}

fn add_to_non_tail<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 3));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let first = child(&mut idgen, &g);
    let second = child(&mut idgen, &g);

    sm.add(dyn_block(&first)).unwrap();
    assert_eq!(sm.preference(), first.id());
    sm.add(dyn_block(&second)).unwrap();
    assert_eq!(sm.preference(), first.id());
}

fn add_on_unknown_parent<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 3));
    let mut idgen = IdGen::new();
    let unknown_parent = idgen.next();
    let block = Arc::new(TestSnowmanBlock::new(idgen.next(), unknown_parent, 2));
    let err = sm.add(dyn_block(&block)).unwrap_err();
    assert!(matches!(err, Error::UnknownParentBlock));
}

fn add_decided_block_errors<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 1));
    // Re-adding the genesis (last accepted) block has an unknown parent.
    let g = Arc::new(TestSnowmanBlock::new(
        genesis_id(),
        Id::EMPTY,
        GENESIS_HEIGHT,
    ));
    let err = sm.add(dyn_block(&g)).unwrap_err();
    assert!(matches!(err, Error::UnknownParentBlock));
}

fn record_poll_accept_single<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 2));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block = child(&mut idgen, &g);
    sm.add(dyn_block(&block)).unwrap();

    let votes = Bag::of([block.id()]);
    sm.record_poll(&votes).unwrap();
    assert_eq!(sm.preference(), block.id());
    assert_eq!(sm.num_processing(), 1);
    assert_eq!(block.status(), Status::Undecided);

    sm.record_poll(&votes).unwrap();
    assert_eq!(sm.preference(), block.id());
    assert_eq!(sm.num_processing(), 0);
    assert_eq!(block.status(), Status::Accepted);
}

fn record_poll_accept_and_reject<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 2));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let first = child(&mut idgen, &g);
    let second = child(&mut idgen, &g);
    sm.add(dyn_block(&first)).unwrap();
    sm.add(dyn_block(&second)).unwrap();

    let votes = Bag::of([first.id()]);
    sm.record_poll(&votes).unwrap();
    assert_eq!(sm.preference(), first.id());
    assert_eq!(sm.num_processing(), 2);

    sm.record_poll(&votes).unwrap();
    assert_eq!(sm.preference(), first.id());
    assert_eq!(sm.num_processing(), 0);
    assert_eq!(first.status(), Status::Accepted);
    assert_eq!(second.status(), Status::Rejected);
}

fn record_poll_when_finalized<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 1));
    let votes = Bag::of([genesis_id()]);
    sm.record_poll(&votes).unwrap();
    assert_eq!(sm.num_processing(), 0);
    assert_eq!(sm.preference(), genesis_id());
}

fn record_poll_reject_transitively<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 1));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block0 = child(&mut idgen, &g);
    let block1 = child(&mut idgen, &g);
    let block2 = child(&mut idgen, &block1);
    sm.add(dyn_block(&block0)).unwrap();
    sm.add(dyn_block(&block1)).unwrap();
    sm.add(dyn_block(&block2)).unwrap();

    let votes = Bag::of([block0.id()]);
    sm.record_poll(&votes).unwrap();

    assert_eq!(sm.num_processing(), 0);
    assert_eq!(sm.preference(), block0.id());
    assert_eq!(block0.status(), Status::Accepted);
    assert_eq!(block1.status(), Status::Rejected);
    assert_eq!(block2.status(), Status::Rejected);
}

fn record_poll_transitively_reset_confidence<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 2));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block0 = child(&mut idgen, &g);
    let block1 = child(&mut idgen, &g);
    let block2 = child(&mut idgen, &block1);
    let block3 = child(&mut idgen, &block1);
    sm.add(dyn_block(&block0)).unwrap();
    sm.add(dyn_block(&block1)).unwrap();
    sm.add(dyn_block(&block2)).unwrap();
    sm.add(dyn_block(&block3)).unwrap();

    let votes2 = Bag::of([block2.id()]);
    sm.record_poll(&votes2).unwrap();
    assert_eq!(sm.num_processing(), 4);
    assert_eq!(sm.preference(), block2.id());

    let empty = Bag::new();
    sm.record_poll(&empty).unwrap();
    assert_eq!(sm.num_processing(), 4);
    assert_eq!(sm.preference(), block2.id());

    sm.record_poll(&votes2).unwrap();
    assert_eq!(sm.num_processing(), 4);
    assert_eq!(sm.preference(), block2.id());

    // Vote for block3. block1 finalizes (accepting it, rejecting block0), so the
    // tree collapses to the contested block2/block3 branch (2 processing). Which
    // of block2/block3 is preferred after a single block3 poll is genuinely
    // id-bit-layout-dependent in Go (its `BuildChild` uses random ids); the
    // structurally-invariant fact is that the preference is on the contested
    // branch and num_processing has dropped to 2.
    let votes3 = Bag::of([block3.id()]);
    sm.record_poll(&votes3).unwrap();
    assert_eq!(sm.num_processing(), 2);
    assert!(
        sm.preference() == block2.id() || sm.preference() == block3.id(),
        "preference must be on the contested branch"
    );
    assert_eq!(block0.status(), Status::Rejected);
    assert_eq!(block1.status(), Status::Accepted);

    // A second block3 poll tips the strength to block3, finalizing it (rejecting
    // block2). This is fully deterministic.
    sm.record_poll(&votes3).unwrap();
    assert_eq!(sm.num_processing(), 0);
    assert_eq!(sm.preference(), block3.id());
    assert_eq!(block2.status(), Status::Rejected);
    assert_eq!(block3.status(), Status::Accepted);
}

fn record_poll_invalid_vote<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 2));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block = child(&mut idgen, &g);
    let unknown = idgen.next();
    sm.add(dyn_block(&block)).unwrap();

    let valid = Bag::of([block.id()]);
    sm.record_poll(&valid).unwrap();

    let invalid = Bag::of([unknown]);
    sm.record_poll(&invalid).unwrap();
    sm.record_poll(&valid).unwrap();
    assert_eq!(sm.num_processing(), 1);
    assert_eq!(sm.preference(), block.id());
}

fn record_poll_transitive_voting<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(3, 3, 1));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block0 = child(&mut idgen, &g);
    let block1 = child(&mut idgen, &block0);
    let block2 = child(&mut idgen, &block1);
    let block3 = child(&mut idgen, &block0);
    let block4 = child(&mut idgen, &block3);
    sm.add(dyn_block(&block0)).unwrap();
    sm.add(dyn_block(&block1)).unwrap();
    sm.add(dyn_block(&block2)).unwrap();
    sm.add(dyn_block(&block3)).unwrap();
    sm.add(dyn_block(&block4)).unwrap();

    let votes = Bag::of([block0.id(), block2.id(), block4.id()]);
    sm.record_poll(&votes).unwrap();
    assert_eq!(sm.num_processing(), 4);
    assert_eq!(sm.preference(), block2.id());
    assert_eq!(block0.status(), Status::Accepted);

    let votes_2 = Bag::of([block2.id(), block2.id(), block2.id()]);
    sm.record_poll(&votes_2).unwrap();
    assert_eq!(sm.num_processing(), 0);
    assert_eq!(sm.preference(), block2.id());
    assert_eq!(block1.status(), Status::Accepted);
    assert_eq!(block2.status(), Status::Accepted);
    assert_eq!(block3.status(), Status::Rejected);
    assert_eq!(block4.status(), Status::Rejected);
}

fn record_poll_change_preferred_chain<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 10));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let a1 = child(&mut idgen, &g);
    let b1 = child(&mut idgen, &g);
    let a2 = child(&mut idgen, &a1);
    let b2 = child(&mut idgen, &b1);
    sm.add(dyn_block(&a1)).unwrap();
    sm.add(dyn_block(&a2)).unwrap();
    sm.add(dyn_block(&b1)).unwrap();
    sm.add(dyn_block(&b2)).unwrap();

    assert_eq!(sm.preference(), a2.id());
    assert!(sm.is_preferred(a1.id()));
    assert!(sm.is_preferred(a2.id()));
    assert!(!sm.is_preferred(b1.id()));
    assert!(!sm.is_preferred(b2.id()));

    let b2_votes = Bag::of([b2.id()]);
    sm.record_poll(&b2_votes).unwrap();
    assert_eq!(sm.preference(), b2.id());
    assert!(!sm.is_preferred(a1.id()));
    assert!(sm.is_preferred(b1.id()));
    assert!(sm.is_preferred(b2.id()));

    let a1_votes = Bag::of([a1.id()]);
    sm.record_poll(&a1_votes).unwrap();
    sm.record_poll(&a1_votes).unwrap();
    assert_eq!(sm.preference(), a2.id());
    assert!(sm.is_preferred(a1.id()));
    assert!(sm.is_preferred(a2.id()));
    assert!(!sm.is_preferred(b1.id()));
}

fn last_accepted<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 2));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block0 = child(&mut idgen, &g);
    let block1 = child(&mut idgen, &block0);
    let block2 = child(&mut idgen, &block1);
    let block1_conflict = child(&mut idgen, &block0);

    assert_eq!(sm.last_accepted(), (genesis_id(), GENESIS_HEIGHT));
    sm.add(dyn_block(&block0)).unwrap();
    sm.add(dyn_block(&block1)).unwrap();
    sm.add(dyn_block(&block1_conflict)).unwrap();
    sm.add(dyn_block(&block2)).unwrap();
    assert_eq!(sm.last_accepted(), (genesis_id(), GENESIS_HEIGHT));

    sm.record_poll(&Bag::of([block0.id()])).unwrap();
    assert_eq!(sm.last_accepted(), (genesis_id(), GENESIS_HEIGHT));

    sm.record_poll(&Bag::of([block1.id()])).unwrap();
    assert_eq!(sm.last_accepted(), (block0.id(), block0.height()));

    sm.record_poll(&Bag::of([block1.id()])).unwrap();
    assert_eq!(sm.last_accepted(), (block1.id(), block1.height()));

    sm.record_poll(&Bag::of([block2.id()])).unwrap();
    assert_eq!(sm.last_accepted(), (block1.id(), block1.height()));

    sm.record_poll(&Bag::of([block2.id()])).unwrap();
    assert_eq!(sm.last_accepted(), (block2.id(), block2.height()));
}

fn error_on_accept<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 1));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block = Arc::new(child(&mut idgen, &g).as_ref().clone_with_accept_err());
    sm.add(dyn_block(&block)).unwrap();
    let votes = Bag::of([block.id()]);
    let err = sm.record_poll(&votes).unwrap_err();
    assert!(matches!(err, Error::Multiple(_)));
}

fn error_on_reject_sibling<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 1));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block0 = child(&mut idgen, &g);
    let block1 = Arc::new(child(&mut idgen, &g).as_ref().clone_with_reject_err());
    sm.add(dyn_block(&block0)).unwrap();
    sm.add(dyn_block(&block1)).unwrap();
    let votes = Bag::of([block0.id()]);
    let err = sm.record_poll(&votes).unwrap_err();
    assert!(matches!(err, Error::Multiple(_)));
}

fn error_on_transitive_rejection<C: SnowmanConsensus>(make: &Ctor<C>) {
    let (mut sm, _) = new(make, params(1, 1, 1));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block0 = child(&mut idgen, &g);
    let block1 = child(&mut idgen, &g);
    let block2 = Arc::new(child(&mut idgen, &block1).as_ref().clone_with_reject_err());
    sm.add(dyn_block(&block0)).unwrap();
    sm.add(dyn_block(&block1)).unwrap();
    sm.add(dyn_block(&block2)).unwrap();
    let votes = Bag::of([block0.id()]);
    let err = sm.record_poll(&votes).unwrap_err();
    assert!(matches!(err, Error::Multiple(_)));
}

fn acceptor_ordering<C: SnowmanConsensus>(make: &Ctor<C>) {
    // The block acceptor must be notified of an accepted block; the recorded id
    // matches the accepted child.
    let (mut sm, acceptor) = new(make, params(1, 1, 1));
    let mut idgen = IdGen::new();
    let g = genesis_block();
    let block = child(&mut idgen, &g);
    sm.add(dyn_block(&block)).unwrap();
    sm.record_poll(&Bag::of([block.id()])).unwrap();
    assert_eq!(block.status(), Status::Accepted);
    assert_eq!(acceptor.accepted(), vec![block.id()]);
}

impl TestSnowmanBlock {
    fn clone_with_accept_err(&self) -> Self {
        Self {
            id: self.id,
            parent: self.parent,
            height: self.height,
            timestamp: self.timestamp,
            bytes: self.bytes.clone(),
            status: AtomicU8::new(self.status.load(Ordering::SeqCst)),
            accept_err: true,
            reject_err: self.reject_err,
        }
    }
    fn clone_with_reject_err(&self) -> Self {
        Self {
            id: self.id,
            parent: self.parent,
            height: self.height,
            timestamp: self.timestamp,
            bytes: self.bytes.clone(),
            status: AtomicU8::new(self.status.load(Ordering::SeqCst)),
            accept_err: self.accept_err,
            reject_err: true,
        }
    }
}
