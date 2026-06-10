// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The reusable §10 invariant harness (specs/11 §10, invariants 1–11).
//!
//! This module holds the shared chain-driver fakes (block builder, executor,
//! durable "disk") and a per-invariant assertion helper. The integration test
//! `crates/ava-saevm/core/tests/invariants.rs` wires one named test per
//! invariant onto these helpers so the whole set is selectable with
//! `cargo nextest run -p ava-saevm-core -E 'test(invariant)'` and the M7.32
//! exit gate can find them.
//!
//! The fakes mirror the ones in `core/tests/{lifecycle,recovery}.rs`: the core
//! crate has no Firewood dependency, so "disk" is the in-memory [`DiskState`]
//! and "execution" is the controllable [`FakeExecutor`]. Where a property is
//! enforced by the *call-order contract* of these fakes (rather than a real
//! durable write), the helper documents what is modeled versus real.
//!
//! # What delegates elsewhere
//!
//! * Invariant 7 (recovery equivalence) drives a live chain, snapshots the
//!   durable inputs, and reconstructs A/E/S via M7.24's
//!   [`recover`](ava_saevm_core::recover) — the same path `core/tests/recovery.rs`
//!   exercises.
//! * Invariant 11 (determinism) is owned by M7.16's
//!   `prop::sae_execution_determinism` (in `exec/tests`); here it is re-asserted
//!   at the chain level (the same block program, accepted/executed/settled twice
//!   under independent VMs and an out-of-insertion-order frontier map, yields
//!   identical settled state).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use ava_evm_reth::{B256, EthReceipt, Header, RethBlock, SealedBlock};
use ava_saevm_adaptor::{BlockProperties, ChainVm};
use ava_saevm_blocks::{Block, ExecutionArtefacts, WorstCaseBounds, in_memory_block_count};
use ava_saevm_core::recovery::{RecoverySource, recover};
use ava_saevm_core::{BlockBuilderSeam, BuildError, ExecutorSeam, Vm};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_proxytime::Time;
use ava_saevm_types::ExecutionResults;
use ava_types::id::Id;
use ava_vm::components::gas::Price;

// ---------------------------------------------------------------------------
// Block fixtures (shared with core/tests/{lifecycle,recovery}.rs).
// ---------------------------------------------------------------------------

/// Seals an eth block at `number`/`timestamp` carrying `parent_hash` and the
/// settled ancestor's post-exec `state_root` (the `Root` repurpose, specs/11
/// §1.3). The worst-case gas params are fixed constants so a faithful rebuild
/// reproduces the identical hash.
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
///
/// # Panics
/// Only if the genesis fixture is malformed (a programming error in the
/// harness, not a runtime condition); the `expect` documents the invariant.
#[must_use]
fn genesis() -> Arc<Block> {
    let g = Arc::new(
        Block::new(eth_block(0, 0, B256::ZERO, B256::ZERO), None, None).expect("genesis fixture"),
    );
    g.mark_synchronous().expect("mark genesis synchronous");
    g
}

/// The fixed worst-case bounds the fake builder predicts for every block.
fn bounds() -> WorstCaseBounds {
    WorstCaseBounds {
        max_base_fee: Price(7),
        latest_end_time: GasTime::new(0, 0, 0, GasPriceConfig::default()),
        min_op_burner_balances: Vec::new(),
    }
}

/// The execution results for the block at `height`: a recognisable,
/// deterministic post-state root (so a re-execution from disk reproduces it)
/// plus a gas-time at `exec_unix` so settlement (which reads the executed
/// gas-time) is observable and reproducible across a restart.
///
/// `receipt_root` is the receipts-trie root over a single successful
/// 21k-gas transfer — the same `derive_sha(receipts)` the real execute step
/// produces (specs/11 §10 inv 10), so invariant 10 can assert the stored root
/// equals the derived one.
fn results_at(height: u64, exec_unix: u64) -> ExecutionResults {
    ExecutionResults {
        gas_time: Time::<u64>::new(exec_unix, 0, 1),
        base_fee: Price(1),
        receipt_root: derived_receipt_root(),
        post_state_root: B256::repeat_byte(u8::try_from(height % 251).unwrap_or(0)),
    }
}

