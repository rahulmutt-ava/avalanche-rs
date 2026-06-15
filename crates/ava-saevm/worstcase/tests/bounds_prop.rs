// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Property tests: worst-case bounds are **never** violated by realised
//! execution (specs/11 §12, §9.3).
//!
//! Two properties are tested over random tx sets driven through the [`State`]
//! (worst-case prediction) + `check_*` seam, without spinning up a real EVM:
//!
//! - `actual_base_fee_le_max_base_fee`: the realised base fee computed from the
//!   same gas clock never exceeds `WorstCaseBounds::max_base_fee`.
//! - `sender_balances_ge_min_op_burner_balances`: before applying each op in
//!   actual execution, every sender's balance in the actual state is ≥ the
//!   pre-burn balance snapshot recorded by the worst-case replay.
//!
//! The prediction is conservative by construction: it charges the maximum
//! possible amount (`gas * fee_cap + value`) per op, while actual execution
//! charges `gas * min(fee_cap, base_fee + tip_cap) + value ≤ gas * fee_cap +
//! value`. Therefore the worst-case balances are always ≤ actual balances, and
//! the bound must never be violated.

// Readable reference arithmetic in test fixtures; operands are proptest-bounded
// small values, so neither overflow nor truncation can occur in practice.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::collections::BTreeMap;

use ava_evm_reth::{Header, SealedHeader};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_hook::StateRead;
use ava_saevm_hook::op::{AccountDebit, Op, StateMut};
use ava_saevm_types::{Address, B256, U256};
use ava_saevm_worstcase::{State, check_base_fee_bound, check_sender_balance_bound};
use ava_vm::components::gas::{Gas, Price};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Constants matching the existing worstcase.rs test harness
// ---------------------------------------------------------------------------

const INITIAL_GAS_TARGET: u64 = 1_000_000;
// max excess yielding a price of 1 at INITIAL_GAS_TARGET
const INITIAL_EXCESS: u64 = 60_303_807;

fn config() -> GasPriceConfig {
    GasPriceConfig::default()
}

fn clock(target: u64, excess: u64) -> GasTime {
    GasTime::from_excess(0, target, excess, config())
}

// ---------------------------------------------------------------------------
// In-memory state seam (reused from worstcase.rs style)
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct MemState {
    balances: BTreeMap<Address, U256>,
    nonces: BTreeMap<Address, u64>,
}

impl MemState {
    fn set_balance(&mut self, addr: Address, bal: U256) {
        self.balances.insert(addr, bal);
    }
}

impl StateRead for MemState {
    fn balance(&self, a: Address) -> U256 {
        self.balances.get(&a).copied().unwrap_or(U256::ZERO)
    }
    fn nonce(&self, a: Address) -> u64 {
        self.nonces.get(&a).copied().unwrap_or(0)
    }
}

impl StateMut for MemState {
    fn balance(&self, a: Address) -> U256 {
        self.balances.get(&a).copied().unwrap_or(U256::ZERO)
    }
    fn nonce(&self, a: Address) -> u64 {
        self.nonces.get(&a).copied().unwrap_or(0)
    }
    fn set_nonce(&mut self, a: Address, n: u64) {
        self.nonces.insert(a, n);
    }
    fn sub_balance(&mut self, a: Address, amount: U256) {
        let e = self.balances.entry(a).or_insert(U256::ZERO);
        *e = e.saturating_sub(amount);
    }
    fn add_balance(&mut self, a: Address, amount: U256) {
        let e = self.balances.entry(a).or_insert(U256::ZERO);
        *e = e.saturating_add(amount);
    }
}

// ---------------------------------------------------------------------------
// Minimal hook stub (mirrors worstcase.rs StubHooks)
// ---------------------------------------------------------------------------

struct StubHooks {
    target: Gas,
    cfg: GasPriceConfig,
}

impl StubHooks {
    fn new(target: u64) -> Self {
        Self {
            target: Gas(target),
            cfg: GasPriceConfig::default(),
        }
    }
}

#[derive(Debug)]
struct StubError;

impl ava_saevm_hook::Points for StubHooks {
    type Error = StubError;
    type Block = ();
    type Receipts = ();
    type Rules = ();
    type ExecutionResultsDb = ();

