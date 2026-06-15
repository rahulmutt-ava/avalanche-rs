// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Execute-step tests (specs/11 §6.1).
//!
//! Three cheap tests drive the pure orchestration through a **fake**
//! [`EvmDriver`] (no live revm): the parent-hash sanity check, the
//! errored-vs-reverted tx distinction, and the worst-case base-fee bound. A
//! fourth, heavier test drives the **production** [`AvaEvmDriver`] over a real
//! `FirewoodStateProvider` (tempdir) with a genesis-funded account and one
//! recorded value-transfer tx, asserting byte-exact post-state-root parity and
//! that the block advanced E (executed) + committed D→M→I→X.

#![allow(clippy::arithmetic_side_effects)] // readable reference arithmetic in tests.

use std::str::FromStr;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use ava_database::{DynDatabase, MemDb};
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::AvaEvmConfig;
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, B256, BundleState, Decodable2718, EMPTY_ROOT_HASH, EthReceipt, Header, RethBlock,
    SealedBlock, TransactionSigned, U256,
};
use ava_saevm_blocks::{Block, WorstCaseBounds};
use ava_saevm_db::{Config, Tracker};
use ava_saevm_exec::{
    AvaEvmDriver, BlockOutcome, Error, EvmDriver, Executor, NoopExecHooks, TxReceipt,
};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_hook::Op;
use ava_vm::components::gas::Price;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Opens a fresh `FirewoodStateProvider` over in-memory side stores (mirrors the
/// saedb test setup). Holds the tempdir alive in the returned tuple.
fn open_provider() -> (tempfile::TempDir, Arc<FirewoodStateProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider = FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open");
    (dir, provider)
}

/// A minimal empty-body SAE block at `number` built on `parent_hash`.
fn empty_block(number: u64, parent_hash: B256) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        timestamp: number,
        transactions_root: EMPTY_ROOT_HASH,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

/// A worst-case bound permitting any base fee up to `max_base_fee` (no op
/// burners; `latest_end_time` is observational here).
fn bounds(max_base_fee: u64) -> WorstCaseBounds {
    WorstCaseBounds {
        max_base_fee: Price(max_base_fee),
        latest_end_time: GasTime::new(
            0,
            1,
            ava_vm::components::gas::Price(0),
            GasPriceConfig::default(),
        ),
        min_op_burner_balances: Vec::new(),
    }
}

/// A static-priced gas clock at `unix` whose `price()` equals `base_fee` — lets a
/// test pin the realised base fee deterministically (static pricing holds the
/// excess at its minimum, so `price() == min_price`).
fn static_clock(unix: u64, base_fee: u64) -> GasTime {
    GasTime::new(
        unix,
        1,
        ava_vm::components::gas::Price(0),
        GasPriceConfig::new(base_fee, 87, true),
    )
}

/// The coreth `TestApricotPhase3Config` upgrade schedule the `genesis_to_1`
/// fixture was produced under: AP1..AP3 active from genesis (London-era / revm
/// LONDON), all later forks far in the future.
fn ap3_london_upgrades() -> NetworkUpgrades {
    const FAR_FUTURE: u64 = u64::MAX;
    NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: FAR_FUTURE,
        apricot_phase_5: FAR_FUTURE,
        apricot_phase_pre_6: FAR_FUTURE,
        apricot_phase_6: FAR_FUTURE,
        apricot_phase_post_6: FAR_FUTURE,
        banff: FAR_FUTURE,
        cortina: FAR_FUTURE,
        durango: FAR_FUTURE,
        etna: FAR_FUTURE,
        fortuna: FAR_FUTURE,
        granite: FAR_FUTURE,
    }
}

/// A genesis SAE block (synchronous) whose hash a child can be built against.
fn genesis() -> Arc<Block> {
    let g = Arc::new(Block::new(empty_block(0, B256::ZERO), None, None).expect("genesis"));
    g.mark_synchronous((
        ava_vm::components::gas::Gas(0),
        ava_saevm_gastime::GasPriceConfig::default(),
    ))
    .expect("mark synchronous");
    g
}

// ---------------------------------------------------------------------------
// Fake EvmDriver
// ---------------------------------------------------------------------------

