// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The worst-case replay [`State`] (specs/11 §9.3/§2.4/§6.1).
//!
//! Faithful port of `vms/saevm/worstcase/state.go`. [`State`] tracks the
//! worst-case gas clock and account state as operations are replayed, producing
//! the [`WorstCaseBounds`] attached to a block before execution.
//!
//! # State seam
//!
//! Following the M7.9/M7.12 precedent, the mutable execution state is abstracted
//! behind the object-safe [`StateMut`] seam (passed into [`State::apply`]) rather
//! than a concrete revm/Firewood `StateDB`, so the replay logic is testable
//! without a live EVM. The Go reference holds a `*state.StateDB`; here `apply`
//! receives a `&mut dyn StateMut` per call. The concrete revm-backed `StateMut`
//! plus the `saedb`-opener-driven [`State::new`] (opening state at the settled
//! root, deriving the clock from `settled.executed_by_gas_time()`) wire in
//! M7.14.
//!
//! # Deferred to M7.14
//!
//! The reth-tx-specific `ApplyTx` path — the libevm `txpool.ValidateTransaction`
//! intrinsic-gas validation, `types.Sender` signer recovery, and the
//! `GetCodeHash` EOA check (`core.ErrSenderNoEOA`) — needs a concrete reth signed
//! transaction and a `code_hash` state accessor that does not yet exist on the
//! [`StateMut`]/[`StateRead`] seam. Those are deferred to M7.14. The parts that
//! do **not** need a concrete signed transaction are implemented here:
//! [`State::tx_to_op`] (the affordability `mul_add` math + `can_execute_transaction`
//! hook call) and [`State::apply`] (the Go `Apply`).

use ava_saevm_blocks::WorstCaseBounds;
use ava_saevm_gastime::GasTime;
use ava_saevm_hook::op::{AccountDebit, Op, StateMut};
use ava_saevm_hook::{Points, StateRead};
use ava_saevm_types::{Address, B256, SealedHeader, U256};
use ava_vm::components::gas::{Gas, Price};

use crate::{Error, Result, mul_add};

/// `TauSeconds * Lambda` — the number of gas-seconds in a full block. Mirrors
/// Go's `maxGasSecondsPerBlock`.
const MAX_GAS_SECONDS_PER_BLOCK: u64 = ava_saevm_params::TAU_SECONDS * ava_saevm_params::LAMBDA;

/// Returns the maximum block size (the `Ω_B` of specs/11 §2.4) for the clock's
/// rate, capping it so a full closed queue still fits in a `u64`.
///
/// `Ω_B = min(R, maxSafeRate) * (TauSeconds * Lambda)` where
/// `maxSafeRate = u64::MAX / (MaxFullBlocksInClosedQueue * TauSeconds * Lambda)`.
/// At the time of writing the cap is ~6e17, so capping is exceedingly unlikely.
///
/// Faithful port of Go's `safeMaxBlockSize`.
#[must_use]
pub fn safe_max_block_size(clock: &GasTime) -> u64 {
    // const denominators (compile-time non-zero); wrapping_* only to satisfy
    // arithmetic_side_effects on provably-safe constant folding.
    const MAX_GAS_SECONDS_IN_CLOSED_QUEUE: u64 =
        ava_saevm_params::MAX_FULL_BLOCKS_IN_CLOSED_QUEUE.wrapping_mul(MAX_GAS_SECONDS_PER_BLOCK);
    // maxGasInClosedQueue = u64::MAX.
    const MAX_SAFE_RATE: u64 = u64::MAX / MAX_GAS_SECONDS_IN_CLOSED_QUEUE;

    let rate = clock.rate().min(MAX_SAFE_RATE);
    // rate <= MAX_SAFE_RATE so rate * MAX_GAS_SECONDS_PER_BLOCK <= u64::MAX:
    // saturating_mul is exact here and lint-clean.
    rate.saturating_mul(MAX_GAS_SECONDS_PER_BLOCK)
}

/// Tracks the worst-case gas price and account state as operations are replayed.
///
/// Usage MUST follow the pattern:
///  1. [`State::start_block`] for each block to be included.
///  2. [`State::gas_limit`] / [`State::base_fee`] to query the block parameters.
///  3. [`State::apply`] (or [`State::tx_to_op`] then [`State::apply`]) for each
///     [`Op`] to include in the block.
///  4. [`State::gas_used`] to query the total gas used in the block.
///  5. [`State::finish_block`] to finalise the block's gas time and obtain the
///     [`WorstCaseBounds`].
///  6. Repeat from step 1 for the next block.
///
/// Faithful port of Go's `worstcase.State`.
pub struct State<H: Points> {
    hooks: H,
    clock: GasTime,
    /// Sanity-checks that blocks are provided in order. The `curr` header is
    /// modified to reflect worst-case bounds, so its hash can't be used; the
    /// expected parent hash is tracked separately.
    expected_parent_hash: B256,