    fn execution_results_db(&self, _: &str) -> Result<(), StubError> {
        Ok(())
    }
    fn gas_config_after(&self, _: &SealedHeader) -> (Gas, GasPriceConfig) {
        (self.target, self.cfg)
    }
    fn block_time(&self, _: &SealedHeader) -> (u64, u32) {
        (0, 0)
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

fn header(parent: B256, number: u64) -> SealedHeader {
    SealedHeader::seal_slow(Header {
        parent_hash: parent,
        number,
        timestamp: 0,
        ..Header::default()
    })
}

// ---------------------------------------------------------------------------
// A single-tx description drawn by proptest
// ---------------------------------------------------------------------------

/// A randomised but always-affordable single-sender tx description.
///
/// `gas_fee_cap >= gas_tip_cap` and `gas_fee_cap * gas + value <= initial_balance`
/// to ensure the worst-case apply succeeds.
#[derive(Clone, Debug)]
struct TxDesc {
    /// Sender address byte (used as `Address::repeat_byte`).
    sender_byte: u8,
    /// Gas allocated to this tx (21 000 – 100 000).
    gas: u64,
    /// The fee cap (worst-case price; must be >= `base_fee`).
    gas_fee_cap: u64,
    /// The tip cap (`effective_price` = `min(fee_cap, base_fee + tip_cap)`).
    gas_tip_cap: u64,
    /// Value transferred (kept small so affordability holds).
    value: u64,
    /// Initial balance of the sender.
    initial_balance: u64,
}

/// Strategy producing a [`TxDesc`] that is always affordable under worst-case
/// assumptions (`gas * fee_cap + value <= initial_balance`), with `fee_cap >= 1`
/// (base fee is 1 at `INITIAL_EXCESS`/`INITIAL_GAS_TARGET`) and `fee_cap >= tip_cap`.
fn tx_strategy() -> impl Strategy<Value = TxDesc> {
    // Fix the sender-address byte to a non-zero value (0 is the zero address).
    let sender_byte = 1u8..=200u8;
    // gas in [21_000, 100_000].
    let gas = 21_000u64..=100_000u64;
    // fee_cap in [1, 1_000]; tip_cap in [0, fee_cap].
    let fee_cap = 1u64..=1_000u64;
    (sender_byte, gas, fee_cap).prop_flat_map(|(sender_byte, gas, fee_cap)| {
        let tip_cap = 0u64..=fee_cap;
        // value in [0, 1_000].
        let value = 0u64..=1_000u64;
        (Just(sender_byte), Just(gas), Just(fee_cap), tip_cap, value).prop_map(
            move |(sb, g, fc, tc, v)| {
                // initial_balance = gas * fee_cap + value + some headroom (100_000).
                // All of these fit comfortably in u64 with the bounds above.
                let min_needed = g * fc + v;
                let initial_balance = min_needed + 100_000;
                TxDesc {
                    sender_byte: sb,
                    gas: g,
                    gas_fee_cap: fc,
                    gas_tip_cap: tc,
                    value: v,
                    initial_balance,
                }
            },
        )
    })
}

/// Strategy producing a non-empty list of 1–4 [`TxDesc`]s.
fn tx_list_strategy() -> impl Strategy<Value = Vec<TxDesc>> {
    proptest::collection::vec(tx_strategy(), 1..=4)
}

// ---------------------------------------------------------------------------
// Helpers for building Ops from TxDescs
// ---------------------------------------------------------------------------

/// Builds the worst-case [`Op`] for a [`TxDesc`] (charges `gas * fee_cap + value`).
fn wc_op(desc: &TxDesc, nonce: u64, base_fee: Price) -> Op {
    State::<StubHooks>::tx_to_op_inner(
        Address::repeat_byte(desc.sender_byte),
        desc.gas,
        U256::from(desc.gas_fee_cap),
        U256::from(desc.gas_tip_cap),
        U256::from(desc.value),
        base_fee,
    )
    .expect("tx_to_op_inner must not overflow for bounded inputs")
    // Stamp the nonce onto the burn entry so apply() validates it.
    .with_nonce(Address::repeat_byte(desc.sender_byte), nonce)
}

/// Builds the actual (effective-price) [`Op`] for a [`TxDesc`]: charges
/// `gas * min(fee_cap, base_fee + tip_cap) + value`.
fn actual_op(desc: &TxDesc, nonce: u64, base_fee: Price) -> Op {
    // effective_gas_price = min(fee_cap, base_fee + tip_cap)
    let effective = (base_fee.0 + desc.gas_tip_cap).min(desc.gas_fee_cap);
    let amount = desc.gas * effective + desc.value;
    let mut burn = BTreeMap::new();
    burn.insert(
        Address::repeat_byte(desc.sender_byte),
        AccountDebit {
            nonce,
            amount: U256::from(amount),
            min_balance: U256::from(amount),
        },
    );
    Op {
        id: ava_types::id::Id::EMPTY,
        gas: Gas(desc.gas),
        gas_fee_cap: U256::from(desc.gas_fee_cap),
        burn,
        mint: BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Op extension helper (sets the nonce on the burn entry after construction)
// ---------------------------------------------------------------------------

trait OpExt {
    fn with_nonce(self, addr: Address, nonce: u64) -> Self;
}

impl OpExt for Op {
    fn with_nonce(mut self, addr: Address, nonce: u64) -> Self {
        if let Some(debit) = self.burn.get_mut(&addr) {
            debit.nonce = nonce;
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

mod prop {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

        /// specs/11 §12 property 1: the realised base fee at execution never
        /// exceeds `WorstCaseBounds::max_base_fee`.
        ///
        /// The worst-case replay captures `base_fee = gas_clock.price()` at
        /// `start_block`. The executor derives the realised base fee from the
        /// same gas clock after the same `before_block` call. Because both
        /// paths use the same clock, the realised base fee equals the predicted
        /// maximum; the bound is never violated.
        #[test]
        fn actual_base_fee_le_max_base_fee(
            clock_target in INITIAL_GAS_TARGET..=INITIAL_GAS_TARGET * 10,
            clock_excess in 0u64..=INITIAL_EXCESS * 2,
        ) {
            let settled_hash = B256::repeat_byte(0xab);

            // Build the worst-case state and capture the block's worst-case
            // base fee via start_block + finish_block (no ops needed).
            let bounds = {
                let mut s = State::new(
                    StubHooks::new(clock_target),
                    clock(clock_target, clock_excess),
                    settled_hash,
                );
                s.start_block(&header(settled_hash, 0)).expect("start_block");
                s.finish_block().expect("finish_block")
            };

            // The executor derives its realised base fee from the same gas
            // clock after an identical before_block call. Reproduce that here:
            let mut exec_clock = clock(clock_target, clock_excess);
            exec_clock.before_block(0, 0); // same block_time as StubHooks
            let realised_base_fee = exec_clock.price();

            // The realised and worst-case base fees must agree (same clock),
            // and the bound must never be violated.
            prop_assert_eq!(
                realised_base_fee, bounds.max_base_fee,
                "realised and worst-case base fees derive from the same clock"
            );
            prop_assert!(
                check_base_fee_bound(&bounds, realised_base_fee).is_ok(),
                "base-fee bound must never be violated: actual={} max={}",
                realised_base_fee.0,
                bounds.max_base_fee.0,
            );
        }

        /// specs/11 §12 property 2: before applying each op in actual
        /// execution, every sender's balance ≥ the pre-burn worst-case balance
        /// snapshot (`WorstCaseBounds::min_op_burner_balances[i]`).
        ///
        /// The prediction charges the maximum amount (`gas * fee_cap + value`)
        /// per op. Actual execution charges at most that much (effective price
        /// ≤ fee cap). After i completed ops the worst-case state has consumed
        /// at least as much balance as the actual state, so the worst-case
        /// pre-op[i] balance ≤ actual pre-op[i] balance — the bound must never
        /// be violated.
        #[test]
        fn sender_balances_ge_min_op_burner_balances(txs in tx_list_strategy()) {
            let settled_hash = B256::repeat_byte(0xab);
            let c = clock(INITIAL_GAS_TARGET, INITIAL_EXCESS);

            // --- worst-case replay -------------------------------------------
            // Build a MemState pre-funded with each sender's initial balance
            // and run the worst-case State over it to capture the bounds.
            let mut wc_mem = MemState::default();
            for tx in &txs {
                wc_mem.set_balance(
                    Address::repeat_byte(tx.sender_byte),
                    U256::from(tx.initial_balance),
                );
            }

            let mut wc = State::new(StubHooks::new(INITIAL_GAS_TARGET), c.clone(), settled_hash);
            wc.start_block(&header(settled_hash, 0)).expect("start_block");
            let base_fee = wc.base_fee();

            // Track per-sender nonces for the worst-case replay.
            let mut wc_nonces: BTreeMap<u8, u64> = BTreeMap::new();
            let mut applied_count = 0usize;
            for tx in &txs {
                let nonce = *wc_nonces.get(&tx.sender_byte).unwrap_or(&0);
                let op = wc_op(tx, nonce, base_fee);
                if wc.apply(&op, &mut wc_mem).is_ok() {
                    applied_count += 1;
                    wc_nonces.insert(tx.sender_byte, nonce + 1);
                }
                // If the worst-case state rejects the op (e.g. gas limit
                // reached) we stop here; only the accepted prefix is bounded.
                else {
                    break;
                }
            }
            let bounds = wc.finish_block().expect("finish_block");

            // --- actual execution --------------------------------------------
            // Start a fresh exec state with identical initial balances. For
            // each op i that was accepted: (a) verify the balance bound holds
            // BEFORE applying the op, then (b) apply the op with the actual
            // (lower) effective-price charge.
            let mut exec_mem = MemState::default();
            for tx in &txs {
                exec_mem.set_balance(
                    Address::repeat_byte(tx.sender_byte),
                    U256::from(tx.initial_balance),
                );
            }

            let mut exec_nonces: BTreeMap<u8, u64> = BTreeMap::new();
            let mut op_index = 0usize;

            for tx in &txs {
                if op_index >= applied_count {
                    break;
                }
                let nonce = *exec_nonces.get(&tx.sender_byte).unwrap_or(&0);
                let op = actual_op(tx, nonce, base_fee);

                // (a) Check balance bound BEFORE applying this op.
                let check = check_sender_balance_bound(&bounds, op_index, &exec_mem);
                prop_assert!(
                    check.is_ok(),
                    "sender balance bound violated at op {}: {:?}",
                    op_index,
                    check.unwrap_err(),
                );

                // (b) Apply the actual (effective-price) op.
                if op.apply_to(&mut exec_mem).is_ok() {
                    exec_nonces.insert(tx.sender_byte, nonce + 1);
                }
                op_index += 1;
            }

            // All accepted ops must have been covered by the bound check.
            prop_assert_eq!(
                op_index,
                applied_count,
                "all accepted ops should be covered by the actual execution loop"
            );
        }
    }
}
