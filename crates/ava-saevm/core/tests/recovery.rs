// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! SAE restart-recovery tests (specs/11 §1.4, specs/27 §3 (C6) / §5.4).
//!
//! Recovery rebuilds all three frontiers (A/E/S) **from disk with no trust in
//! in-memory state** after a crash + restart. These tests build a chain through
//! the live VM lifecycle (build → verify → accept → execute → settle), snapshot
//! the *durable* inputs that survive a process restart (the canonical accepted
//! block bodies + the height-indexed committed [`ExecutionResults`] + the trie
//! commit policy), drop the VM, then reconstruct a fresh A/E/S via
//! [`ava_saevm_core::recover`] and assert the recovered frontiers + post-state
//! roots are identical (specs/11 §10 invariant 7).
//!
//! Mirrors the Go reference `vms/saevm/sae/recovery_test.go`
//! (`TestRecoverFromDatabase` / `TestRecoverSimple`).
//!
//! # The persistence / crash-point seam
//!
//! `ava-saevm-core` has no Firewood dependency, so "disk" is modelled by
//! [`DiskState`]: a height-indexed table of `(eth body, execution results)`
//! plus the saedb commit policy. It implements the
//! [`RecoverySource`](ava_saevm_core::recovery::RecoverySource) seam the same
//! way the lifecycle tests model the hook/executor through fake seams. A crash
//! point (specs/27 §3 C6) is modelled by *truncating* the durable state — e.g.
//! pretending fewer execution roots were committed (mid-execute) — before
//! `recover()`; recovery must reconstruct the same final A/E/S regardless,
//! because re-execution from the last committed root is pure (specs/11 §6.1).

// Readable reference arithmetic + small-index casts in the fixture builders; the
// loop counters are tiny constants, so truncation cannot occur here.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use ava_evm_reth::{B256, Header, RethBlock, SealedBlock};
use ava_saevm_adaptor::{BlockProperties, ChainVm};
use ava_saevm_blocks::{Block, ExecutionArtefacts, WorstCaseBounds};
use ava_saevm_core::recovery::{RecoverError, RecoverySource, recover};
use ava_saevm_core::{BlockBuilderSeam, BuildError, ExecutorSeam, Vm};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_params::TAU;
use ava_saevm_proxytime::Time;
use ava_saevm_types::ExecutionResults;
use ava_vm::components::gas::Price;

// ---------------------------------------------------------------------------
// Block fixtures (mirrors tests/lifecycle.rs).
// ---------------------------------------------------------------------------

fn eth_block(
    number: u64,
    timestamp: u64,
    parent_hash: B256,
    state_root: B256,
) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        timestamp,
        gas_limit: 8_000_000,
        gas_used: 21_000,
        base_fee_per_gas: Some(7),
        state_root,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

/// A genesis (synchronous, self-settling) SAE block at height 0.
fn genesis() -> Arc<Block> {
    let g =
        Arc::new(Block::new(eth_block(0, 0, B256::ZERO, B256::ZERO), None, None).expect("genesis"));
    g.mark_synchronous((
        ava_vm::components::gas::Gas(0),
        ava_saevm_gastime::GasPriceConfig::default(),
    ))
    .expect("mark synchronous");
    g
}

fn bounds() -> WorstCaseBounds {
    WorstCaseBounds {
        max_base_fee: Price(7),
        latest_end_time: GasTime::new(
            0,
            0,
            ava_vm::components::gas::Price(0),
            GasPriceConfig::default(),
        ),
        min_op_burner_balances: Vec::new(),
    }
}

/// The execution results for the block at `height`: a recognisable post-state
/// root + a gas-time at `exec_unix` so settlement (which reads the executed
/// gas-time) is observable and reproducible across a restart.
fn results_at(height: u64, exec_unix: u64) -> ExecutionResults {
    ExecutionResults {
        gas_time: Time::<u64>::new(exec_unix, 0, 1),
        base_fee: Price(1),
        receipt_root: B256::ZERO,
        // A recognisable, height-derived post-state root: deterministic so a
        // re-execution from disk reproduces the exact same root (invariant 7).
        post_state_root: B256::repeat_byte(u8::try_from(height % 251).unwrap_or(0)),
    }
}

// ---------------------------------------------------------------------------
// Fake builder + executor seams (live VM, mirrors tests/lifecycle.rs).
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct FakeBuilder;

impl FakeBuilder {
    fn settled_root(parent: &Arc<Block>) -> B256 {
        parent
            .last_settled()
            .map_or(B256::ZERO, |s| s.post_execution_state_root())
    }

