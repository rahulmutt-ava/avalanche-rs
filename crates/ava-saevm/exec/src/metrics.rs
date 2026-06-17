// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `saexec`-namespace prometheus metrics — the SAE execution-pressure
//! family (specs/18 §2.11; Go `saexec/metrics.go`, `553742045d` → `72adc639e6`).
//!
//! These meter the depth and throughput of the single-task streaming execution
//! queue. Go registers them via `metrics.MakeAndRegister(snowCtx.Metrics,
//! "saexec")`; the Rust VM-facing registry seam is the M8 node-assembly
//! integration. This module is the SAE half: a [`SaexecMetrics`] handle the
//! [`crate::Executor`] updates at the queue/execute event sites
//! ([`mark_enqueued`](SaexecMetrics::mark_enqueued) /
//! [`mark_executed`](SaexecMetrics::mark_executed) /
//! [`mark_dequeued`](SaexecMetrics::mark_dequeued)) and which
//! [`register_into`](SaexecMetrics::register_into) registers into a
//! caller-supplied [`Registry`].
//!
//! | Metric | Type | Updated on |
//! |---|---|---|
//! | `execution_queue_blocks` | G | enqueue (+1) / dequeue (−1) |
//! | `execution_queue_gas_limit` | G | enqueue (+limit) / dequeue (−limit) |
//! | `accepted_gas_limit_total` | C | enqueue (+limit) |
//! | `executed_gas_charged_total` | C | execute (+charged gas) |
//! | `executed_gas_limit_total` | C | execute (+limit) |
//! | `execution_queue_duration_seconds` | H | dequeue (enqueue→execute-complete) |
//! | `execute_block_duration_seconds` | H | execute (wall-clock of one block) |
//!
//! "Gas limit" is the block's eth gas limit ([`ava_saevm_blocks::Block::gas_limit`],
//! the worst-case gas); "charged gas" is [`crate::StepOutput::gas_consumed`]
//! (tx charged gas + end-of-block op gas, **not** the eth gas used). The bare
//! metric names carry no `saexec_` prefix — the namespace is applied by the
//! node's prefix gatherer at registration (specs/18 §1).

use prometheus::{
    Histogram, HistogramOpts, IntCounter, IntGauge, Opts, Registry, exponential_buckets,
};

/// The `saexec`-namespace execution-pressure metrics (Go `saexec.metrics`).
///
/// Every field is a cloneable, `Arc`-backed prometheus handle that works before
/// (and independently of) registration, so the [`crate::Executor`] can hold and
/// update one whether or not a registry has been wired yet. Cloning shares the
/// underlying value; [`register_into`](Self::register_into) registers clones.
#[derive(Clone)]
pub struct SaexecMetrics {
    /// Accepted blocks not yet finished executing (incl. the one executing).
    execution_queue_blocks: IntGauge,
    /// Σ gas limits (worst-case gas) of the blocks currently queued.
    execution_queue_gas_limit: IntGauge,
    /// Cumulative gas limit of blocks accepted into the execution queue.
    accepted_gas_limit_total: IntCounter,
    /// Cumulative charged gas of executed blocks (not the eth gas used).
    executed_gas_charged_total: IntCounter,
    /// Cumulative gas limit (worst-case gas) of executed blocks.
    executed_gas_limit_total: IntCounter,
    /// Time from a block's enqueue until its execution completes.
    execution_queue_duration_seconds: Histogram,
    /// Wall-clock to execute one block (incl. commit + post-execution work).
    execute_block_duration_seconds: Histogram,
}