    q_size: u64,
    block_size: u64,
    max_block_size: u64,

    base_fee: Price,
    /// The current block's header (number/time are read by `gas_config_after` /
    /// `block_time`). `None` before the first `start_block`.
    curr: Option<SealedHeader>,
    min_op_burner_balances: Vec<std::collections::BTreeMap<Address, U256>>,
}

impl<H: Points> State<H> {
    /// Constructs a new worst-case state rooted at the settled block.
    ///
    /// `clock` is the settled block's execution gas clock and
    /// `expected_parent_hash` the settled block's hash (the first
    /// [`State::start_block`] header must point at it).
    ///
    /// The `saedb`-opener-driven constructor (opening state at the settled root
    /// and deriving the clock from `settled.executed_by_gas_time()`, returning
    /// [`Error::SettledBlockNotExecuted`] when the settled block has not
    /// executed) wires in M7.14; see the module docs.
    #[must_use]
    pub fn new(hooks: H, clock: GasTime, expected_parent_hash: B256) -> Self {
        Self {
            hooks,
            clock,
            expected_parent_hash,
            q_size: 0,
            block_size: 0,
            max_block_size: 0,
            base_fee: Price(0),
            curr: None,
            min_op_burner_balances: Vec::new(),
        }
    }

    /// Updates the worst-case state to the beginning of the provided block.
    ///
    /// `GasLimit`/`BaseFee` need not be set on the header, but all other fields
    /// should be populated and `parent_hash` MUST match the previous block's
    /// hash.
    ///
    /// Faithful port of Go's `State.StartBlock`.
    ///
    /// # Errors
    ///
    /// [`Error::NonConsecutiveBlocks`] if `parent_hash` does not match the
    /// expected hash; [`Error::QueueFull`] if the queue is too full to accept
    /// another block.
    pub fn start_block(&mut self, h: &SealedHeader) -> Result<()> {
        let parent = h.parent_hash;
        if parent != self.expected_parent_hash {
            return Err(Error::NonConsecutiveBlocks {
                expected: self.expected_parent_hash,
                got: parent,
            });
        }

        let (secs, nanos) = self.hooks.block_time(h);
        self.clock.before_block(secs, nanos);
        self.block_size = 0;

        self.max_block_size = safe_max_block_size(&self.clock);
        let max_open_q_size =
            ava_saevm_params::MAX_FULL_BLOCKS_IN_OPEN_QUEUE.saturating_mul(self.max_block_size);
        if self.q_size > max_open_q_size {
            return Err(Error::QueueFull {
                size: self.q_size,
                max: max_open_q_size,
            });
        }

        self.base_fee = self.clock.price();
        // `finish_block` returns a clone, so the backing alloc can be reused.
        self.min_op_burner_balances.clear();

        // expectedParentHash is updated prior to (notionally) modifying the
        // GasLimit/BaseFee so historical block hashes are not modified.
        self.expected_parent_hash = h.hash();
        self.curr = Some(h.clone());
        Ok(())
    }

    /// Returns the available gas limit for the current block (`Ω_B`).
    #[must_use]
    pub fn gas_limit(&self) -> u64 {
        self.max_block_size
    }

    /// Returns the worst-case base fee for the current block.
    #[must_use]
    pub fn base_fee(&self) -> Price {
        self.base_fee
    }

    /// Returns the gas used for the current block.
    #[must_use]
    pub fn gas_used(&self) -> u64 {
        self.block_size
    }

    /// Converts the affordability-relevant fields of a transaction into an
    /// [`Op`], computing the worst-case (`gas * gas_fee_cap + value`) and
    /// effective-price (`gas * min(gas_fee_cap, base_fee + gas_tip_cap) + value`)
    /// costs.
    ///
    /// `can_execute_transaction` is consulted via the hook before the op is
    /// built. The reth-tx intrinsic-gas validation, signer recovery, and EOA
    /// codehash check are deferred to M7.14 (see module docs).
    ///
    /// Faithful port of the non-reth-specific half of Go's `txToOp`.
    ///
    /// # Errors
    ///
    /// [`Error::CostOverflow`] if `gas * fee + value` overflows; [`Error::Hook`]
    /// if `can_execute_transaction` rejects the sender.
    #[allow(clippy::too_many_arguments)]
    pub fn tx_to_op(
        &self,
        from: Address,
        gas: u64,
        gas_fee_cap: U256,
        gas_tip_cap: U256,
        value: U256,
        base_fee: Price,
        to: Option<Address>,
        state: &dyn StateRead,
    ) -> Result<Op>
    where
        H::Error: std::fmt::Debug,
    {
        self.hooks
            .can_execute_transaction(from, to, state)
            .map_err(|e| Error::Hook(format!("{e:?}")))?;
        Self::tx_to_op_inner(from, gas, gas_fee_cap, gas_tip_cap, value, base_fee)
    }

