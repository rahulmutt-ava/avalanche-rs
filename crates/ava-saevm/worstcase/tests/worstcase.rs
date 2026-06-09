// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Worst-case predictive-bounds tests (specs/11 §9.3/§2.4/§6.1).
//!
//! Mirrors `vms/saevm/worstcase/state_test.go`. The reth-tx-specific
//! `ApplyTx` (intrinsic-gas validation + signer recovery + EOA codehash) is
//! deferred to M7.14, so the affordability math is exercised via `tx_to_op`
//! and `apply` directly (the `Op` seam), matching the Go `Apply`/`txToOp`
//! split.

// Readable reference arithmetic in test fixtures; the operands are tiny
// compile-time constants, so neither overflow nor truncation can occur here.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;

use ava_evm_reth::{Header, SealedHeader};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_hook::op::{AccountDebit, Op, StateMut};
use ava_saevm_hook::{Points, StateRead};
use ava_saevm_types::{Address, B256, U256};
use ava_saevm_worstcase::{Error, State, mul_add, safe_max_block_size};
use ava_vm::components::gas::{Gas, Price};

// --- gas-time fixture constants (mirror the Go test) ----------------------
const INITIAL_GAS_TARGET: u64 = 1_000_000;
const INITIAL_EXCESS: u64 = 60_303_807; // max excess yielding a price of 1
// TARGET_TO_RATE (2) * (TauSeconds (5) * Lambda (2)) = 20.
const TARGET_TO_MAX_BLOCK_SIZE: u64 = 20;
const INITIAL_MAX_BLOCK_SIZE: u64 = INITIAL_GAS_TARGET * TARGET_TO_MAX_BLOCK_SIZE; // 20_000_000

fn config() -> GasPriceConfig {
    GasPriceConfig::default()
}

fn clock(target: u64, excess: u64) -> GasTime {
    GasTime::new(0, target, excess, config())
}

// --- in-memory state seam --------------------------------------------------

/// A trivial in-memory account state implementing the hook [`StateMut`] /
/// [`StateRead`] seams, standing in for the revm/Firewood-backed state that
/// wires in M7.14.
#[derive(Default)]
struct MemState {
    balances: BTreeMap<Address, U256>,
    nonces: BTreeMap<Address, u64>,
}