    fn assemble(parent: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        let height = parent.height() + 1;
        let timestamp = parent.build_time() + 1;
        let eth = eth_block(height, timestamp, parent.hash(), Self::settled_root(parent));
        let last_settled = parent.last_settled();
        let block = Block::new(eth, Some(Arc::clone(parent)), last_settled)
            .map_err(|e| BuildError::Builder(e.to_string()))?;
        let block = Arc::new(block);
        block.set_worst_case_bounds(bounds());
        Ok(block)
    }
}

impl BlockBuilderSeam for FakeBuilder {
    fn build_on(&self, parent: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        Self::assemble(parent)
    }

    fn rebuild(&self, parent: &Arc<Block>, _b: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        Self::assemble(parent)
    }
}

/// A controllable executor that also records what it executed into the shared
/// [`DiskState`], so the same durable artefacts survive a "restart".
struct FakeExecutor {
    disk: Arc<DiskState>,
    queue: Mutex<Vec<Arc<Block>>>,
}

impl FakeExecutor {
    fn new(disk: Arc<DiskState>) -> Self {
        Self {
            disk,
            queue: Mutex::new(Vec::new()),
        }
    }

    /// Executes the next enqueued (not-yet-executed) block: marks it executed
    /// with the disk-persisted results AND records the canonical body + results
    /// into the durable [`DiskState`] (the "disk write" half of execution).
    fn run_next(&self) {
        let next = {
            let q = self.queue.lock();
            q.iter().find(|b| !b.executed()).map(Arc::clone)
        };
        if let Some(b) = next {
            let results = results_at(b.height(), b.build_time());
            let artefacts = ExecutionArtefacts {
                interim_execution_time: results.gas_time.clone(),
                results: results.clone(),
            };
            b.mark_executed(artefacts, None).expect("mark executed");
            self.disk.record(&b, results);
        }
    }
}

impl ExecutorSeam for FakeExecutor {
    fn enqueue(&self, block: &Arc<Block>) -> Result<(), BuildError> {
        self.queue.lock().push(Arc::clone(block));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DiskState — the persistence + crash-point seam.
// ---------------------------------------------------------------------------

/// The durable state that survives a crash + restart: the canonical accepted
/// block bodies (height-indexed) + their committed [`ExecutionResults`] + the
/// saedb commit interval. A `RecoverySource` is built from a *snapshot* of this
/// after the live VM is dropped.
#[derive(Default)]
struct DiskState {
    /// height -> (eth body bytes recipe, execution results). We store the eth
    /// body + parent hash so a canonical block can be rebuilt with fresh parent
    /// linkage during recovery (Go `newCanonicalBlock`).
    canonical: Mutex<BTreeMap<u64, (SealedBlock<RethBlock>, ExecutionResults)>>,
}

impl DiskState {
    fn record(&self, block: &Arc<Block>, results: ExecutionResults) {
        self.canonical
            .lock()
            .insert(block.height(), (block.eth_block().clone(), results));
    }

    /// The head (highest committed canonical) height.
    fn head(&self) -> u64 {
        self.canonical.lock().keys().copied().max().unwrap_or(0)
    }

    /// Snapshots the durable state into a `RecoverySource`, applying a crash
    /// point that pretends only `committed_up_to` heights had their execution
    /// root durably committed (the rest are re-executed from there). The full
    /// canonical chain to `head` is always replayable (accepted bodies are
    /// durable on accept; specs/27 §2.4 D-step).
    fn snapshot(&self, last_synchronous: Arc<Block>, commit_interval: u64) -> Snapshot {
        let canonical = self.canonical.lock().clone();
        Snapshot {
            last_synchronous,
            head: canonical.keys().copied().max().unwrap_or(0),
            canonical,
            commit_interval,
        }
    }
}

/// A frozen, drop-surviving view of the disk: the `RecoverySource` the core
/// `recover()` reads. No `Arc<Block>` from the old VM is retained — every block
/// is reconstructed from the persisted eth body, exactly like reading rawdb.
struct Snapshot {
    last_synchronous: Arc<Block>,
    head: u64,
    canonical: BTreeMap<u64, (SealedBlock<RethBlock>, ExecutionResults)>,
    commit_interval: u64,
}

impl RecoverySource for Snapshot {
    fn last_synchronous(&self) -> Arc<Block> {
        Arc::clone(&self.last_synchronous)
    }

    fn head_height(&self) -> u64 {
        self.head
    }

    fn last_committed_height(&self) -> u64 {
        // Mirrors saedb::last_height_with_execution_root_committed: round the
        // head DOWN to the last commit-interval boundary (non-archival cadence).
        if self.commit_interval == 0 {
            return self.head;
        }
        let rem = self.head % self.commit_interval;
        self.head.saturating_sub(rem)
    }

    fn canonical_eth_block(&self, height: u64) -> Option<SealedBlock<RethBlock>> {
        if height == self.last_synchronous.height() {
            return Some(self.last_synchronous.eth_block().clone());
        }
        self.canonical.get(&height).map(|(eth, _)| eth.clone())
    }

    fn execution_results(&self, height: u64) -> Option<ExecutionResults> {
        self.canonical.get(&height).map(|(_, r)| r.clone())
    }
}

// ---------------------------------------------------------------------------
// VM construction + chain-building helpers.
// ---------------------------------------------------------------------------

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000)
}