impl SaexecMetrics {
    /// Builds the seven `saexec` metrics (unregistered).
    ///
    /// # Errors
    /// Propagates a [`prometheus::Error`] if a metric descriptor or histogram
    /// bucket spec is invalid (impossible for these fixed constants).
    pub fn new() -> Result<Self, prometheus::Error> {
        Ok(Self {
            execution_queue_blocks: IntGauge::with_opts(Opts::new(
                "execution_queue_blocks",
                "Accepted SAE blocks not yet finished executing (including the one executing).",
            ))?,
            execution_queue_gas_limit: IntGauge::with_opts(Opts::new(
                "execution_queue_gas_limit",
                "Sum of the gas limits (worst-case gas) of the blocks currently in the execution queue.",
            ))?,
            accepted_gas_limit_total: IntCounter::with_opts(Opts::new(
                "accepted_gas_limit_total",
                "Cumulative gas limit (worst-case gas) of blocks accepted into the execution queue.",
            ))?,
            executed_gas_charged_total: IntCounter::with_opts(Opts::new(
                "executed_gas_charged_total",
                "Cumulative gas charged by executed blocks (tx charged gas + end-of-block op gas; not the eth gas used).",
            ))?,
            executed_gas_limit_total: IntCounter::with_opts(Opts::new(
                "executed_gas_limit_total",
                "Cumulative gas limit (worst-case gas) of executed blocks.",
            ))?,
            execution_queue_duration_seconds: Histogram::with_opts(
                HistogramOpts::new(
                    "execution_queue_duration_seconds",
                    "Seconds from a block's enqueue until its execution completes.",
                )
                // 1ms → ~16s, exponential (Go `saexec/metrics.go`).
                .buckets(exponential_buckets(0.001, 2.0, 15)?),
            )?,
            execute_block_duration_seconds: Histogram::with_opts(
                HistogramOpts::new(
                    "execute_block_duration_seconds",
                    "Seconds of wall-clock to execute one block (including state commit + post-execution work).",
                )
                // 500µs → ~16s, exponential (Go `saexec/metrics.go`).
                .buckets(exponential_buckets(0.0005, 2.0, 16)?),
            )?,
        })
    }

    /// Registers all seven metrics into `registry` (the `saexec`-namespaced
    /// registry the node hands the executor, Go `MakeAndRegister(metrics,
    /// "saexec")`).
    ///
    /// # Errors
    /// Propagates a [`prometheus::Error`] on duplicate registration.
    pub fn register_into(&self, registry: &Registry) -> Result<(), prometheus::Error> {
        registry.register(Box::new(self.execution_queue_blocks.clone()))?;
        registry.register(Box::new(self.execution_queue_gas_limit.clone()))?;
        registry.register(Box::new(self.accepted_gas_limit_total.clone()))?;
        registry.register(Box::new(self.executed_gas_charged_total.clone()))?;
        registry.register(Box::new(self.executed_gas_limit_total.clone()))?;
        registry.register(Box::new(self.execution_queue_duration_seconds.clone()))?;
        registry.register(Box::new(self.execute_block_duration_seconds.clone()))?;
        Ok(())
    }

    /// Acceptance-side event: a block with worst-case `gas_limit` was enqueued.
    /// Raises the queue gauges and the cumulative accepted-gas counter (Go
    /// `markEnqueued`).
    pub fn mark_enqueued(&self, gas_limit: u64) {
        self.execution_queue_blocks.inc();
        self.execution_queue_gas_limit
            .add(saturating_i64(gas_limit));
        self.accepted_gas_limit_total.inc_by(gas_limit);
    }

    /// Execution-side event: a block with worst-case `gas_limit` finished
    /// executing in `exec_seconds`, charging `gas_charged`. Advances the
    /// cumulative executed counters and observes the per-block duration (Go
    /// `markExecuted` + `sendPostExecutionEvents`). Independent of the queue
    /// path, so it is correct for both queued and direct
    /// [`execute_one`](crate::Executor::execute_one) calls.
    pub fn mark_executed(&self, gas_limit: u64, gas_charged: u64, exec_seconds: f64) {
        self.executed_gas_charged_total.inc_by(gas_charged);
        self.executed_gas_limit_total.inc_by(gas_limit);
        self.execute_block_duration_seconds.observe(exec_seconds);
    }

    /// Queue-side completion event: a block with worst-case `gas_limit` left the
    /// queue after `queue_seconds` of residence (enqueue → execution complete).
    /// Lowers the queue gauges and observes the queue-residence duration. Called
    /// only on the queued drain path (per [`crate::Executor::start_process_queue`]).
    pub fn mark_dequeued(&self, gas_limit: u64, queue_seconds: f64) {
        self.execution_queue_blocks.dec();
        self.execution_queue_gas_limit
            .sub(saturating_i64(gas_limit));
        self.execution_queue_duration_seconds.observe(queue_seconds);
    }
}

/// Converts a `u64` gas value to the `i64` an [`IntGauge`] takes, saturating a
/// (practically impossible) value above `i64::MAX` rather than truncating — the
/// SAE crates forbid lossy `as` casts.
fn saturating_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}