impl MemState {
    fn with_balance(addr: Address, bal: U256) -> Self {
        let mut s = Self::default();
        s.balances.insert(addr, bal);
        s
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

// --- hook stub -------------------------------------------------------------

/// A minimal [`Points`] stub: a fixed gas target/config after every block, a
/// zero block time, and an always-allow `can_execute_transaction`.
struct StubHooks {
    target: Gas,
    config: GasPriceConfig,
}

impl StubHooks {
    fn new(target: u64) -> Self {
        Self {
            target: Gas(target),
            config: GasPriceConfig::default(),
        }
    }
}

#[derive(Debug)]
struct StubError;

impl Points for StubHooks {
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

/// A [`Points`] stub identical to [`StubHooks`] except its
/// `can_execute_transaction` always rejects, exercising the [`Error::Hook`]
/// surface in [`State::tx_to_op`].
struct RejectingHooks {
    target: Gas,
    config: GasPriceConfig,
}

impl RejectingHooks {
    fn new(target: u64) -> Self {
        Self {
            target: Gas(target),
            config: GasPriceConfig::default(),
        }
    }
}

impl Points for RejectingHooks {
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
        Err(StubError)
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

fn header(parent: B256, number: u64, time: u64) -> SealedHeader {
    SealedHeader::seal_slow(Header {
        parent_hash: parent,
        number,
        timestamp: time,
        ..Header::default()
    })
}

/// Builds a [`State`] rooted at a synthetic settled-block hash, with the given
/// clock target/excess and after-block gas target.
fn new_state(after_target: u64, clock_target: u64, clock_excess: u64) -> (State<StubHooks>, B256) {
    let settled_hash = B256::repeat_byte(0xab);
    let state = State::new(
        StubHooks::new(after_target),
        clock(clock_target, clock_excess),
        settled_hash,
    );
    (state, settled_hash)
}

/// Builds an [`Op`] with a single burn entry for `from` at `nonce`, with the
/// given gas and fee cap and a `min_balance`/`amount` of zero (so `apply_to`'s
/// balance checks always pass — these fixtures target the pre-`apply_to`
/// gas/fee-cap/nonce branches of [`State::apply`]).
fn burn_op(from: Address, nonce: u64, gas: u64, gas_fee_cap: U256) -> Op {
    let mut burn = BTreeMap::new();
    burn.insert(
        from,
        AccountDebit {
            nonce,
            amount: U256::ZERO,
            min_balance: U256::ZERO,
        },
    );
    Op {
        id: ava_types::id::Id::EMPTY,
        gas: Gas(gas),
        gas_fee_cap,
        burn,
        mint: BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Step-1 Red tests
// ---------------------------------------------------------------------------

#[test]
fn max_block_gas_is_r_tau_lambda() {
    // Omega_B = min(R, maxSafeRate) * (TauSeconds * Lambda) = R * 10 here.
    let c = clock(INITIAL_GAS_TARGET, INITIAL_EXCESS);
    let r = c.rate();
    assert_eq!(
        r,
        2 * INITIAL_GAS_TARGET,
        "rate R = TARGET_TO_RATE * target"
    );
    assert_eq!(
        safe_max_block_size(&c),
        INITIAL_MAX_BLOCK_SIZE,
        "Omega_B = R * (TauSeconds * Lambda)"
    );
    assert_eq!(safe_max_block_size(&c), r * 10);

    // Capping: a clock whose rate would overflow a closed queue is capped at
    // maxSafeRate so MaxFullBlocksInClosedQueue blocks still fit in u64.
    let cap = clock(u64::MAX / 2, 0); // rate saturates near u64::MAX
    let omega = safe_max_block_size(&cap);
    // 3 full blocks (closed queue) must not overflow u64.
    assert!(omega.checked_mul(3).is_some(), "closed queue must fit u64");
}

#[test]
fn min_gas_consumption_ceil() {
    // min consumption = max(gas_used, ceil(gas_limit / Lambda)); Lambda = 2.
    assert_eq!(ava_saevm_worstcase::minimum_gas_consumption(0), 0);
    assert_eq!(ava_saevm_worstcase::minimum_gas_consumption(1), 1); // ceil(1/2)
    assert_eq!(ava_saevm_worstcase::minimum_gas_consumption(4), 2);
    assert_eq!(ava_saevm_worstcase::minimum_gas_consumption(5), 3); // ceil(5/2)
}

#[test]
fn worst_case_affordability_mul_add() {
    // mul_add(gas, fee_cap, value) = gas * fee_cap + value.
    let got = mul_add(100_000, U256::from(2u64), U256::from(123_456u64));
    assert_eq!(got, Some(U256::from(100_000u64 * 2 + 123_456)));

    // Overflow yields None.
    assert_eq!(mul_add(2, U256::MAX, U256::ZERO), None);
    assert_eq!(mul_add(1, U256::MAX, U256::from(1u64)), None);
}

#[test]
fn check_base_fee_bound_rejects_above_max() {
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("start_block");
    let bounds = state.finish_block().expect("finish_block");

    // The max base fee is 1 here; the realised base fee must not exceed it.
    let max = bounds.max_base_fee;
    assert!(ava_saevm_worstcase::check_base_fee_bound(&bounds, max).is_ok());
    let too_high = Price(max.0 + 1);
    assert!(matches!(
        ava_saevm_worstcase::check_base_fee_bound(&bounds, too_high),
        Err(Error::BaseFeeBoundExceeded { .. })
    ));
}

#[test]
fn err_queue_full_when_open_queue_exceeds_2_omega_b() {
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    let mut last = settled;

    // Fill the open queue with the minimum gas to prevent additional blocks:
    // two full blocks then one extra unit (MaxFullBlocksInOpenQueue = 2).
    for (number, gas) in [INITIAL_MAX_BLOCK_SIZE, INITIAL_MAX_BLOCK_SIZE, 1]
        .into_iter()
        .enumerate()
    {
        let h = header(last, number as u64, 0);
        state.start_block(&h).expect("start_block");
        let mut s = MemState::default();
        state
            .apply(
                &Op {
                    id: ava_types::id::Id::EMPTY,
                    gas: Gas(gas),
                    gas_fee_cap: U256::from(2u64),
                    burn: BTreeMap::new(),
                    mint: BTreeMap::new(),
                },
                &mut s,
            )
            .expect("apply");
        state.finish_block().expect("finish_block");
        last = h.hash();
    }

    let err = state.start_block(&header(last, 3, 0)).unwrap_err();
    assert!(matches!(err, Error::QueueFull { .. }), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Additional faithful-port coverage
// ---------------------------------------------------------------------------

#[test]
fn non_consecutive_blocks_rejected() {
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("first start_block");
    // Reusing the genesis parent hash (rather than the prior header's hash) is
    // non-consecutive.
    let err = state.start_block(&header(settled, 1, 0)).unwrap_err();
    assert!(
        matches!(err, Error::NonConsecutiveBlocks { .. }),
        "got {err:?}"
    );
}

#[test]
fn apply_charges_min_balance_and_records_snapshot() {
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("start_block");

    let from = Address::repeat_byte(0x01);
    let start_balance = U256::from(1_000_000u64);
    let mut s = MemState::with_balance(from, start_balance);

    // affordability: gas (21000) * fee_cap (2) + value (0) = 42000 <= balance.
    // Exercise the full `tx_to_op` path (incl. the can_execute_transaction hook).
    let base_fee = state.base_fee();
    let op = state
        .tx_to_op(
            from,
            21_000,
            U256::from(2u64),
            U256::ZERO,
            U256::ZERO,
            base_fee,
            Some(Address::ZERO),
            &s,
        )
        .expect("tx_to_op");
    state.apply(&op, &mut s).expect("apply");

    let bounds = state.finish_block().expect("finish_block");
    assert_eq!(bounds.min_op_burner_balances.len(), 1);
    assert_eq!(
        bounds.min_op_burner_balances[0].get(&from).copied(),
        Some(start_balance),
        "snapshot taken before apply_to"
    );
}

#[test]
fn apply_rejects_gas_above_block_limit() {
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("start_block");
    let mut s = MemState::default();
    let err = state
        .apply(
            &Op {
                id: ava_types::id::Id::EMPTY,
                gas: Gas(INITIAL_MAX_BLOCK_SIZE + 1),
                gas_fee_cap: U256::from(1u64),
                burn: BTreeMap::new(),
                mint: BTreeMap::new(),
            },
            &mut s,
        )
        .unwrap_err();
    assert!(matches!(err, Error::GasLimitReached), "got {err:?}");
}

#[test]
fn apply_rejects_fee_cap_below_base_fee() {
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("start_block");
    let mut s = MemState::default();
    let err = state
        .apply(
            &Op {
                id: ava_types::id::Id::EMPTY,
                gas: Gas(21_000),
                gas_fee_cap: U256::ZERO, // base fee is 1
                burn: BTreeMap::new(),
                mint: BTreeMap::new(),
            },
            &mut s,
        )
        .unwrap_err();
    assert!(matches!(err, Error::FeeCapTooLow), "got {err:?}");
}

#[test]
fn check_sender_balance_bound_detects_below() {
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("start_block");
    let from = Address::repeat_byte(0x02);
    let mut s = MemState::with_balance(from, U256::from(1_000_000u64));
    let base_fee = state.base_fee();
    let op = State::<StubHooks>::tx_to_op_inner(
        from,
        21_000,
        U256::from(2u64),
        U256::ZERO,
        U256::ZERO,
        base_fee,
    )
    .expect("tx_to_op_inner");
    state.apply(&op, &mut s).expect("apply");
    let bounds = state.finish_block().expect("finish_block");

    // Op 0 recorded a min balance of 1_000_000. The realised balance must be
    // >= that bound.
    let mut realised = MemState::with_balance(from, U256::from(1_000_000u64));
    assert!(ava_saevm_worstcase::check_sender_balance_bound(&bounds, 0, &realised).is_ok());

    realised = MemState::with_balance(from, U256::from(999_999u64));
    assert!(matches!(
        ava_saevm_worstcase::check_sender_balance_bound(&bounds, 0, &realised),
        Err(Error::SenderBalanceBelowBound { .. })
    ));
}

// ---------------------------------------------------------------------------
// Error-branch coverage (ports of Go `Apply`'s nonce switch + `txToOp`)
// ---------------------------------------------------------------------------

#[test]
fn apply_rejects_nonce_below_state() {
    // Go `Apply`: `case nonce < next: ErrNonceTooLow`.
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("start_block");
    let from = Address::repeat_byte(0x03);
    let mut s = MemState::default();
    s.set_nonce(from, 5);
    // op nonce 4 < state nonce 5.
    let err = state
        .apply(&burn_op(from, 4, 21_000, U256::from(2u64)), &mut s)
        .unwrap_err();
    assert!(
        matches!(err, Error::NonceTooLow { got: 4, want: 5 }),
        "got {err:?}"
    );
}

#[test]
fn apply_rejects_nonce_above_state() {
    // Go `Apply`: `case nonce > next: ErrNonceTooHigh`.
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("start_block");
    let from = Address::repeat_byte(0x04);
    let mut s = MemState::default();
    s.set_nonce(from, 2);
    // op nonce 7 > state nonce 2.
    let err = state
        .apply(&burn_op(from, 7, 21_000, U256::from(2u64)), &mut s)
        .unwrap_err();
    assert!(
        matches!(err, Error::NonceTooHigh { got: 7, want: 2 }),
        "got {err:?}"
    );
}

#[test]
fn apply_rejects_nonce_at_max() {
    // Go `Apply`: `case next == math.MaxUint64: ErrNonceMax`. The op nonce must
    // equal `next` so the TooLow/TooHigh arms fall through to this one.
    let (mut state, settled) = new_state(INITIAL_GAS_TARGET, INITIAL_GAS_TARGET, INITIAL_EXCESS);
    state
        .start_block(&header(settled, 0, 0))
        .expect("start_block");
    let from = Address::repeat_byte(0x05);
    let mut s = MemState::default();
    s.set_nonce(from, u64::MAX);
    let err = state
        .apply(&burn_op(from, u64::MAX, 21_000, U256::from(2u64)), &mut s)
        .unwrap_err();
    assert!(matches!(err, Error::NonceMax), "got {err:?}");
}

#[test]
fn tx_to_op_inner_rejects_cost_overflow() {
    // Go `txToOp`: `mulAdd(gas, gasFeeCap, value)` overflow → errCostOverflow.
    // Exercise it THROUGH tx_to_op_inner: gas * fee_cap already saturates U256,
    // so adding value overflows on the first mul_add.
    let from = Address::repeat_byte(0x06);
    let base_fee = Price(1);
    let err = State::<StubHooks>::tx_to_op_inner(
        from,
        2,          // gas
        U256::MAX,  // fee_cap: 2 * U256::MAX overflows
        U256::ZERO, // gas_tip_cap
        U256::ZERO, // value
        base_fee,
    )
    .unwrap_err();
    assert!(matches!(err, Error::CostOverflow), "got {err:?}");
}

#[test]
fn tx_to_op_surfaces_hook_rejection() {
    // Go `ApplyTx`/`txToOp` path: a `CanExecuteTransaction` hook error is
    // surfaced as Error::Hook before the op is built.
    let settled_hash = B256::repeat_byte(0xab);
    let state = State::new(
        RejectingHooks::new(INITIAL_GAS_TARGET),
        clock(INITIAL_GAS_TARGET, INITIAL_EXCESS),
        settled_hash,
    );
    let from = Address::repeat_byte(0x07);
    let s = MemState::with_balance(from, U256::from(1_000_000u64));
    let err = state
        .tx_to_op(
            from,
            21_000,
            U256::from(2u64),
            U256::ZERO,
            U256::ZERO,
            state.base_fee(),
            Some(Address::ZERO),
            &s,
        )
        .unwrap_err();
    assert!(matches!(err, Error::Hook(_)), "got {err:?}");
}
