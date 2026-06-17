// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The single-task streaming [`Executor`] skeleton (specs/11 §6.1).
//!
//! [`Executor`] holds the state one execute step needs: the VM-level
//! `last_executed` pointer, the parent gas clock (cached across blocks for exact
//! continuity), the [`EvmDriver`] reuse seam, the [`ExecHooks`], the saedb
//! [`Tracker`], and a minimal [`ReceiptSink`]. Its synchronous
//! [`Executor::execute_one`] drives [`execute_step`](crate::execute_step) for one
//! block and publishes the receipts.
//!
//! # Async reactor (M7.15)
//!
//! The executor now carries the async-notification layer (specs/11 §6, §1.5):
//! an [`Eventual<TxReceipt>`] receipt buffer keyed by tx hash, a [`HeadEvents`]
//! chain-head broadcast, the [`ExecutionWaiters`] `WaitUntil{Executed,Settled}`
//! signals, and a [`CancellationToken`] + [`TaskTracker`] for graceful
//! shutdown. After [`Executor::execute_one`] commits a block it resolves the
//! per-tx receipt eventuals, advances the executed-frontier height, then emits a
//! [`ChainHeadEvent`] — strictly **after** advancing `last_executed`
//! (invariant 6, specs/11 §10).
//!
//! # Deferred to M7.26
//!
//! The bounded `mpsc` queue + the spawned `processQueue` task *loop* (the
//! backpressure path) is M7.26 — see the `// M7.26` markers. This task delivers
//! the notification/shutdown primitives wired into the synchronous step.

use std::sync::Arc;
use std::time::Instant;

use arc_swap::{ArcSwap, ArcSwapOption};
use ava_saevm_blocks::{Block, WorstCaseBounds};
use ava_saevm_db::Tracker;
use ava_saevm_gastime::GasTime;
use parking_lot::Mutex;
use std::collections::HashMap;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::driver::{EvmDriver, ExecHooks, TxReceipt};
use crate::error::{Error, Result};
use crate::events::{ChainHeadEvent, ExecutionWaiters, HeadEvents};
use crate::eventual::Eventual;
use crate::execute_step::{StepOutput, execute_step};
use crate::metrics::SaexecMetrics;

/// A shared, late-bindable slot for the `saexec` execution-pressure metrics.
///
/// The [`Executor`] is constructed before any prometheus registry exists (the
/// registry is handed in at node assembly, M8), so the metrics live behind an
/// [`ArcSwapOption`]: empty until [`Executor::set_metrics`] wires one, after
/// which the queue/execute event sites update it. All updates are cheap no-ops
/// while the slot is empty.
type MetricsSlot = ArcSwapOption<SaexecMetrics>;

/// A minimal receipt sink: receipts produced by the execute step are appended
/// here in execution order (specs/11 §6.1 step 6).
///
/// The full `Eventual<Receipt>` buffer keyed by tx hash (so a caller can await a
/// specific tx's receipt before its block executes) lands with the async reactor
/// in M7.15. For the M7.14 synchronous step this is a simple appended log the
/// tests assert on.
#[derive(Default)]
pub struct ReceiptSink {
    receipts: Mutex<Vec<TxReceipt>>,
}

impl ReceiptSink {
    /// Constructs an empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a block's receipts in execution order.
    pub fn publish(&self, receipts: &[TxReceipt]) {
        self.receipts.lock().extend_from_slice(receipts);
    }

    /// A snapshot of every receipt published so far.
    #[must_use]
    pub fn snapshot(&self) -> Vec<TxReceipt> {
        self.receipts.lock().clone()
    }
}

/// One item queued for execution: an ordered block, its parent's committed
/// post-execution state root, the builder's worst-case prediction, and the
/// [`Instant`] the block was enqueued (for the `execution_queue_duration_seconds`
/// histogram; specs/11 §6.2, specs/18 §2.11). The FIFO order of these on the
/// bounded channel is the total execution order — there is no parallel block
/// execution.
type QueueItem = (Arc<Block>, ava_saevm_types::B256, WorstCaseBounds, Instant);

