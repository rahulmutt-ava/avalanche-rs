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
//! # Deferred to M7.15 (the async reactor)
//!
//! The bounded `mpsc` queue + `processQueue` task loop, the `Eventual<Receipt>`
//! receipt buffer keyed by tx hash, the `ChainHead` / `WaitUntil*` event
//! plumbing, and the `CancellationToken` / `JoinHandle` / `TaskTracker` graceful
//! shutdown are all M7.15 — see the `// TODO(M7.15)` markers. This task delivers
//! the synchronous step + the fields the reactor will hang off of.

use std::sync::Arc;

use arc_swap::{ArcSwap, ArcSwapOption};
use ava_saevm_blocks::{Block, WorstCaseBounds};
use ava_saevm_db::Tracker;
use ava_saevm_gastime::GasTime;
use parking_lot::Mutex;

use crate::driver::{EvmDriver, ExecHooks, TxReceipt};
use crate::error::{Error, Result};
use crate::execute_step::{StepOutput, execute_step};

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
    /// The per-tx receipt sink.
    receipts: Arc<ReceiptSink>,
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
        Self {
            last_executed: ArcSwapOption::from(Some(last_executed)),
            parent_gas_clock: ArcSwap::from_pointee(parent_gas_clock),
            driver,
            hooks,
            tracker,
            receipts: Arc::new(ReceiptSink::new()),
        }
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
        // Publish the block's receipts to the sink.
        self.receipts.publish(&output.receipts);

        // TODO(M7.15): the async reactor — bounded mpsc queue + processQueue task
        // loop, Eventual<Receipt> buffer keyed by tx hash, ChainHead /
        // WaitUntil* events, CancellationToken / JoinHandle / TaskTracker drain.

        Ok(output)
    }
}