    /// The pure affordability-math half of [`State::tx_to_op`], without the
    /// `can_execute_transaction` hook (so it is callable in unit tests over the
    /// state seam alone). The reth-tx glue is deferred to M7.14.
    ///
    /// # Errors
    ///
    /// [`Error::CostOverflow`] if `gas * fee + value` overflows.
    pub fn tx_to_op_inner(
        from: Address,
        gas: u64,
        gas_fee_cap: U256,
        gas_tip_cap: U256,
        value: U256,
        base_fee: Price,
    ) -> Result<Op> {
        let min_balance = mul_add(gas, gas_fee_cap, value).ok_or(Error::CostOverflow)?;

        // effective_gas_price = min(gas_fee_cap, base_fee + gas_tip_cap).
        let base = U256::from(base_fee.0);
        let effective_gas_price = match base.checked_add(gas_tip_cap) {
            Some(sum) if sum <= gas_fee_cap => sum,
            _ => gas_fee_cap,
        };
        let amount = mul_add(gas, effective_gas_price, value).ok_or(Error::CostOverflow)?;

        let mut burn = std::collections::BTreeMap::new();
        burn.insert(
            from,
            AccountDebit {
                nonce: 0,
                amount,
                min_balance,
            },
        );
        Ok(Op {
            id: ava_types::id::Id::EMPTY,
            gas: Gas(gas),
            gas_fee_cap,
            burn,
            // Mint MUST NOT be populated: the transaction may revert.
            mint: std::collections::BTreeMap::new(),
        })
    }

    /// Attempts to apply the operation to `state` under worst-case assumptions.
    ///
    /// On error the state is not modified. An operation is invalid if it
    /// consumes more gas than the block has available, specifies too low a gas
    /// price, is from an account with an incorrect/invalid nonce, or has
    /// insufficient balance.
    ///
    /// Faithful port of Go's `State.Apply`.
    ///
    /// # Errors
    ///
    /// [`Error::GasLimitReached`], [`Error::FeeCapTooLow`],
    /// [`Error::NonceTooLow`]/[`Error::NonceTooHigh`]/[`Error::NonceMax`], or
    /// [`Error::Op`] (insufficient funds / min-balance below amount).
    pub fn apply(&mut self, o: &Op, state: &mut dyn StateMut) -> Result<()> {
        let remaining = self.max_block_size.saturating_sub(self.block_size);
        if o.gas.0 > remaining {
            return Err(Error::GasLimitReached);
        }
        if o.gas_fee_cap < U256::from(self.base_fee.0) {
            return Err(Error::FeeCapTooLow);
        }

        // Snapshot each burner's balance BEFORE `apply_to`, mirroring the
        // executor check ordering.
        let mut burner_balances = std::collections::BTreeMap::new();
        for (from, ad) in &o.burn {
            let next = state.nonce(*from);
            if ad.nonce < next {
                return Err(Error::NonceTooLow {
                    got: ad.nonce,
                    want: next,
                });
            }
            if ad.nonce > next {
                return Err(Error::NonceTooHigh {
                    got: ad.nonce,
                    want: next,
                });
            }
            if next == u64::MAX {
                return Err(Error::NonceMax);
            }
            burner_balances.insert(*from, state.balance(*from));
        }

        o.apply_to(state).map_err(Error::Op)?;
        self.min_op_burner_balances.push(burner_balances);
        // o.gas.0 <= remaining = max_block_size - block_size, so this sum
        // cannot overflow u64.
        self.block_size = self.block_size.saturating_add(o.gas.0);
        Ok(())
    }

    /// Advances the [`GasTime`] in preparation for the next block and returns the
    /// [`WorstCaseBounds`] for the just-replayed block.
    ///
    /// The returned bounds assume every successfully-applied op was included,
    /// reflected in the indexing of the per-op burner balances.
    ///
    /// Faithful port of Go's `State.FinishBlock`.
    ///
    /// # Errors
    ///
    /// [`Error::NoCurrentBlock`] if called before any [`State::start_block`].
    pub fn finish_block(&mut self) -> Result<WorstCaseBounds> {
        let curr = self.curr.as_ref().ok_or(Error::NoCurrentBlock)?;
        let (target, gas_cfg) = self.hooks.gas_config_after(curr);
        self.clock.after_block(self.block_size, target.0, gas_cfg);
        self.q_size = self.q_size.saturating_add(self.block_size);
        Ok(WorstCaseBounds {
            max_base_fee: self.base_fee,
            latest_end_time: self.clock.clone(),
            min_op_burner_balances: self.min_op_burner_balances.clone(),
        })
    }
}