/// A fake [`EvmDriver`] returning a canned outcome (or a fatal error), so the
/// pure orchestration is exercised without a live revm.
struct FakeDriver {
    /// `Err` => the driver reports a fatal *errored* tx (block aborts);
    /// `Ok` => the canned outcome (receipts may carry `reverted: true`, which is
    /// normal).
    outcome: Result<BlockOutcome, ()>,
}

impl FakeDriver {
    fn ok(receipts: Vec<TxReceipt>, gas_used: u64) -> Self {
        Self {
            outcome: Ok(BlockOutcome {
                receipts,
                gas_used,
                post_state_root: B256::repeat_byte(0x42),
                receipt_root: B256::repeat_byte(0x24),
            }),
        }
    }

    fn fatal() -> Self {
        Self { outcome: Err(()) }
    }
}

impl EvmDriver for FakeDriver {
    fn execute_block(
        &self,
        _block: &Block,
        _parent_root: B256,
        _base_fee: Price,
        _ops: &[Op],
    ) -> Result<BlockOutcome, Error> {
        match &self.outcome {
            Ok(o) => Ok(BlockOutcome {
                receipts: o.receipts.clone(),
                gas_used: o.gas_used,
                post_state_root: o.post_state_root,
                receipt_root: o.receipt_root,
            }),
            Err(()) => Err(Error::Fatal(
                "transaction execution errored (not reverted)".to_owned(),
            )),
        }
    }
}

/// Independently derives the receipts-trie root over the single successful
/// 21k-gas legacy value-transfer receipt block 1 produces (success, no logs),
/// using the same reth ordered-trie helper the driver uses — so test (4) can
/// assert the persisted root is `derive_sha(receipts)` rather than the header's
/// declared (empty) `ReceiptHash` (specs/11 §10 inv 10).
fn single_transfer_receipt_root() -> B256 {
    let receipt: EthReceipt = EthReceipt {
        success: true,
        cumulative_gas_used: 21_000,
        ..Default::default()
    };
    EthReceipt::calculate_receipt_root_no_memo(&[receipt])
}

fn fake_receipt(reverted: bool, gas_used: u64) -> TxReceipt {
    TxReceipt {
        tx_hash: B256::repeat_byte(0x11),
        gas_used,
        effective_gas_price: Price(225_000_000_000),
        reverted,
    }
}

// ---------------------------------------------------------------------------
// (1) Parent-hash mismatch is fatal
// ---------------------------------------------------------------------------