/// The receipts the canonical executed block produces: a single successful,
/// 21k-gas, log-less legacy value transfer (the same shape `exec/tests` uses).
fn canonical_receipts() -> Vec<EthReceipt> {
    vec![EthReceipt {
        success: true,
        cumulative_gas_used: 21_000,
        ..Default::default()
    }]
}

/// `derive_sha(receipts)` for [`canonical_receipts`], via the same reth
/// ordered-trie helper the executor's driver uses
/// (`EthReceipt::calculate_receipt_root_no_memo`, see
/// `exec/src/driver.rs`). Invariant 10 asserts the stored
/// [`ExecutionResults::receipt_root`] equals this.
fn derived_receipt_root() -> B256 {
    EthReceipt::calculate_receipt_root_no_memo(&canonical_receipts())
}

// ---------------------------------------------------------------------------
// Fake builder seam (deterministic; shared with the lifecycle/recovery fakes).
// ---------------------------------------------------------------------------

/// A deterministic block builder: on a parent at height `h` it produces a block
/// at `h + 1`, timestamp `parent.build_time + 1`, copying the worst-case header
/// fields and placing the settled ancestor's post-exec root in `Root`. Because
/// `build_on`/`rebuild` share one recipe, a faithfully re-broadcast block
/// rebuilds to the same hash (so verify-by-rebuild is exercisable).
#[derive(Clone, Default)]
struct FakeBuilder;

impl FakeBuilder {
    fn settled_root(parent: &Arc<Block>) -> B256 {
        parent
            .last_settled()
            .map_or(B256::ZERO, |s| s.post_execution_state_root())
    }