/// A cloneable handle to the executor's bounded execution queue (specs/11 §6.2).
///
/// Holds the [`mpsc::Sender`] side of the bounded channel feeding the single
/// `processQueue` drain task. [`enqueue`](Self::enqueue) is the AcceptBlock-side
/// push: because the channel is **bounded**, `enqueue` parks (`send().await`)
/// once the channel is full, pacing consensus to the execution thread (`Ω_Q`,
/// specs/11 §2.4) — no unbounded queue growth.
#[derive(Clone)]
pub struct Queue {
    tx: mpsc::Sender<QueueItem>,
    /// Shared handle to the executor's metrics slot, so the acceptance-side
    /// `mark_enqueued` event is recorded here (Go `markEnqueued`).
    metrics: Arc<MetricsSlot>,
}

impl Queue {
    /// Enqueues `block` (with its `parent_root` and worst-case `bounds`) for
    /// execution, parking until a slot is free if the bounded channel is full.
    ///
    /// This is the backpressure seam: with a bounded channel, a flood of accepts
    /// blocks here rather than buffering unboundedly, so consensus paces itself
    /// to execution throughput (specs/11 §6.2).
    ///
    /// # Errors
    ///
    /// [`Error::QueueClosed`] if the executor's `processQueue` drain task has
    /// shut down (the receiver was dropped); the block was not enqueued.
    pub async fn enqueue(
        &self,
        block: Arc<Block>,
        parent_root: ava_saevm_types::B256,
        bounds: WorstCaseBounds,
    ) -> Result<()> {
        // Record the acceptance-side metric event before the block can be drained
        // (so the queue gauge's `inc` happens-before the drain loop's matching
        // `dec`); a send failure over-counts only on the rare shutdown path.
        let gas_limit = block.gas_limit();
        if let Some(metrics) = self.metrics.load_full() {
            metrics.mark_enqueued(gas_limit);
        }
        self.tx
            .send((block, parent_root, bounds, Instant::now()))
            .await
            .map_err(|_| Error::QueueClosed)
    }
}

/// The single-task streaming executor (specs/11 §6.1).
///
/// Owns the execution-thread state and drives one block at a time through
/// [`execute_step`]. The async FIFO reactor that feeds it is M7.15.
pub struct Executor<D: EvmDriver, H: ExecHooks> {
    /// The last fully-executed block (the `X`-visible pointer the next block's
    /// parent-hash check reads). `None` only before the genesis/synchronous
    /// block is installed.
    last_executed: ArcSwapOption<Block>,
    /// The gas clock of the last-executed block, cached for exact continuity
    /// (avoids the lossy `rebuild_gas_clock` recovery path on the hot path).
    parent_gas_clock: ArcSwap<GasTime>,
    /// The EVM reuse seam (production: `AvaEvmDriver`; tests: a fake).
    driver: D,
    /// The SAE block-lifecycle hooks (`NoopExecHooks` for the pure-EVM path).
    hooks: H,
    /// The saedb revision tracker (commit policy + ref-count window).
    tracker: Tracker,
    /// The per-tx receipt sink (the ordered append log; the awaitable per-tx
    /// resolution is [`receipt_eventuals`](Self::receipt_eventuals)).
    receipts: Arc<ReceiptSink>,
    /// The awaitable per-tx receipt buffer keyed by tx hash (Go
    /// `eventual.Value[*Receipt]`). A caller can [`await_receipt`](Self::await_receipt)
    /// a specific tx before its block executes; the eventual is resolved once
    /// the block commits.
    receipt_eventuals: Arc<Mutex<HashMap<ava_saevm_types::B256, Eventual<TxReceipt>>>>,
    /// The chain-head event broadcast (Go `event.FeedOf[ChainHeadEvent]`).
    head_events: HeadEvents,
    /// The `WaitUntil{Executed,Settled}` height waiters (invariant 6).
    waiters: ExecutionWaiters,
    /// Cancels the executor's spawned tasks on shutdown (Go `context.Context`).
    shutdown: CancellationToken,
    /// Tracks the executor's spawned tasks so [`shutdown`](Self::shutdown) can
    /// drain them (Go `io.Closer` reverse-order close / `sync.WaitGroup`).
    tasks: TaskTracker,
    /// Backing store for the `sae` `last_executed_height` gauge: the height of
    /// the latest block that completed async execution (set at construction +
    /// per post-execution event; specs/18 §2.11, Go `844535b313`).
    ///
    /// The honest gauge backing store, read via [`Executor::last_executed_height`].
    /// The `sae`-namespace `last_executed_height` gauge is sourced from the
    /// core `Frontier` (`ava-saevm-core`'s `SaeMetrics`); this executor-local
    /// store mirrors it for the synchronous step's own accounting.
    last_executed_height_gauge: std::sync::atomic::AtomicU64,
    /// The `saexec` execution-pressure metrics (specs/18 §2.11), late-bound via
    /// [`Executor::set_metrics`]. Empty until the node hands in a registry (M8);
    /// the queue/execute event sites then update it.
    metrics: Arc<MetricsSlot>,
}

