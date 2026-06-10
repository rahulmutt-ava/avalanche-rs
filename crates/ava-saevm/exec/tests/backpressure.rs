// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Backpressure tests (specs/11 §6.2, M7.26).
//!
//! Two tests exercise the bounded-mpsc queue contract:
//!
//! 1. `flood_accept_keeps_queue_bounded` — issue `enqueue` calls (accepts)
//!    faster than the executor loop drains; assert the mpsc queue stays bounded
//!    (the next enqueue parks once the channel is full, rather than buffering
//!    unboundedly).
//!
//! 2. `builder_refuses_when_worst_case_queue_full` — drive a
//!    `worstcase::State` to a depth > `MaxFullBlocksInOpenQueue·Ω_B`; assert
//!    `start_block` returns `Error::QueueFull`, i.e. the builder's
//!    `ErrQueueFull` pacing fires.

#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::cast_possible_truncation)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ava_database::{DynDatabase, MemDb};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{Address, B256, EMPTY_ROOT_HASH, Header, RethBlock, SealedBlock, SealedHeader};
use ava_saevm_blocks::{Block, WorstCaseBounds};
use ava_saevm_db::{Config, Tracker};
use ava_saevm_exec::{BlockOutcome, Error, EvmDriver, Executor, NoopExecHooks};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_hook::op::{Op, StateMut};
use ava_saevm_hook::{Points, StateRead};
use ava_saevm_types::U256;
use ava_saevm_worstcase::{Error as WcError, State, state::safe_max_block_size};
use ava_vm::components::gas::{Gas, Price};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn open_provider() -> (tempfile::TempDir, Arc<FirewoodStateProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider = FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open");
    (dir, provider)
}

fn empty_sae_block(number: u64, parent_hash: B256) -> Arc<Block> {
    let header = Header {
        parent_hash,
        number,
        timestamp: number,
        transactions_root: EMPTY_ROOT_HASH,
        ..Header::default()
    };
    Arc::new(
        Block::new(SealedBlock::seal_slow(RethBlock::uncle(header)), None, None).expect("block"),
    )
}

fn sae_genesis() -> Arc<Block> {
    let g = empty_sae_block(0, B256::ZERO);
    g.mark_synchronous().expect("genesis synchronous");
    g
}

fn permissive_bounds() -> WorstCaseBounds {
    WorstCaseBounds {
        max_base_fee: Price(u64::MAX),
        latest_end_time: GasTime::new(0, 1, 0, GasPriceConfig::default()),
        min_op_burner_balances: Vec::new(),
    }
}

fn static_clock(unix: u64) -> GasTime {
    GasTime::new(unix, 1, 0, GasPriceConfig::default())
}

// ---------------------------------------------------------------------------
// GatedDriver: a fake EvmDriver that blocks until permits are issued
// ---------------------------------------------------------------------------

/// A fake [`EvmDriver`] that blocks `execute_block` until a permit is issued
/// via a semaphore, letting the test control exactly when execution proceeds.
struct GatedDriver {
    gate: Arc<tokio::sync::Semaphore>,
    executed: Arc<AtomicUsize>,
}

impl EvmDriver for GatedDriver {
    fn execute_block(
        &self,
        _block: &Block,
        _parent_root: B256,
        _base_fee: Price,
        _ops: &[Op],
    ) -> Result<BlockOutcome, Error> {
        // Spin until a permit is available. Called from async context
        // (the processQueue loop), but the trait is sync, so we spin-yield.
        loop {
            if self.gate.try_acquire().is_ok() {
                break;
            }
            std::thread::yield_now();
        }
        self.executed.fetch_add(1, Ordering::SeqCst);
        Ok(BlockOutcome {
            receipts: Vec::new(),
            gas_used: 0,
            post_state_root: B256::repeat_byte(0x42),
            receipt_root: B256::repeat_byte(0x24),
        })
    }
}

