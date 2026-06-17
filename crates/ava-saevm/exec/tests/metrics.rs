// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `saexec`-namespace execution-pressure metric tests (specs/18 §2.11; Go
//! `saexec/metrics.go`).
//!
//! Drives the [`Executor`] over a fake [`EvmDriver`] with a wired
//! [`SaexecMetrics`], registers it into a fresh [`prometheus::Registry`], and
//! gathers — asserting the queue gauges, cumulative gas counters and per-block
//! duration histograms track the enqueue/execute/dequeue event sites.

// f64 -> integer reads of gathered gauge/counter/histogram values; the values
// are tiny non-negative exact integers, so no truncation or sign loss occurs.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::sync::Arc;

use ava_database::{DynDatabase, MemDb};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{B256, EMPTY_ROOT_HASH, Header, RethBlock, SealedBlock};
use ava_saevm_blocks::{Block, WorstCaseBounds};
use ava_saevm_db::{Config, Tracker};
use ava_saevm_exec::{
    BlockOutcome, Error, EvmDriver, Executor, NoopExecHooks, SaexecMetrics, TxReceipt,
};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_hook::Op;
use ava_vm::components::gas::Price;
use prometheus::Registry;
use prometheus::proto::MetricFamily;

// ---------------------------------------------------------------------------
// Shared helpers (mirror exec/tests/{execute_step,backpressure}.rs).
// ---------------------------------------------------------------------------

const GAS_LIMIT: u64 = 8_000_000;
const GAS_CHARGED: u64 = 21_000;

fn open_provider() -> (tempfile::TempDir, Arc<FirewoodStateProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider = FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open");
    (dir, provider)
}

/// An empty-body SAE block at `number`/`parent_hash` with a non-zero gas limit
/// (so the `*_gas_limit` metrics are observably non-zero).
fn block(number: u64, parent_hash: B256) -> Arc<Block> {
    let header = Header {
        parent_hash,
        number,
        timestamp: number,
        gas_limit: GAS_LIMIT,
        transactions_root: EMPTY_ROOT_HASH,
        ..Header::default()
    };
    Arc::new(
        Block::new(SealedBlock::seal_slow(RethBlock::uncle(header)), None, None).expect("block"),
    )
}

fn genesis() -> Arc<Block> {
    let g = block(0, B256::ZERO);
    g.mark_synchronous((ava_vm::components::gas::Gas(0), GasPriceConfig::default()))
        .expect("genesis synchronous");
    g
}

fn bounds() -> WorstCaseBounds {
    WorstCaseBounds {
        max_base_fee: Price(u64::MAX),
        latest_end_time: GasTime::new(0, 1, Price(0), GasPriceConfig::default()),
        min_op_burner_balances: Vec::new(),
    }
}

fn static_clock() -> GasTime {
    GasTime::new(0, 1, Price(0), GasPriceConfig::default())
}

/// A fake driver returning a canned outcome charging `GAS_CHARGED` gas.
struct FakeDriver;

impl EvmDriver for FakeDriver {
    fn execute_block(
        &self,
        _block: &Block,
        _parent_root: B256,
        _base_fee: Price,
        _ops: &[Op],
    ) -> Result<BlockOutcome, Error> {
        Ok(BlockOutcome {
            receipts: vec![TxReceipt {
                tx_hash: B256::repeat_byte(0x11),
                gas_used: GAS_CHARGED,
                effective_gas_price: Price(7),
                reverted: false,
            }],
            gas_used: GAS_CHARGED,
            post_state_root: B256::repeat_byte(0x42),
            receipt_root: B256::repeat_byte(0x24),
        })
    }
}

fn make_executor() -> (tempfile::TempDir, Executor<FakeDriver, NoopExecHooks>) {
    let (dir, provider) = open_provider();
    let tracker = Tracker::new(provider, Config::interval(4096));
    let executor = Executor::new(
        genesis(),
        static_clock(),
        FakeDriver,
        NoopExecHooks::default(),
        tracker,
    );
    (dir, executor)
}

// -- gather readers ---------------------------------------------------------

fn counter(families: &[MetricFamily], name: &str) -> Option<u64> {
    families
        .iter()
        .find(|f| f.get_name() == name)
        .and_then(|f| f.get_metric().first())
        .map(|m| m.get_counter().get_value() as u64)
}

fn gauge(families: &[MetricFamily], name: &str) -> Option<i64> {
    families
        .iter()
        .find(|f| f.get_name() == name)
        .and_then(|f| f.get_metric().first())
        .map(|m| m.get_gauge().get_value() as i64)
}