impl<D: EvmDriver, H: ExecHooks> Executor<D, H> {
    /// Builds an executor seeded with the genesis/last-executed block and its
    /// gas clock.
    #[must_use]
    pub fn new(
        last_executed: Arc<Block>,
        parent_gas_clock: GasTime,
        driver: D,
        hooks: H,
        tracker: Tracker,
    ) -> Self {
        // The gauge starts at the seeded last-executed height (Go sets it at
        // `Executor` construction).
        let seed_height = last_executed.height();
        Self {
            last_executed: ArcSwapOption::from(Some(last_executed)),
            parent_gas_clock: ArcSwap::from_pointee(parent_gas_clock),
            driver,
            hooks,
            tracker,
            receipts: Arc::new(ReceiptSink::new()),
            receipt_eventuals: Arc::new(Mutex::new(HashMap::new())),
            head_events: HeadEvents::new(),
            waiters: ExecutionWaiters::new(),
            shutdown: CancellationToken::new(),
            tasks: TaskTracker::new(),
            last_executed_height_gauge: std::sync::atomic::AtomicU64::new(seed_height),
            metrics: Arc::new(MetricsSlot::empty()),
        }
    }

    /// Wires the `saexec` execution-pressure metrics (specs/18 §2.11). Idempotent
    /// and interior-mutable: the node calls this once it has the registry-backed
    /// [`SaexecMetrics`] (M8); until then the event sites are no-ops.
    pub fn set_metrics(&self, metrics: Arc<SaexecMetrics>) {
        self.metrics.store(Some(metrics));
    }

    /// The wired `saexec` metrics, if any.
    #[must_use]
    pub fn metrics(&self) -> Option<Arc<SaexecMetrics>> {
        self.metrics.load_full()
    }

    /// The receipt sink (shared handle).
    #[must_use]
    pub fn receipts(&self) -> &Arc<ReceiptSink> {
        &self.receipts
    }

    /// The current last-executed block, if any.
    #[must_use]
    pub fn last_executed(&self) -> Option<Arc<Block>> {
        self.last_executed.load_full()
    }