// ---------------------------------------------------------------------------
// Test 1: flood_accept_keeps_queue_bounded
//
// Confirm that with a bounded channel of `capacity`, the (capacity+1)-th
// enqueue parks until the executor drains a slot (backpressure paces accepts).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flood_accept_keeps_queue_bounded() {
    let (_dir, provider) = open_provider();
    let tracker = Tracker::new(Arc::clone(&provider), Config::interval(4096));

    let gate = Arc::new(tokio::sync::Semaphore::new(0)); // executor blocked
    let executed = Arc::new(AtomicUsize::new(0));

    let g = sae_genesis();
    let clock = static_clock(0);

    let executor = Arc::new(Executor::new(
        Arc::clone(&g),
        clock,
        GatedDriver {
            gate: Arc::clone(&gate),
            executed: Arc::clone(&executed),
        },
        NoopExecHooks::default(),
        tracker,
    ));

    // Use a small capacity so backpressure kicks in quickly.
    let capacity = 3usize;
    let queue = executor.clone().start_process_queue(capacity);

    // Build `capacity + 1` chained blocks.
    let mut blocks = Vec::new();
    let mut prev_hash = g.hash();
    for i in 1..=(capacity + 1) {
        let b = empty_sae_block(i as u64, prev_hash);
        prev_hash = b.hash();
        blocks.push(b);
    }

    // Enqueue the first `capacity` blocks — these fill the channel without blocking.
    for b in &blocks[..capacity] {
        queue
            .enqueue(Arc::clone(b), B256::ZERO, permissive_bounds())
            .await
            .expect("enqueue should succeed (channel not yet full)");
    }

    // The channel is now full. The extra enqueue must park until a slot opens.
    let extra_block = Arc::clone(&blocks[capacity]);
    let queue2 = queue.clone();
    let enqueue_task = tokio::spawn(async move {
        queue2
            .enqueue(extra_block, B256::ZERO, permissive_bounds())
            .await
            .expect("extra enqueue should eventually succeed");
    });

    // Give the task time to run and park on the full channel.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(
        !enqueue_task.is_finished(),
        "enqueue should be parked on a full queue (backpressure active)"
    );

    // Release one permit: the executor loop drains one block, opening a slot.
    gate.add_permits(1);
    enqueue_task
        .await
        .expect("extra enqueue should complete once a slot opens");

    // Release remaining permits and shut down cleanly.
    gate.add_permits(capacity);
    executor.shutdown().await;
}

// ---------------------------------------------------------------------------
// Test 2: builder_refuses_when_worst_case_queue_full
//
// Drive a `worstcase::State` past MaxFullBlocksInOpenQueue·Ω_B and assert
// `start_block` returns `Error::QueueFull` (the builder's ErrQueueFull guard).
// ---------------------------------------------------------------------------

/// A minimal [`Points`] stub: fixed gas config, header timestamp as block time.
struct StubPoints {
    target: Gas,
    config: GasPriceConfig,
}

#[derive(Debug)]
struct StubError;

impl Points for StubPoints {
    type Error = StubError;
    type Block = ();
    type Receipts = ();
    type Rules = ();
    type ExecutionResultsDb = ();

    fn execution_results_db(&self, _: &str) -> Result<(), StubError> {
        Ok(())
    }
    fn gas_config_after(&self, _: &SealedHeader) -> (Gas, GasPriceConfig) {
        (self.target, self.config)
    }
    fn block_time(&self, h: &SealedHeader) -> (u64, u32) {
        (h.timestamp, 0)
    }
    fn settled_by(&self, _: &SealedHeader) -> ava_saevm_hook::Settled {
        unreachable!("settled_by not used by worstcase")
    }
    fn end_of_block_ops(&self, (): &()) -> Result<Vec<Op>, StubError> {
        Ok(Vec::new())
    }
    fn can_execute_transaction(
        &self,
        _: Address,
        _: Option<Address>,
        _: &dyn StateRead,
    ) -> Result<(), StubError> {
        Ok(())
    }
    fn before_executing_block(
        &self,
        (): &(),
        _: &mut dyn StateMut,
        (): &(),
    ) -> Result<(), StubError> {
        Ok(())
    }
    fn after_executing_block(
        &self,
        _: &mut dyn StateMut,
        (): &(),
        (): (),
    ) -> Result<(), StubError> {
        Ok(())
    }
}

/// A minimal in-memory [`StateMut`] for the worstcase `apply` call.
#[derive(Default)]
struct MemState {
    balances: BTreeMap<Address, U256>,
    nonces: BTreeMap<Address, u64>,
}

impl StateRead for MemState {
    fn balance(&self, a: Address) -> U256 {
        self.balances.get(&a).copied().unwrap_or(U256::MAX)
    }
    fn nonce(&self, a: Address) -> u64 {
        self.nonces.get(&a).copied().unwrap_or(0)
    }
}