fn hist_count(families: &[MetricFamily], name: &str) -> Option<u64> {
    families
        .iter()
        .find(|f| f.get_name() == name)
        .and_then(|f| f.get_metric().first())
        .map(|m| m.get_histogram().get_sample_count())
}

// ---------------------------------------------------------------------------
// (1) Registration exposes the seven `saexec` families with bare names.
// ---------------------------------------------------------------------------

#[test]
fn saexec_metrics_register_exposes_seven_families() {
    let registry = Registry::new();
    SaexecMetrics::new()
        .expect("build saexec metrics")
        .register_into(&registry)
        .expect("register");

    let families = registry.gather();
    let names: Vec<&str> = families.iter().map(MetricFamily::get_name).collect();
    for want in [
        "execution_queue_blocks",
        "execution_queue_gas_limit",
        "accepted_gas_limit_total",
        "executed_gas_charged_total",
        "executed_gas_limit_total",
        "execution_queue_duration_seconds",
        "execute_block_duration_seconds",
    ] {
        assert!(names.contains(&want), "saexec metric family {want} exposed");
    }
}

// ---------------------------------------------------------------------------
// (2) execute_one advances the executed counters + per-block duration.
// ---------------------------------------------------------------------------

#[test]
fn execute_one_records_executed_metrics() {
    let (_dir, executor) = make_executor();
    let metrics = Arc::new(SaexecMetrics::new().expect("metrics"));
    executor.set_metrics(Arc::clone(&metrics));

    let registry = Registry::new();
    metrics.register_into(&registry).expect("register");

    let g_hash = executor.last_executed().expect("seeded").hash();
    let block1 = block(1, g_hash);
    executor
        .execute_one(&block1, B256::ZERO, &bounds())
        .expect("execute block 1");

    let families = registry.gather();
    assert_eq!(
        counter(&families, "executed_gas_charged_total"),
        Some(GAS_CHARGED),
        "charged gas accumulated",
    );
    assert_eq!(
        counter(&families, "executed_gas_limit_total"),
        Some(GAS_LIMIT),
        "executed gas limit accumulated",
    );
    assert_eq!(
        hist_count(&families, "execute_block_duration_seconds"),
        Some(1),
        "one block-execution duration observed",
    );
    // The queue path was not exercised: its gauges/counter stay at zero.
    assert_eq!(gauge(&families, "execution_queue_blocks"), Some(0));
    assert_eq!(counter(&families, "accepted_gas_limit_total"), Some(0));
    assert_eq!(
        hist_count(&families, "execution_queue_duration_seconds"),
        Some(0),
    );
}

// ---------------------------------------------------------------------------
// (3) The queued drain path records the acceptance + queue-residence metrics
//     and returns the queue gauges to zero once drained.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_drain_records_queue_metrics() {
    let (_dir, executor) = make_executor();
    let executor = Arc::new(executor);
    let metrics = Arc::new(SaexecMetrics::new().expect("metrics"));
    executor.set_metrics(Arc::clone(&metrics));

    let registry = Registry::new();
    metrics.register_into(&registry).expect("register");

    let mut head = executor.subscribe_chain_head();
    let queue = Arc::clone(&executor).start_process_queue(8);

    let g_hash = executor.last_executed().expect("seeded").hash();
    queue
        .enqueue(block(1, g_hash), B256::ZERO, bounds())
        .await
        .expect("enqueue");

    // Block until the drain loop has executed the block (deterministic — no
    // sleeps): the chain-head event fires after execute_one + mark_dequeued.
    let evt = head.recv().await.expect("chain-head event");
    assert_eq!(evt.height, 1, "block 1 executed");

    let families = registry.gather();
    assert_eq!(
        counter(&families, "accepted_gas_limit_total"),
        Some(GAS_LIMIT),
        "acceptance-side gas limit recorded at enqueue",
    );
    assert_eq!(
        counter(&families, "executed_gas_limit_total"),
        Some(GAS_LIMIT),
        "executed gas limit recorded",
    );
    assert_eq!(
        hist_count(&families, "execution_queue_duration_seconds"),
        Some(1),
        "one queue-residence duration observed at dequeue",
    );
    assert_eq!(
        hist_count(&families, "execute_block_duration_seconds"),
        Some(1),
        "one block-execution duration observed",
    );
    // The block has been drained: the queue depth/gas gauges are back to zero.
    assert_eq!(
        gauge(&families, "execution_queue_blocks"),
        Some(0),
        "queue depth back to zero after drain",
    );
    assert_eq!(
        gauge(&families, "execution_queue_gas_limit"),
        Some(0),
        "queued gas limit back to zero after drain",
    );

    executor.shutdown().await;
}