    /// The `sae` `last_executed_height` gauge value: the height of the latest
    /// block that completed async execution (set at construction + per
    /// post-execution event; specs/18 §2.11). The backing store for the
    /// prometheus gauge once metrics plumbing reaches the SAE crates (M8).
    #[must_use]
    pub fn last_executed_height(&self) -> u64 {
        self.last_executed_height_gauge
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Subscribes to the chain-head event feed (one [`ChainHeadEvent`] per
    /// executed block; specs/11 §6).
    #[must_use]
    pub fn subscribe_chain_head(&self) -> broadcast::Receiver<ChainHeadEvent> {
        self.head_events.subscribe_chain_head()
    }

    /// The `WaitUntil{Executed,Settled}` waiters (invariant 6 ordering).
    #[must_use]
    pub fn waiters(&self) -> &ExecutionWaiters {
        &self.waiters
    }

    /// The executor's [`CancellationToken`]; cancelled by [`shutdown`](Self::shutdown).
    #[must_use]
    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.shutdown
    }

    /// The [`TaskTracker`] the executor spawns its async tasks under (M7.26's
    /// `processQueue` loop registers here; see [`shutdown`](Self::shutdown)).
    #[must_use]
    pub fn task_tracker(&self) -> &TaskTracker {
        &self.tasks
    }

    /// Awaits the receipt of transaction `tx_hash`, registering an
    /// [`Eventual`] if one is not already pending. Resolves once the block
    /// containing the tx commits (specs/11 §6.1 step 6).
    pub async fn await_receipt(&self, tx_hash: ava_saevm_types::B256) -> TxReceipt {
        let eventual = self
            .receipt_eventuals
            .lock()
            .entry(tx_hash)
            .or_default()
            .clone();
        eventual.wait().await
    }

    /// Gracefully shuts the executor down (specs/11 §6.2): cancels the
    /// [`CancellationToken`] (so any spawned task observing it finishes its
    /// in-flight work), closes the [`TaskTracker`], and awaits the drain.
    ///
    /// The Firewood `tracker.close(last_root)` snapshot-flatten is a documented
    /// hook here: the synchronous [`execute_one`](Self::execute_one) commits
    /// per-block via the saedb [`Tracker`], so no separate close is required for
    /// the M7.15 path; the explicit `close` lands with the M7.26 `processQueue`
    /// loop that owns the long-lived Firewood handle.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }

    /// Synchronously executes one block against the current `last_executed`
    /// state, advancing the executor's pointers and publishing receipts on
    /// success (specs/11 §6.1).
    ///
    /// `parent_root` is the parent's committed post-execution state root;
    /// `bounds` is the builder's worst-case prediction attached to `block`
    /// before execution.
    ///
    /// # Errors
    ///
    /// Propagates any [`execute_step`] failure (parent mismatch, worst-case bound
    /// violation, fatal EVM/state error, commit/lifecycle error). The executor's
    /// pointers are only advanced on success.
    pub fn execute_one(
        &self,
        block: &Arc<Block>,
        parent_root: ava_saevm_types::B256,
        bounds: &WorstCaseBounds,
    ) -> Result<StepOutput> {
        // Wall-clock timer for the `execute_block_duration_seconds` histogram
        // (specs/18 §2.11). Observational only — never feeds consensus output.
        let started = Instant::now();

        // The parent-hash check needs the current last-executed block. The VM
        // seeds the executor with the genesis/synchronous block at init (M7.18),
        // so `None` here is a programming error, not a recoverable state — fail
        // honestly rather than fabricate a sentinel parent.
        let last_block = self.last_executed.load_full().ok_or(Error::NotSeeded)?;
        let parent_clock = self.parent_gas_clock.load_full();

        let output = execute_step(
            block,
            &last_block,
            &parent_clock,
            parent_root,
            bounds,
            &self.driver,
            &self.hooks,
            &self.tracker,
            &self.last_executed,
        )?;

        // Advance the cached parent gas clock for the next block's continuity.
        self.parent_gas_clock
            .store(Arc::new(output.gas_time.clone()));
        // Publish the block's receipts to the ordered sink.
        self.receipts.publish(&output.receipts);

        // --- Async-reactor notifications (M7.15) ---
        //
        // INVARIANT 6 (atomics-before-broadcast, specs/11 §10): `execute_step`
        // has already advanced the internal `last_executed` pointer (its `I`
        // step, inside `mark_executed`). Only AFTER that do we fan out the
        // external signals (`X`): resolve the per-tx receipt eventuals, advance
        // the executed-frontier height, then emit the chain-head event. A
        // poll-after-wake therefore always observes a `last_executed`/height
        // `>=` what any broadcast announced.

        // Resolve the awaitable per-tx receipt buffer (set-once each).
        {
            let mut buf = self.receipt_eventuals.lock();
            for receipt in &output.receipts {
                buf.entry(receipt.tx_hash).or_default().set(receipt.clone());
            }
        }

        // Advance the executed-frontier height BEFORE the chain-head broadcast.
        let height = block.height();
        self.waiters.set_executed(height);
        // Update the `sae` `last_executed_height` gauge (Go sets it per
        // `sendPostExecutionEvents`). `Relaxed`: a monitoring gauge has no
        // ordering relationship with consensus state.
        self.last_executed_height_gauge
            .store(height, std::sync::atomic::Ordering::Relaxed);

        // Emit the chain-head event (last — the external `X` signal).
        self.head_events.emit(ChainHeadEvent {
            height,
            hash: block.hash(),
        });

        // Record the execution-side `saexec` metrics (Go `markExecuted` /
        // `sendPostExecutionEvents`): cumulative charged/limit gas + per-block
        // duration. Independent of the queue path, so it is correct whether this
        // block was drained from the queue or executed directly.
        if let Some(metrics) = self.metrics.load_full() {
            metrics.mark_executed(
                block.gas_limit(),
                output.gas_consumed,
                started.elapsed().as_secs_f64(),
            );
        }

        Ok(output)
    }
}

impl<D: EvmDriver + Send + Sync + 'static, H: ExecHooks + Send + Sync + 'static> Executor<D, H> {
    /// Spawns the bounded-`mpsc` `processQueue` drain task and returns a
    /// cloneable [`Queue`] handle that feeds it (specs/11 §6.2).
    ///
    /// The channel is bounded to `capacity`, so [`Queue::enqueue`] parks once it
    /// is full — this is the backpressure that paces consensus (`AcceptBlock`) to
    /// the execution thread (`Ω_Q`, specs/11 §2.4); the queue cannot grow without
    /// bound.
    ///
    /// The drain task is a single FIFO loop (no parallel block execution): it
    /// pulls `(block, parent_root, bounds)` off the receiver and drives the
    /// synchronous [`execute_one`](Self::execute_one). The total execution order
    /// is exactly the enqueue order. The task is registered under
    /// [`task_tracker`](Self::task_tracker) and exits on either channel-close or
    /// [`shutdown_token`](Self::shutdown_token) cancellation, so
    /// [`shutdown`](Self::shutdown) drains it cleanly.
    ///
    /// A recoverable per-block error is logged and the loop continues; a fatal
    /// error (`Error::is_fatal`) stops the loop (Go `errFatal` terminates the
    /// executor thread, specs/11 §11).
    #[must_use]
    pub fn start_process_queue(self: Arc<Self>, capacity: usize) -> Queue {
        let (tx, mut rx) = mpsc::channel::<QueueItem>(capacity);
        let token = self.shutdown.clone();
        let executor = Arc::clone(&self);

        self.tasks.spawn(async move {
            loop {
                tokio::select! {
                    // Cancellation wins: stop draining promptly on shutdown.
                    () = token.cancelled() => break,
                    item = rx.recv() => {
                        let Some((block, parent_root, bounds, enqueued)) = item else {
                            // Channel closed: every `Queue` sender dropped.
                            break;
                        };
                        let result = executor.execute_one(&block, parent_root, &bounds);
                        // The block has now left the queue (whatever the outcome):
                        // lower the queue gauges and observe its queue residence
                        // (Go decrements the queue depth once execution completes).
                        if let Some(metrics) = executor.metrics.load_full() {
                            metrics.mark_dequeued(
                                block.gas_limit(),
                                enqueued.elapsed().as_secs_f64(),
                            );
                        }
                        match result {
                            Ok(_) => {}
                            Err(e) if e.is_fatal() => {
                                tracing::error!(
                                    error = %e,
                                    height = block.height(),
                                    "fatal execution error; stopping processQueue loop",
                                );
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    height = block.height(),
                                    "recoverable execution error; continuing processQueue loop",
                                );
                            }
                        }
                    }
                }
            }
        });

        Queue {
            tx,
            metrics: Arc::clone(&self.metrics),
        }
    }
}