#[test]
fn parent_hash_mismatch_is_fatal() {
    let (_dir, provider) = open_provider();
    let tracker = Tracker::new(provider, Config::archival());

    let g = genesis();
    // A block whose parent_hash is NOT the genesis hash.
    let bogus_parent = B256::repeat_byte(0xee);
    let block = Arc::new(Block::new(empty_block(1, bogus_parent), None, None).expect("block"));

    let last_executed_ptr = ArcSwapOption::from(Some(Arc::clone(&g)));
    let clock = static_clock(0, 1);
    let err = ava_saevm_exec::execute_step(
        &block,
        &g,
        &clock,
        B256::ZERO,
        &bounds(u64::MAX),
        &FakeDriver::ok(Vec::new(), 0),
        &NoopExecHooks::default(),
        &tracker,
        &last_executed_ptr,
    )
    .expect_err("parent mismatch must error");

    assert!(err.is_fatal(), "parent mismatch is fatal");
    match err {
        Error::ParentMismatch { parent, last } => {
            assert_eq!(parent, bogus_parent, "reports the block's parent hash");
            assert_eq!(last, g.hash(), "reports the last-executed hash");
        }
        other => panic!("expected ParentMismatch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// (2) Errored tx is fatal; reverted tx is normal
// ---------------------------------------------------------------------------

#[test]
fn errored_tx_is_fatal_reverted_tx_is_normal() {
    let (_dir, provider) = open_provider();
    // Interval mode so height-1's `maybe_commit` is a pipelined no-op (height 1 is
    // not a commit boundary) — the fake driver's canned root is never proposed to
    // Firewood, so an archival per-block commit would (correctly) miss its
    // proposal. The orchestration + lifecycle is what this test exercises.
    let tracker = Tracker::new(provider, Config::interval(4096));
    let g = genesis();
    let block = Arc::new(Block::new(empty_block(1, g.hash()), None, None).expect("block"));
    let clock = static_clock(0, 1);

    // (a) An *errored* tx (the fake driver returns Err) is fatal and stops the
    //     block: `mark_executed` never runs.
    {
        let last_executed_ptr = ArcSwapOption::from(Some(Arc::clone(&g)));
        let err = ava_saevm_exec::execute_step(
            &block,
            &g,
            &clock,
            B256::ZERO,
            &bounds(u64::MAX),
            &FakeDriver::fatal(),
            &NoopExecHooks::default(),
            &tracker,
            &last_executed_ptr,
        )
        .expect_err("errored tx must abort");
        assert!(err.is_fatal(), "an errored tx is fatal");
        assert!(
            !block.executed(),
            "block must not be marked executed on abort"
        );
    }

    // (b) A *reverted* tx (receipt.reverted = true) is NORMAL: gas is consumed,
    //     the block proceeds to execution + commit.
    {
        let last_executed_ptr = ArcSwapOption::from(Some(Arc::clone(&g)));
        let out = ava_saevm_exec::execute_step(
            &block,
            &g,
            &clock,
            B256::ZERO,
            &bounds(u64::MAX),
            &FakeDriver::ok(vec![fake_receipt(true, 21_000)], 21_000),
            &NoopExecHooks::default(),
            &tracker,
            &last_executed_ptr,
        )
        .expect("a reverted tx is normal — the block executes");
        assert_eq!(
            out.receipts.len(),
            1,
            "the reverted tx still produced a receipt"
        );
        assert!(out.receipts[0].reverted, "the receipt records the revert");
        assert_eq!(out.gas_used, 21_000, "a revert still consumes gas");
        assert!(block.executed(), "the block is marked executed");
        assert_eq!(
            last_executed_ptr.load_full().map(|b| b.hash()),
            Some(block.hash()),
            "last_executed advanced to the executed block",
        );
    }
}

// ---------------------------------------------------------------------------
// (3) Base fee checked against the worst-case bound
// ---------------------------------------------------------------------------

#[test]
fn base_fee_checked_against_worst_case_bound() {
    let (_dir, provider) = open_provider();
    let tracker = Tracker::new(provider, Config::archival());
    let g = genesis();
    let block = Arc::new(Block::new(empty_block(1, g.hash()), None, None).expect("block"));

    // The clock prices a unit of gas at 1_000, but the builder predicted a max of
    // 999 — the realised base fee exceeds the worst-case bound.
    let clock = static_clock(0, 1_000);
    let last_executed_ptr = ArcSwapOption::from(Some(Arc::clone(&g)));
    let err = ava_saevm_exec::execute_step(
        &block,
        &g,
        &clock,
        B256::ZERO,
        &bounds(999),
        &FakeDriver::ok(Vec::new(), 0),
        &NoopExecHooks::default(),
        &tracker,
        &last_executed_ptr,
    )
    .expect_err("base fee above the worst-case bound must error");

    match err {
        Error::WorstCase(ava_saevm_worstcase::Error::BaseFeeBoundExceeded { actual, max }) => {
            assert_eq!(actual, 1_000, "the realised base fee");
            assert_eq!(max, 999, "the predicted worst-case maximum");
        }
        other => panic!("expected WorstCase(BaseFeeBoundExceeded), got {other:?}"),
    }
    assert!(
        !block.executed(),
        "the block must not execute on a bound violation"
    );
}

// ---------------------------------------------------------------------------
// (4) A single real block advances E and commits D→M→I→X
// ---------------------------------------------------------------------------

/// The block-1 base fee recorded by coreth in the `genesis_to_1` reexecute
/// fixture (shared with `ava-evm`'s `cchain_state_root` test).
const BLOCK1_BASE_FEE: u64 = 225_000_000_000;

#[derive(serde::Deserialize)]
struct AllocEntry {
    address: String,
    balance: String,
}

#[derive(serde::Deserialize)]
struct Fixture {
    chain_id: u64,
    alloc: Vec<AllocEntry>,
    genesis_state_root: String,
    block1_txs: Vec<String>,
    expected_post_state_root: String,
}

#[test]
#[allow(clippy::too_many_lines)] // heavy end-to-end integration test (genesis materialize + real driver + parity asserts).
fn execute_single_block_advances_e_and_commits() {
    // Shared `ava-evm` reexecute fixture: AP3 (London) genesis (1 funded EOA) →
    // block 1 (one value transfer), Go-executed against coreth.
    let raw = include_str!(
        "../../../ava-evm/tests/vectors/cchain/reexecute/genesis_to_1/genesis_to_1.json"
    );
    let fx: Fixture = serde_json::from_str(raw).expect("parse fixture");

    // --- Materialize the genesis alloc into a fresh Firewood-ethhash db. ---
    let (_dir, provider) = open_provider();
    let mut builder = BundleState::builder(0..=0);
    for entry in &fx.alloc {
        let addr = Address::from_str(&entry.address).expect("alloc addr");
        let balance = U256::from_str_radix(&entry.balance, 10).expect("alloc balance");
        builder = builder.state_present_account_info(
            addr,
            ava_evm_reth::AccountInfo {
                balance,
                nonce: 0,
                ..Default::default()
            },
        );
    }
    let genesis_root = provider
        .propose_from_bundle(&builder.build())
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");
    assert_eq!(
        provider.root(),
        B256::from_str(&fx.genesis_state_root).expect("b256"),
        "genesis state root parity vs coreth",
    );

    // --- Build a SAE block carrying block 1's recorded transfer tx. ---
    // The state transition for a plain transfer depends only on the txs + the
    // (overridden) base fee + the London spec, NOT on parent_hash; we set
    // parent_hash to the synthetic SAE genesis hash so the executor's
    // parent-hash sanity check passes.
    let tx_bytes = hex::decode(fx.block1_txs[0].trim_start_matches("0x")).expect("tx hex");
    let tx = TransactionSigned::decode_2718(&mut tx_bytes.as_slice()).expect("decode tx 2718");

    let g = genesis();
    let header = Header {
        parent_hash: g.hash(),
        number: 1,
        timestamp: 10,
        gas_limit: 8_000_000,
        // The driver overrides this with the gas clock's price; set it to the
        // recorded value so the (sealed) header is self-consistent.
        base_fee_per_gas: Some(BLOCK1_BASE_FEE),
        beneficiary: Address::ZERO,
        ..Header::default()
    };
    let mut eth = RethBlock::uncle(header);
    eth.body.transactions = vec![tx];
    let block = Arc::new(
        Block::new(
            SealedBlock::seal_slow(eth),
            Some(Arc::clone(&g)),
            Some(Arc::clone(&g)),
        )
        .expect("block"),
    );

    // --- The production driver over the AP3/London schedule (matches the
    //     fixture's coreth `TestApricotPhase3Config`). ---
    let chain_spec = AvaChainSpec::from_parts(
        ap3_london_upgrades(),
        ava_evm_reth::Chain::from_id(fx.chain_id),
        false,
    );
    let config = AvaEvmConfig::new(chain_spec);
    let driver = AvaEvmDriver::new(config, Arc::clone(&provider));

    // Pin the realised base fee to the recorded value via a static-priced clock,
    // so the driver's base-fee override reproduces the recorded post-state root.
    let clock = static_clock(9, BLOCK1_BASE_FEE);
    let tracker = Tracker::new(Arc::clone(&provider), Config::archival());
    let last_executed_ptr = ArcSwapOption::from(Some(Arc::clone(&g)));

    let out = ava_saevm_exec::execute_step(
        &block,
        &g,
        &clock,
        genesis_root,
        &bounds(u64::MAX),
        &driver,
        &NoopExecHooks::default(),
        &tracker,
        &last_executed_ptr,
    )
    .expect("execute the real block");

    // --- Post-state-root parity vs coreth (the load-bearing assertion). ---
    let expected = B256::from_str(&fx.expected_post_state_root).expect("b256");
    assert_eq!(
        out.post_state_root, expected,
        "SAE post-state root parity vs coreth",
    );
    assert_eq!(out.base_fee, Price(BLOCK1_BASE_FEE), "realised base fee");
    assert_eq!(out.receipts.len(), 1, "one transfer receipt");
    assert!(!out.receipts[0].reverted, "the transfer succeeds");
    assert_eq!(out.gas_used, 21_000, "a value transfer costs 21000 gas");

    // --- The block advanced E (executed) and committed D→M→I→X. ---
    assert!(block.executed(), "the block is marked executed");
    assert_eq!(
        block.post_execution_state_root(),
        expected,
        "the executed post-state root is recorded on the block",
    );

    // --- The persisted receipt_root is DERIVED from the produced receipts
    //     (specs/11 §10 inv 10), NOT the eth header's declared ReceiptHash. ---
    let persisted = block
        .execution_results()
        .expect("execution results recorded");
    assert_eq!(
        persisted.receipt_root,
        single_transfer_receipt_root(),
        "receipt_root is derive_sha over the produced receipts (specs/11 §10 inv 10)",
    );
    assert_ne!(
        persisted.receipt_root, EMPTY_ROOT_HASH,
        "the derived receipt_root is NOT the header's declared (empty) ReceiptHash",
    );
    assert_eq!(
        last_executed_ptr.load_full().map(|b| b.hash()),
        Some(block.hash()),
        "last_executed advanced to the executed block (the X step)",
    );
    // The committed revision is durably openable (D: archival commits every
    // block).
    tracker
        .state_db(expected)
        .expect("the committed post-state revision is openable");
}

// ---------------------------------------------------------------------------
// (5) The Executor wrapper: cross-block clock continuity + receipt accumulation
// ---------------------------------------------------------------------------

#[test]
fn executor_execute_one_chains_blocks_and_accumulates_receipts() {
    let (_dir, provider) = open_provider();
    // Interval mode so the fake driver's canned (never-proposed) root is not
    // archival-committed; this test exercises the Executor wrapper, not Firewood.
    let tracker = Tracker::new(provider, Config::interval(4096));

    let g = genesis();
    // A static clock so each block's realised base fee is deterministic and the
    // before/after-block advance is well-defined across blocks.
    let clock = static_clock(0, 1);
    let executor = Executor::new(
        Arc::clone(&g),
        clock,
        FakeDriver::ok(vec![fake_receipt(false, 21_000)], 21_000),
        NoopExecHooks::default(),
        tracker,
    );

    // The `sae` last_executed_height gauge starts at the seeded genesis height.
    assert_eq!(
        executor.last_executed_height(),
        0,
        "gauge seeded at genesis height",
    );

    // Block 1, built on the genesis SAE hash.
    let block1 = Arc::new(Block::new(empty_block(1, g.hash()), None, None).expect("block1"));
    let out1 = executor
        .execute_one(&block1, B256::ZERO, &bounds(u64::MAX))
        .expect("execute block 1");
    assert_eq!(
        executor.last_executed().map(|b| b.hash()),
        Some(block1.hash()),
        "executor advanced last_executed to block 1",
    );
    assert_eq!(
        executor.last_executed_height(),
        1,
        "gauge advanced per post-execution event (block 1)",
    );
    assert_eq!(executor.receipts().snapshot().len(), 1, "block 1's receipt");

    // Block 2, chained on block 1's SAE hash — the parent-hash check must pass
    // against the executor's now-advanced last_executed pointer.
    let block2 = Arc::new(Block::new(empty_block(2, block1.hash()), None, None).expect("block2"));
    let out2 = executor
        .execute_one(&block2, B256::ZERO, &bounds(u64::MAX))
        .expect("execute block 2");
    assert_eq!(
        executor.last_executed().map(|b| b.hash()),
        Some(block2.hash()),
        "executor advanced last_executed to block 2",
    );
    assert_eq!(
        executor.last_executed_height(),
        2,
        "gauge advanced per post-execution event (block 2)",
    );
    assert_eq!(
        executor.receipts().snapshot().len(),
        2,
        "the ReceiptSink accumulated both blocks' receipts in order",
    );

    // Cross-block gas-clock continuity: block 2's clock is seeded from block 1's
    // executed gas-time, so its instant is >= block 1's (the clock never rewinds).
    assert!(
        out2.gas_time.time().unix_seconds() >= out1.gas_time.time().unix_seconds(),
        "block 2's gas clock continues from block 1's (monotonic)",
    );
}