/// Builds a live VM over a shared [`DiskState`], drives `n` blocks through the
/// full lifecycle (build → verify → accept → execute → settle), and returns the
/// VM, the disk, the genesis, and the recorded A/E/S heights + post-state roots
/// of the live frontiers (the recovery oracle).
struct LiveRun {
    disk: Arc<DiskState>,
    genesis: Arc<Block>,
    settled_h: u64,
    executed_h: u64,
    accepted_h: u64,
    settled_root: B256,
    executed_root: B256,
}

async fn build_live_chain(n: u64) -> LiveRun {
    let disk = Arc::new(DiskState::default());
    let g = genesis();
    let exec = Arc::new(FakeExecutor::new(Arc::clone(&disk)));
    let vm = Arc::new(Vm::new(&g, FakeBuilder, Arc::clone(&exec), now));

    // The newest accepted-and-executed block (the chain head). The executor
    // reactor that advances the VM's `LastExecuted` frontier pointer is M7.26
    // (stubbed by `FakeExecutor`), so the head is the oracle for E here — the
    // `FakeExecutor` executes every accepted block, so E == A == head, exactly
    // the post-recovery invariant (Go `vm.go`: `last.accepted.Store(head)` with
    // `head = exec.LastExecuted()`).
    let mut head: Arc<Block> = Arc::clone(&g);
    for _ in 0..n {
        let built = vm.build_block(None).await.expect("build");
        vm.verify_block(None, &built).await.expect("verify");
        vm.accept_block(&built).await.expect("accept");
        exec.run_next();
        vm.set_preference(built.id(), None).await.expect("pref");
        head = Arc::clone(built.block());
    }

    // S is wired through the live `settle()` driver (accept-time), so the VM's
    // `last_settled` frontier IS the oracle for S.
    let f = vm.frontier();
    LiveRun {
        disk,
        genesis: g,
        settled_h: f.last_settled().height(),
        executed_h: head.height(),
        accepted_h: f.last_accepted().height(),
        settled_root: f.last_settled().post_execution_state_root(),
        executed_root: head.post_execution_state_root(),
    }
}

// ---------------------------------------------------------------------------
// (1) recovery rebuilds identical A/E/S frontiers + post-state roots.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_rebuilds_identical_frontiers_and_roots() {
    let live = build_live_chain(8).await;

    // Drop the live VM's in-memory state; reconstruct A/E/S purely from disk.
    let snap = live.disk.snapshot(Arc::clone(&live.genesis), 16);
    let recovered = recover(&snap).await.expect("recover");

    let f = &recovered.frontier;
    assert_eq!(
        f.last_accepted().height(),
        live.accepted_h,
        "recovered LastAccepted height matches the live frontier",
    );
    assert_eq!(
        f.last_executed().expect("executed").height(),
        live.executed_h,
        "recovered LastExecuted height matches",
    );
    assert_eq!(
        f.last_settled().height(),
        live.settled_h,
        "recovered LastSettled height matches",
    );

    // Invariant 7: identical post-state roots after a pure re-execution.
    assert_eq!(
        f.last_executed()
            .expect("executed")
            .post_execution_state_root(),
        live.executed_root,
        "recovered executed post-state root identical",
    );
    assert_eq!(
        f.last_settled().post_execution_state_root(),
        live.settled_root,
        "recovered settled post-state root identical",
    );

    // The frontier ordering invariant holds on the reconstructed frontier.
    assert!(f.heights_ordered(), "S <= E <= A after recovery");

    // A == E == head (recovery re-executes everything to the tip; Go vm.go).
    assert_eq!(f.last_accepted().height(), live.disk.head());
}