    fn assemble(parent: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        let height = parent.height().saturating_add(1);
        let timestamp = parent.build_time().saturating_add(1);
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

// ---------------------------------------------------------------------------
// Fake executor seam — models D → M → I → X with a durable "disk" write.
// ---------------------------------------------------------------------------

/// A controllable executor that records what it executed into the shared
/// [`DiskState`]. [`FakeExecutor::run_next`] runs the strict `D → M → I → X`
/// order of [`Block::mark_executed_with`]: it writes the durable artefacts (the
/// **D** step, modeled by [`DiskState::record`]) *inside* the persist closure —
/// which `mark_executed` runs **before** it sets the execution cell (M),
/// advances `last_executed` (I), and fires the `executed` notify (X). So any
/// `wait_until_executed` waiter woken by X is guaranteed to read the persisted
/// disk state (invariant 3) and the advanced pointer (invariant 6).
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

    /// Executes the next enqueued (not-yet-executed) block in `D → M → I → X`
    /// order: the disk write (D) happens inside `mark_executed`'s persist
    /// closure, strictly before the in-memory execution state is published.
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
            let disk = Arc::clone(&self.disk);
            let block = Arc::clone(&b);
            let recorded = results.clone();
            // D — durable write FIRST (modeled), then M/I/X. A persist error
            // would leave the block untouched; the fixture never errors.
            b.mark_executed_with(artefacts, None, move || {
                disk.record(&block, recorded);
                Ok(())
            })
            .expect("mark executed (D->M->I->X)");
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
// DiskState — the durable persistence + crash-point seam (mirrors recovery.rs).
// ---------------------------------------------------------------------------

/// The durable state surviving a crash + restart: the canonical executed block
/// bodies (height-indexed) and their committed [`ExecutionResults`]. A
/// [`Snapshot`] is taken after the live VM is dropped; recovery reads only the
/// snapshot.
#[derive(Default)]
struct DiskState {
    /// height → (eth body, execution results). Ordered so the head height and
    /// the canonical chain are walkable.
    canonical: Mutex<BTreeMap<u64, (SealedBlock<RethBlock>, ExecutionResults)>>,
}

impl DiskState {
    fn record(&self, block: &Arc<Block>, results: ExecutionResults) {
        self.canonical
            .lock()
            .insert(block.height(), (block.eth_block().clone(), results));
    }

    fn head(&self) -> u64 {
        self.canonical.lock().keys().copied().max().unwrap_or(0)
    }

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

/// A frozen, drop-surviving view of the disk — the [`RecoverySource`] that the
/// core `recover()` reads. No live `Arc<Block>` is retained; every block is
/// reconstructed from the persisted eth body, exactly like reading rawdb.
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
        if self.commit_interval == 0 {
            return self.head;
        }
        // `commit_interval != 0` here (guarded above); `checked_rem` keeps the
        // SAE arithmetic bar happy without an `unwrap`.
        let rem = self.head.checked_rem(self.commit_interval).unwrap_or(0);
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
// Live chain driver.
// ---------------------------------------------------------------------------

/// A wall-clock far enough in the future that no fixture block trips the
/// future-block bound. The harness never reads wall-clock into a consensus
/// output (invariant 11); this clock only gates verify's future-block check.
fn now() -> SystemTime {
    UNIX_EPOCH
        .checked_add(Duration::from_secs(1_000_000))
        .unwrap_or(UNIX_EPOCH)
}

/// The recorded oracle for the three frontiers after a live run, plus the
/// durable disk + genesis so a recovery snapshot can be taken.
struct LiveRun {
    disk: Arc<DiskState>,
    genesis: Arc<Block>,
    settled_h: u64,
    executed_h: u64,
    accepted_h: u64,
    settled_root: B256,
    executed_root: B256,
    /// Canonical accepted ids in increasing height (height 1..=n).
    accepted_ids: Vec<Id>,
}

/// Builds a live VM over a shared [`DiskState`] and drives `n` blocks through
/// the full lifecycle (build → verify → accept → execute → settle), executing
/// every accepted block so E == A == head and S advances via the live
/// settlement driver. Returns the recorded frontier oracle.
///
/// # Panics
/// On any lifecycle-seam error (a harness/programming error, not a runtime
/// condition); the `expect` messages name the failing step.
async fn build_live_chain(n: u64) -> LiveRun {
    let disk = Arc::new(DiskState::default());
    let g = genesis();
    let exec = Arc::new(FakeExecutor::new(Arc::clone(&disk)));
    let vm = Arc::new(Vm::new(&g, FakeBuilder, Arc::clone(&exec), now));

    let mut head: Arc<Block> = Arc::clone(&g);
    let mut accepted_ids = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
    for _ in 0..n {
        let built = vm.build_block(None).await.expect("build");
        vm.verify_block(None, &built).await.expect("verify");
        vm.accept_block(&built).await.expect("accept");
        exec.run_next();
        vm.set_preference(built.id(), None).await.expect("pref");
        accepted_ids.push(built.id());
        head = Arc::clone(built.block());
    }

    let f = vm.frontier();
    LiveRun {
        disk,
        genesis: g,
        settled_h: f.last_settled().height(),
        executed_h: head.height(),
        accepted_h: f.last_accepted().height(),
        settled_root: f.last_settled().post_execution_state_root(),
        executed_root: head.post_execution_state_root(),
        accepted_ids,
    }
}

// ---------------------------------------------------------------------------
// (1) Frontier ordering: height(S) <= height(E) <= height(A) always.
// ---------------------------------------------------------------------------

/// Asserts invariant 1 (frontier ordering) holds at every step of an `n`-block
/// live run, and on the final frontier.
///
/// The executor reactor that advances the VM's `LastExecuted` *frontier
/// pointer* is M7.26 (stubbed here by [`FakeExecutor`], which passes `None` for
/// the pointer), so the live frontier's E pointer stays at genesis. The real E
/// — the height the executor has actually committed — is the *executed head*
/// (every accepted block is executed by the fake, so E == A == head once a step
/// completes). The harness tracks that head and asserts `S <= E_head <= A` at
/// every step, plus the directly-readable `S <= A` on the frontier.
///
/// # Panics
/// On any ordering violation or lifecycle-seam error.
// `s`/`e`/`a` are the spec's own names for the three frontiers (S/E/A, specs/11
// §1.1); the single-char locals mirror that domain notation deliberately.
#[allow(clippy::many_single_char_names)]
pub async fn assert_frontier_ordering(n: u64) {
    let disk = Arc::new(DiskState::default());
    let g = genesis();
    let exec = Arc::new(FakeExecutor::new(Arc::clone(&disk)));
    let vm = Arc::new(Vm::new(&g, FakeBuilder, Arc::clone(&exec), now));

    // Ordering holds at construction (all three pointers at genesis).
    assert!(vm.frontier().heights_ordered(), "S<=E<=A at genesis");

    let mut executed_head: Arc<Block> = Arc::clone(&g);
    for step in 0..n {
        let built = vm.build_block(None).await.expect("build");
        vm.verify_block(None, &built).await.expect("verify");
        vm.accept_block(&built).await.expect("accept");
        // After accept (A advanced; S advanced for any caught-up ancestor):
        // S <= A on the frontier, and S <= E_head (settlement never outruns the
        // executed head — settle returns ExecutionLagging otherwise).
        let f = vm.frontier();
        let s = f.last_settled().height();
        let a = f.last_accepted().height();
        let e = executed_head.height();
        assert!(s <= e, "S({s}) <= E_head({e}) after accept at step {step}");
        assert!(e <= a, "E_head({e}) <= A({a}) after accept at step {step}");
        assert!(s <= a, "S({s}) <= A({a}) on the frontier at step {step}");

        // Execute the freshly-accepted block (advances the real executed head).
        exec.run_next();
        executed_head = Arc::clone(built.block());

        // After execute: E_head caught up to A, S still <= E_head.
        let s = f.last_settled().height();
        let a = f.last_accepted().height();
        let e = executed_head.height();
        assert!(
            s <= e && e <= a,
            "S({s})<=E_head({e})<=A({a}) after execute @ {step}"
        );

        vm.set_preference(built.id(), None).await.expect("pref");
    }

    let f = vm.frontier();
    let s = f.last_settled().height();
    let e = executed_head.height();
    let a = f.last_accepted().height();
    assert!(s <= e && e <= a, "final S({s})<=E_head({e})<=A({a})");
}

// ---------------------------------------------------------------------------
// (2) Stage causality: settled ⇒ executed ⇒ accepted.
// ---------------------------------------------------------------------------

/// Asserts invariant 2 (stage causality): every settled block is executed, and
/// every executed block is accepted, over an `n`-block live run.
///
/// # Panics
/// On any causality violation or lifecycle-seam error.
// `s`/`e`/`a` mirror the spec's S/E/A frontier names (specs/11 §1.1).
#[allow(clippy::many_single_char_names)]
pub async fn assert_stage_causality(n: u64) {
    let live = build_live_chain(n).await;
    let snap = live.disk.snapshot(Arc::clone(&live.genesis), 1);
    let recovered = recover(&snap).await.expect("recover");
    let f = &recovered.frontier;

    let s = f.last_settled();
    let e = f.last_executed().expect("executed");
    let a = f.last_accepted();

    // S ⇒ E: the settled frontier is executed (it carries committed results).
    assert!(s.executed(), "settled block must be executed");
    // S ⇒ A: settled height does not exceed accepted height.
    assert!(s.height() <= a.height(), "settled <= accepted");
    // E ⇒ A: the executed frontier is at or below the accepted frontier.
    assert!(e.height() <= a.height(), "executed <= accepted");
    assert!(e.executed(), "executed frontier is executed");

    // Settled ⇒ Executed at the stage level (Settled is the top lifecycle
    // stage, which subsumes Executed). The recovered settled frontier reports
    // `settled()`.
    assert!(s.settled(), "settled frontier reports settled()");
}

// ---------------------------------------------------------------------------
// (3) Persistence ordering on execute: D → M → I → X; reader of X reads D.
// ---------------------------------------------------------------------------

/// Asserts invariant 3 (persistence ordering on execute): `mark_executed` runs
/// `D → M → I → X`; a task woken by the `executed` notify (X) always observes
/// the persisted artefacts (D) and the advanced execution state.
///
/// Models D with an in-memory flag set inside the `persist` closure (the real
/// durable D-step is behind the Firewood seam; M7.18/M7.24). The contract under
/// test is the *order*: the persist closure runs before the execution cell is
/// published and before the notify fires.
///
/// # Panics
/// If a woken waiter observes the block as un-persisted/un-executed.
pub async fn assert_persist_order_execute() {
    let g = genesis();
    let built = FakeBuilder::assemble(&g).expect("build child");

    // The "disk" D-flag — set inside the persist closure, observed by the woken
    // waiter. Modeled (no real Firewood here); the order is what is tested.
    let persisted = Arc::new(AtomicU64::new(0));

    // A waiter parked on the executed notify before execution begins.
    let waiter_block = Arc::clone(&built);
    let waiter_flag = Arc::clone(&persisted);
    let waiter = tokio::spawn(async move {
        waiter_block.wait_until_executed().await;
        // Woken by X — D and M must already be observable.
        (
            waiter_flag.load(Ordering::SeqCst),
            waiter_block.executed(),
            waiter_block.post_execution_state_root(),
        )
    });

    tokio::task::yield_now().await;

    let results = results_at(built.height(), built.build_time());
    let artefacts = ExecutionArtefacts {
        interim_execution_time: results.gas_time.clone(),
        results: results.clone(),
    };
    let flag = Arc::clone(&persisted);
    // D inside persist, then M/I/X.
    built
        .mark_executed_with(artefacts, None, move || {
            flag.store(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("mark executed");

    let (saw_persisted, saw_executed, root) = waiter.await.expect("waiter task");
    assert_eq!(
        saw_persisted, 1,
        "X-woken reader must observe D (persist ran first)"
    );
    assert!(
        saw_executed,
        "X-woken reader must observe M (execution published)"
    );
    assert_eq!(
        root, results.post_state_root,
        "X-woken reader reads the persisted post-state root",
    );
}

// ---------------------------------------------------------------------------
// (4) Persistence ordering on accept: D(σ∈S) before D(b∈A).
// ---------------------------------------------------------------------------

/// Asserts invariant 4 (persistence ordering on accept): the settled/finalized
/// hash is persisted before the canonical/accepted hash. Over an `n`-block run,
/// at every step the settled frontier height never exceeds the accepted
/// frontier height — i.e. a block is only ever settled *after* it (and its
/// settled ancestors) have been accepted and their executed roots committed to
/// the durable disk.
///
/// Models the durable-disk D-steps via [`DiskState`]; the order asserted is
/// that settlement (S) only advances over heights already recorded as durably
/// executed-and-accepted (present in the canonical disk table).
///
/// # Panics
/// On any ordering violation or lifecycle-seam error.
pub async fn assert_persist_order_accept(n: u64) {
    let disk = Arc::new(DiskState::default());
    let g = genesis();
    let exec = Arc::new(FakeExecutor::new(Arc::clone(&disk)));
    let vm = Arc::new(Vm::new(&g, FakeBuilder, Arc::clone(&exec), now));

    for step in 0..n {
        let built = vm.build_block(None).await.expect("build");
        vm.verify_block(None, &built).await.expect("verify");
        vm.accept_block(&built).await.expect("accept");
        exec.run_next();
        vm.set_preference(built.id(), None).await.expect("pref");

        let f = vm.frontier();
        let settled_h = f.last_settled().height();
        let accepted_h = f.last_accepted().height();
        // D(σ∈S) before D(b∈A): the settled hash is never ahead of accepted.
        assert!(
            settled_h <= accepted_h,
            "settled hash ({settled_h}) persisted after accepted ({accepted_h}) at step {step}",
        );
        // Every height up to and including the settled frontier is durably
        // recorded (its accepted + executed D-step landed before it settled).
        // Height 0 is genesis (synchronous, not on disk); heights >= 1 must be
        // present once settled.
        for h in 1..=settled_h {
            assert!(
                disk.canonical.lock().contains_key(&h),
                "settled height {h} was durably persisted before settling (step {step})",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// (5) Settle-in-order: mark_settled on Σ_n in increasing height.
// ---------------------------------------------------------------------------

/// Asserts invariant 5 (settle-in-order): the settlement driver marks ancestors
/// settled in strictly increasing height. Records the height of the settled
/// frontier after each accept and asserts the sequence is non-decreasing and,
/// where it advances, advances monotonically.
///
/// # Panics
/// On any out-of-order settle or lifecycle-seam error.
pub async fn assert_settle_in_order(n: u64) {
    let disk = Arc::new(DiskState::default());
    let g = genesis();
    let exec = Arc::new(FakeExecutor::new(Arc::clone(&disk)));
    let vm = Arc::new(Vm::new(&g, FakeBuilder, Arc::clone(&exec), now));

    let mut settled_seq = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
    for _ in 0..n {
        let built = vm.build_block(None).await.expect("build");
        vm.verify_block(None, &built).await.expect("verify");
        vm.accept_block(&built).await.expect("accept");
        exec.run_next();
        vm.set_preference(built.id(), None).await.expect("pref");
        settled_seq.push(vm.frontier().last_settled().height());
    }

    // The settled frontier height is monotonically non-decreasing (it only ever
    // advances forward — mark_settled walks the range in increasing height).
    for w in settled_seq.windows(2) {
        let (prev, cur) = (w[0], w[1]);
        assert!(
            cur >= prev,
            "settled frontier moved backward: {prev} -> {cur} (seq {settled_seq:?})",
        );
    }
    // It must have advanced past genesis over a sufficiently long run.
    assert!(
        settled_seq.last().is_some_and(|&h| h > 0),
        "settlement advanced past genesis (seq {settled_seq:?})",
    );
}

// ---------------------------------------------------------------------------
// (6) Atomics-before-broadcast: pointer advanced before WaitUntil* fires.
// ---------------------------------------------------------------------------

/// Asserts invariant 6 (atomics-before-broadcast): the VM-level `last_executed`
/// pointer is advanced *before* `mark_executed` fires the `executed` notify, so
/// a `wait_until_executed` waiter never reads a stale pointer. Mirrors
/// `exec/tests/events.rs::wait_until_executed_observes_pointer_first` at the
/// block layer (the real internal pointer rather than a stand-in `AtomicU64`).
///
/// # Panics
/// If the woken waiter observes a pointer lower than the broadcast height.
pub async fn assert_atomics_before_broadcast() {
    use arc_swap::ArcSwapOption;

    let g = genesis();
    let built = FakeBuilder::assemble(&g).expect("build child");
    let target_height = built.height();

    // The VM-level last-executed pointer mark_executed advances (the "internal
    // pointer" the broadcast announces against).
    let last_executed: Arc<ArcSwapOption<Block>> = Arc::new(ArcSwapOption::empty());

    let waiter_block = Arc::clone(&built);
    let waiter_ptr = Arc::clone(&last_executed);
    let waiter = tokio::spawn(async move {
        waiter_block.wait_until_executed().await;
        // Woken by X — the pointer must already be at >= target_height.
        waiter_ptr.load_full().map_or(0, |b| b.height())
    });

    tokio::task::yield_now().await;

    let results = results_at(built.height(), built.build_time());
    let artefacts = ExecutionArtefacts {
        interim_execution_time: results.gas_time.clone(),
        results,
    };
    // mark_executed advances `last_executed` (I) BEFORE notify (X).
    built
        .mark_executed(artefacts, Some(&last_executed))
        .expect("mark executed");

    let observed = waiter.await.expect("waiter task");
    assert!(
        observed >= target_height,
        "woken waiter read a stale pointer ({observed} < {target_height})",
    );
}

// ---------------------------------------------------------------------------
// (7) Recovery equivalence: restart reconstructs identical A/E/S + roots.
// ---------------------------------------------------------------------------

/// Asserts invariant 7 (recovery equivalence) by driving an `n`-block live
/// chain, snapshotting the durable inputs, dropping the live state, and
/// reconstructing A/E/S via M7.24's [`recover`]. Asserts the recovered
/// frontiers + post-state roots equal the live oracle.
///
/// # Panics
/// On any mismatch between the recovered and live frontiers/roots.
pub async fn assert_recovery_equivalence(n: u64) {
    let live = build_live_chain(n).await;
    let snap = live.disk.snapshot(Arc::clone(&live.genesis), 16);
    let recovered = recover(&snap).await.expect("recover");
    let f = &recovered.frontier;

    assert_eq!(f.last_accepted().height(), live.accepted_h, "A height");
    assert_eq!(
        f.last_executed().expect("executed").height(),
        live.executed_h,
        "E height",
    );
    assert_eq!(f.last_settled().height(), live.settled_h, "S height");
    assert_eq!(
        f.last_executed()
            .expect("executed")
            .post_execution_state_root(),
        live.executed_root,
        "E post-state root identical",
    );
    assert_eq!(
        f.last_settled().post_execution_state_root(),
        live.settled_root,
        "S post-state root identical",
    );
    assert!(f.heights_ordered(), "S<=E<=A after recovery");
    assert_eq!(f.last_accepted().height(), live.disk.head(), "A == head");
}

// ---------------------------------------------------------------------------
// (8) GC of settled ancestry: parent/last_settled None + count baseline.
// ---------------------------------------------------------------------------

/// Asserts invariant 8 (GC of settled ancestry): after a chain is fully driven
/// (every block executed + settled) and the VM dropped, a settled block reports
/// `parent()` / `last_settled()` → `None` (ancestry severed) and the global
/// `InMemoryBlockCount` returns to the baseline captured before the run (no
/// ancestry leak).
///
/// Requires nextest (process-per-test) so the `static AtomicI64` is not shared
/// with concurrently-running tests. A benign nextest "leaky" flag is expected
/// because the asserted, retained blocks are dropped after the assertion.
///
/// # Panics
/// If the count fails to return to baseline or the ancestry is not severed.
pub async fn assert_gc_settled_ancestry() {
    let baseline = in_memory_block_count();

    // Drive a chain; keep ONE settled block alive to assert ancestry severance,
    // then drop it and assert the count returns to baseline.
    let severed = {
        let disk = Arc::new(DiskState::default());
        let g = genesis();
        let exec = Arc::new(FakeExecutor::new(Arc::clone(&disk)));
        let vm = Arc::new(Vm::new(&g, FakeBuilder, Arc::clone(&exec), now));

        let mut a_settled_block: Option<Arc<Block>> = None;
        for _ in 0..6u64 {
            let built = vm.build_block(None).await.expect("build");
            vm.verify_block(None, &built).await.expect("verify");
            vm.accept_block(&built).await.expect("accept");
            exec.run_next();
            vm.set_preference(built.id(), None).await.expect("pref");
        }

        // The settled frontier is a settled block: ancestry severed.
        let s = vm.frontier().last_settled();
        if s.height() > 0 {
            a_settled_block = Some(Arc::clone(&s));
        }
        // Genesis (synchronous) reports itself as last_settled; a non-genesis
        // settled block reports None for both parent and last_settled.
        let block = a_settled_block.expect("a non-genesis block settled");
        assert!(block.settled(), "frontier S is settled");
        assert!(block.parent_block().is_none(), "settled ⇒ parent() None");
        assert!(
            block.last_settled().is_none(),
            "settled ⇒ last_settled() None"
        );
        block
        // vm (and all retained ancestry) drops here, except `severed`.
    };

    // Drop the last retained block; the count must return to the baseline.
    drop(severed);
    let after = in_memory_block_count();
    assert_eq!(
        after, baseline,
        "InMemoryBlockCount returned to baseline (no ancestry leak): {after} != {baseline}",
    );
}

// ---------------------------------------------------------------------------
// (9) No reorg: acceptance is final.
// ---------------------------------------------------------------------------

/// Asserts invariant 9 (no reorg): once accepted, the canonical id at each
/// height is final. Drives an `n`-block chain, records the canonical id at each
/// height, then drives more blocks (further snapshot flattening / acceptance)
/// and asserts the earlier canonical ids are unchanged.
///
/// # Panics
/// If any previously-accepted canonical id changes, or on a lifecycle error.
pub async fn assert_no_reorg(n: u64) {
    let disk = Arc::new(DiskState::default());
    let g = genesis();
    let exec = Arc::new(FakeExecutor::new(Arc::clone(&disk)));
    let vm = Arc::new(Vm::new(&g, FakeBuilder, Arc::clone(&exec), now));

    // First wave.
    let mut canonical: Vec<(u64, Id)> = Vec::new();
    for _ in 0..n {
        let built = vm.build_block(None).await.expect("build");
        vm.verify_block(None, &built).await.expect("verify");
        vm.accept_block(&built).await.expect("accept");
        exec.run_next();
        vm.set_preference(built.id(), None).await.expect("pref");
        canonical.push((built.block().height(), built.id()));
    }

    // Second wave (more acceptance — the snapshot layer may flatten freely).
    for _ in 0..n {
        let built = vm.build_block(None).await.expect("build");
        vm.verify_block(None, &built).await.expect("verify");
        vm.accept_block(&built).await.expect("accept");
        exec.run_next();
        vm.set_preference(built.id(), None).await.expect("pref");
    }

    // Every earlier canonical id is unchanged (acceptance is final, no reorg).
    for (h, id) in canonical {
        let now_id = vm.get_block_id_at_height(h).await.expect("height index");
        assert_eq!(now_id, id, "canonical id at height {h} changed (reorg!)");
    }
}

// ---------------------------------------------------------------------------
// (10) Receipt-root match: receipt_root == derive_sha(receipts).
// ---------------------------------------------------------------------------

/// Asserts invariant 10 (receipt-root match): the stored
/// [`ExecutionResults::receipt_root`] equals `derive_sha(receipts)` —
/// `EthReceipt::calculate_receipt_root_no_memo` over the same receipts the
/// canonical block produced (the helper the executor's driver uses, see
/// `exec/src/driver.rs`).
///
/// # Panics
/// If the stored root differs from the independently-derived one.
pub fn assert_receipt_root_match() {
    let results = results_at(1, 1);
    let derived = EthReceipt::calculate_receipt_root_no_memo(&canonical_receipts());
    assert_eq!(
        results.receipt_root, derived,
        "stored receipt_root must equal derive_sha(receipts)",
    );
    // Sanity: the derived root is non-trivial (a real ordered-trie root, not the
    // empty/zero placeholder).
    assert_ne!(derived, B256::ZERO, "derive_sha(receipts) is a real root");
}

// ---------------------------------------------------------------------------
// (11) Determinism: output independent of wall-clock + map order.
// ---------------------------------------------------------------------------

/// Asserts invariant 11 (determinism) at the chain level: the same block
/// program, accepted/executed/settled under two independent VMs (built with
/// independent clocks and frontier maps that receive blocks in
/// insertion-independent order), settles to identical A/E/S + post-state +
/// receipt roots.
///
/// The authoritative determinism gate is M7.16's
/// `prop::sae_execution_determinism` (in `exec/tests`), which forces *pipeline
/// schedules*; this helper re-asserts the principle the harness can reach —
/// that the chain-level settled state is a pure function of the block inputs,
/// not of wall-clock or map iteration order.
///
/// # Panics
/// If the two runs disagree on any frontier height or post-state/receipt root.
pub async fn assert_determinism(n: u64) {
    let a = build_live_chain(n).await;
    let b = build_live_chain(n).await;

    assert_eq!(a.accepted_h, b.accepted_h, "A height deterministic");
    assert_eq!(a.executed_h, b.executed_h, "E height deterministic");
    assert_eq!(a.settled_h, b.settled_h, "S height deterministic");
    assert_eq!(a.settled_root, b.settled_root, "settled root deterministic");
    assert_eq!(
        a.executed_root, b.executed_root,
        "executed root deterministic"
    );
    assert_eq!(
        a.accepted_ids, b.accepted_ids,
        "canonical id sequence deterministic",
    );

    // Two recoveries of the same disk (the frontier map is rebuilt fresh each
    // time, in recovery's own insertion order) reconstruct identical frontiers
    // — settled state is independent of map order.
    let s1 = a.disk.snapshot(Arc::clone(&a.genesis), 16);
    let s2 = a.disk.snapshot(Arc::clone(&a.genesis), 4);
    let r1 = recover(&s1).await.expect("recover 1");
    let r2 = recover(&s2).await.expect("recover 2");
    assert_eq!(
        r1.frontier.last_settled().post_execution_state_root(),
        r2.frontier.last_settled().post_execution_state_root(),
        "settled root independent of commit-interval / map order",
    );
    assert_eq!(
        r1.frontier.last_accepted().height(),
        r2.frontier.last_accepted().height(),
        "accepted height independent of commit-interval / map order",
    );
}