impl StateMut for MemState {
    fn balance(&self, a: Address) -> U256 {
        self.balances.get(&a).copied().unwrap_or(U256::MAX)
    }
    fn nonce(&self, a: Address) -> u64 {
        self.nonces.get(&a).copied().unwrap_or(0)
    }
    fn set_nonce(&mut self, a: Address, n: u64) {
        self.nonces.insert(a, n);
    }
    fn sub_balance(&mut self, a: Address, amount: U256) {
        let e = self.balances.entry(a).or_insert(U256::MAX);
        *e = e.saturating_sub(amount);
    }
    fn add_balance(&mut self, a: Address, amount: U256) {
        let e = self.balances.entry(a).or_insert(U256::ZERO);
        *e = e.saturating_add(amount);
    }
}

#[test]
fn builder_refuses_when_worst_case_queue_full() {
    // Initial gas clock with a meaningful gas target so Ω_B > 0.
    let target = 1_000_000u64;
    let clock = GasTime::new(0, target, 0, GasPriceConfig::default());
    let genesis_hash = B256::repeat_byte(0x00);

    let mut wc = State::new(
        StubPoints {
            target: Gas(target),
            config: GasPriceConfig::default(),
        },
        clock.clone(),
        genesis_hash,
    );

    // Ω_B = safe_max_block_size(&clock).
    let omega_b = safe_max_block_size(&clock);
    assert!(omega_b > 0, "Ω_B must be nonzero");

    // Threshold = MaxFullBlocksInOpenQueue * Ω_B = 2 * Ω_B.
    let max_open_q = ava_saevm_params::MAX_FULL_BLOCKS_IN_OPEN_QUEUE.saturating_mul(omega_b);

    // Drive `max + 2` full blocks through the worstcase state.
    // Each `finish_block` accumulates `block_size` (= gas applied) into `q_size`.
    // MaxFullBlocksInOpenQueue+1 full blocks exceed the threshold.
    let max_iters = ava_saevm_params::MAX_FULL_BLOCKS_IN_OPEN_QUEUE.saturating_add(2);

    let mut prev_hash = genesis_hash;
    let mut queue_full_err: Option<WcError> = None;

    for i in 0u64..max_iters {
        let h = SealedHeader::seal_slow(Header {
            parent_hash: prev_hash,
            number: i.saturating_add(1),
            timestamp: i.saturating_add(1).saturating_mul(10),
            gas_limit: omega_b,
            ..Header::default()
        });

        match wc.start_block(&h) {
            Err(e @ WcError::QueueFull { .. }) => {
                queue_full_err = Some(e);
                break;
            }
            Err(other) => panic!("unexpected worstcase error: {other:?}"),
            Ok(()) => {}
        }

        prev_hash = h.hash();

        // Fill the block to omega_b by applying a single Op that consumes the
        // full gas limit. The fee cap must be >= the worst-case base fee or
        // `apply` rejects it with `FeeCapTooLow` (the base fee is >= the min
        // price of 1, so a zero fee cap never fills the block). The test's
        // `MemState` reports a `U256::MAX` balance, so the (large) worst-case
        // debit `gas * fee_cap` is always affordable and the gas counter grows.
        let gas = wc.gas_limit();
        if gas > 0 {
            let op = State::<StubPoints>::tx_to_op_inner(
                Address::ZERO,
                gas,
                U256::from(u64::MAX), // fee_cap >= base_fee ⇒ apply succeeds
                U256::ZERO,
                U256::ZERO,
                wc.base_fee(),
            )
            .expect("tx_to_op_inner");
            let mut state = MemState::default();
            wc.apply(&op, &mut state)
                .expect("apply fills the block to omega_b");
        }

        wc.finish_block().expect("finish_block");
    }

    // The builder MUST have returned QueueFull before exhausting all `max_iters`.
    match queue_full_err {
        Some(WcError::QueueFull { size, max }) => {
            assert!(
                size > max,
                "QueueFull: size ({size}) must exceed max ({max})"
            );
            assert_eq!(
                max, max_open_q,
                "QueueFull max must equal MaxFullBlocksInOpenQueue * Ω_B ({max_open_q})"
            );
        }
        other => panic!(
            "expected worstcase::Error::QueueFull after filling the queue; \
             got {other:?} (max_open_q={max_open_q}, omega_b={omega_b})"
        ),
    }
}