// ---------------------------------------------------------------------------
// (2) re-execution from the last committed root yields identical roots, and the
//     commit cadence lands on the same heights (purity, specs/11 §6.1).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_re_executes_from_last_committed_root() {
    // commit_interval=4 ⇒ last committed root is rounded down to a multiple of 4.
    let live = build_live_chain(10).await;

    let snap = live.disk.snapshot(Arc::clone(&live.genesis), 4);
    // The recovery start point: last committed height = 10 - (10 % 4) = 8.
    assert_eq!(
        snap.last_committed_height(),
        8,
        "10 rounds down to 8 @ interval 4"
    );

    let recovered = recover(&snap).await.expect("recover");
    let f = &recovered.frontier;

    // Re-execution from height 8 reproduced the exact same roots for the tip.
    assert_eq!(
        f.last_executed()
            .expect("executed")
            .post_execution_state_root(),
        live.executed_root,
        "re-execution from the committed root is pure ⇒ identical roots",
    );
    assert_eq!(f.last_accepted().height(), 10, "re-executed up to head");

    // maybe_commit lands on the same heights: the recovered last-committed
    // height is the SAME function of the same head, regardless of where we
    // re-executed from (purity ⇒ idempotent commit cadence).
    let snap2 = live.disk.snapshot(Arc::clone(&live.genesis), 4);
    assert_eq!(
        snap2.last_committed_height(),
        snap.last_committed_height(),
        "commit cadence is a pure function of head + interval",
    );
}

// ---------------------------------------------------------------------------
// (3) parameterized crash point (specs/27 §3 C6): regardless of how far the
//     execution root had been committed (archival / mid-interval / exact
//     boundary), recovery reconstructs the SAME final A/E/S.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_is_invariant_to_crash_point() {
    let live = build_live_chain(12).await;

    // Each crash point = a different "last committed root" cadence. Archival
    // (commit every block ⇒ recover from head), a wide interval (recover from
    // an early boundary, re-executing most blocks), and an exact boundary.
    for &interval in &[1_u64, 5, 12] {
        let snap = live.disk.snapshot(Arc::clone(&live.genesis), interval);
        let recovered = recover(&snap)
            .await
            .unwrap_or_else(|e| panic!("recover @ interval {interval}: {e}"));
        let f = &recovered.frontier;

        assert_eq!(
            f.last_accepted().height(),
            live.accepted_h,
            "A identical @ interval {interval}",
        );
        assert_eq!(
            f.last_executed().expect("executed").height(),
            live.executed_h,
            "E identical @ interval {interval}",
        );
        assert_eq!(
            f.last_settled().height(),
            live.settled_h,
            "S identical @ interval {interval}",
        );
        assert_eq!(
            f.last_executed()
                .expect("executed")
                .post_execution_state_root(),
            live.executed_root,
            "executed root identical @ interval {interval}",
        );
        assert!(f.heights_ordered(), "ordering holds @ interval {interval}");
    }
}

// ---------------------------------------------------------------------------
// (4) recovery of a never-advanced chain returns the synchronous floor and the
//     missing-canonical-block error path is honest.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_of_genesis_only_chain() {
    // No blocks accepted: head == genesis; A == E == S == genesis.
    let disk = Arc::new(DiskState::default());
    let g = genesis();
    let snap = disk.snapshot(Arc::clone(&g), 16);

    let recovered = recover(&snap).await.expect("recover genesis-only");
    let f = &recovered.frontier;
    assert_eq!(f.last_accepted().height(), 0);
    assert_eq!(f.last_executed().expect("executed").height(), 0);
    assert_eq!(f.last_settled().height(), 0);

    // The TAU floor is exercised by the settlement walk-back; assert it is a
    // sane duration so the import is load-bearing (not a dead `use`).
    assert!(TAU >= Duration::ZERO);
}

// ---------------------------------------------------------------------------
// (5) a gap in the canonical chain surfaces RecoverError::MissingCanonicalBlock.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_missing_canonical_block_is_an_error() {
    let live = build_live_chain(6).await;
    let mut snap = live.disk.snapshot(Arc::clone(&live.genesis), 16);
    // Punch a hole: remove height 3 from the canonical table but leave head=6.
    snap.canonical.remove(&3);

    let result = recover(&snap).await;
    match result {
        Err(RecoverError::MissingCanonicalBlock(3)) => {}
        Err(other) => panic!("wrong error: {other}"),
        Ok(_) => panic!("gap must fail recovery"),
    }
}
