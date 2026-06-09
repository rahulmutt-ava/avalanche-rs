// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-worstcase` — SAE predictive worst-case bounds plus base-fee and
//! sender-balance assertions and `ErrQueueFull` (specs/11 §9.3/§2.4/§6.1).
//!
//! [`State`] replays a settled→parent block history then a candidate block over
//! a [`StateMut`] seam, tracking the worst-case gas clock, base fee, per-op
//! min-balances, and the queue size `q`. [`State::finish_block`] produces the
//! [`WorstCaseBounds`] (defined in [`ava_saevm_blocks`]) attached to the block
//! before execution. During actual execution the executor (M7.14) asserts the
//! realised values stay within those bounds via [`check_base_fee_bound`] /
//! [`check_sender_balance_bound`]; a violation is test-fatal.
//!
//! Faithful port of `vms/saevm/worstcase/state.go`.
//!
//! # Deferred to M7.14
//!
//! The reth-tx `ApplyTx` glue (intrinsic-gas validation, signer recovery, EOA
//! codehash) and the `saedb`-opener-driven [`State::new`] are deferred to M7.14;
//! see [`state`] module docs. A `rayon` batch over the independent
//! affordability/sender checks (specs/11 §13.3, ordering unchanged) is a future
//! perf optimisation, intentionally not added here (determinism first).
//!
//! [`StateMut`]: ava_saevm_hook::op::StateMut

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]
#![deny(clippy::cast_possible_truncation)]
#![deny(clippy::cast_sign_loss)]
#![deny(clippy::cast_possible_wrap)]

pub mod state;

use ava_saevm_blocks::WorstCaseBounds;
use ava_saevm_hook::StateRead;
use ava_saevm_types::{Address, B256, U256};
use ava_vm::components::gas::Price;

pub use crate::state::{State, safe_max_block_size};

/// `minimum_gas_consumption(tx_limit) = ceil(tx_limit / Lambda)`.
///
/// Re-exported from [`ava_saevm_hook`] for the worst-case `max(gas_used,
/// ceil(gas_limit / Lambda))` computation (specs/11 §2.4).
#[must_use]
pub fn minimum_gas_consumption(tx_limit: u64) -> u64 {
    ava_saevm_hook::minimum_gas_consumption(tx_limit)
}

/// The result type for the worst-case crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Failure replaying worst-case state or asserting realised bounds.
///
/// Sentinel variants preserve the Go reference's named errors
/// (`errNonConsecutiveBlocks`, `ErrQueueFull`, `core.Err*`, etc.).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A block was provided whose `parent_hash` does not match the previous
    /// block's hash. Mirrors Go's `errNonConsecutiveBlocks`.
    #[error("non-consecutive blocks: expected parent hash {expected:#x} but was {got:#x}")]
    NonConsecutiveBlocks {
        /// The expected parent hash (the previous block's hash).
        expected: B256,
        /// The `parent_hash` declared by the provided header.
        got: B256,
    },
    /// The queue is too full to accept another block. Can be rectified by
    /// building a block later, once additional blocks settle and drain the
    /// queue. Mirrors Go's `ErrQueueFull`.
    #[error("queue exceeds gas threshold for new block: current size {size} exceeds maximum {max}")]
    QueueFull {
        /// The current queue size, in gas.
        size: u64,
        /// The maximum queue size that still accepts a new block, in gas.
        max: u64,
    },
    /// The operation consumes more gas than the block has available. Mirrors
    /// Go's `core.ErrGasLimitReached`.
    #[error("gas limit reached")]
    GasLimitReached,
    /// The operation's fee cap is below the worst-case base fee. Mirrors Go's
    /// `core.ErrFeeCapTooLow`.
    #[error("max fee per gas less than block base fee")]
    FeeCapTooLow,
    /// The operation is from an account with a nonce lower than expected.
    /// Mirrors Go's `core.ErrNonceTooLow`.
    #[error("nonce too low: {got} < {want}")]
    NonceTooLow {
        /// The nonce supplied by the operation.
        got: u64,
        /// The next expected nonce, from state.
        want: u64,
    },
    /// The operation is from an account with a nonce higher than expected.
    /// Mirrors Go's `core.ErrNonceTooHigh`.
    #[error("nonce too high: {got} > {want}")]
    NonceTooHigh {
        /// The nonce supplied by the operation.
        got: u64,
        /// The next expected nonce, from state.
        want: u64,
    },
    /// The account's nonce has reached its maximum. Mirrors Go's
    /// `core.ErrNonceMax`.
    #[error("nonce has max value")]
    NonceMax,
    /// `gas * fee + value` overflowed a [`U256`]. Mirrors Go's
    /// `errCostOverflow`.
    #[error("cost overflows U256")]
    CostOverflow,
    /// The realised base fee at execution exceeded the predicted worst-case
    /// maximum (specs/11 §6.1 `CheckBaseFeeBound`). Test-fatal.
    #[error("base fee {actual} exceeds worst-case bound {max}")]
    BaseFeeBoundExceeded {
        /// The realised base fee at execution.
        actual: u64,
        /// The predicted worst-case maximum base fee.
        max: u64,
    },
    /// A burner's realised balance was below the predicted worst-case minimum
    /// (specs/11 §6.1 `CheckSenderBalanceBound`). Test-fatal.
    #[error("sender {address:#x} balance {actual} below worst-case bound {min}")]
    SenderBalanceBelowBound {
        /// The burner whose balance fell short.
        address: Address,
        /// The realised balance at execution.
        actual: U256,
        /// The predicted worst-case minimum balance.
        min: U256,
    },
    /// [`State::finish_block`] was called before any [`State::start_block`].
    #[error("no current block: finish_block called before start_block")]
    NoCurrentBlock,
    /// The block marked for settling has not finished execution yet. Mirrors
    /// Go's `errSettledBlockNotExecuted` (raised by the M7.14 `saedb`-opener
    /// constructor).
    #[error("block marked for settling has not finished execution yet")]
    SettledBlockNotExecuted,
    /// An [`Op::apply_to`] failed (insufficient funds / min-balance below
    /// amount). Mirrors Go's `core.ErrInsufficientFunds`.
    ///
    /// [`Op::apply_to`]: ava_saevm_hook::op::Op::apply_to
    #[error("applying operation: {0}")]
    Op(#[from] ava_saevm_hook::op::OpError),
    /// The `can_execute_transaction` hook rejected the sender. Mirrors Go's
    /// `CanExecuteTransaction` block.
    #[error("transaction blocked by can_execute_transaction hook: {0}")]
    Hook(String),
}

/// Returns `a * b + c`, or `None` on [`U256`] overflow.
///
/// Faithful port of Go's `mulAdd` (`a*b + c` with `MulOverflow`/`AddOverflow`).
#[must_use]
pub fn mul_add(a: u64, b: U256, c: U256) -> Option<U256> {
    U256::from(a).checked_mul(b)?.checked_add(c)
}

/// Asserts that the realised `base_fee` at execution does not exceed the
/// predicted worst-case [`WorstCaseBounds::max_base_fee`] (specs/11 §6.1
/// `CheckBaseFeeBound`). A violation is test-fatal.
///
/// # Errors
///
/// [`Error::BaseFeeBoundExceeded`] if `base_fee > bounds.max_base_fee`.
pub fn check_base_fee_bound(bounds: &WorstCaseBounds, base_fee: Price) -> Result<()> {
    if base_fee > bounds.max_base_fee {
        return Err(Error::BaseFeeBoundExceeded {
            actual: base_fee.0,
            max: bounds.max_base_fee.0,
        });
    }
    Ok(())
}

/// Asserts that, for the op at `op_index`, every burner's realised balance in
/// `state` is at least the predicted worst-case minimum recorded in
/// [`WorstCaseBounds::min_op_burner_balances`] (specs/11 §6.1
/// `CheckSenderBalanceBound`). A violation is test-fatal.
///
/// `op_index` out of range is treated as "no recorded bound" (vacuously `Ok`),
/// matching the Go indexing contract where only applied ops are recorded.
///
/// # Errors
///
/// [`Error::SenderBalanceBelowBound`] for the first burner whose realised
/// balance is below its recorded minimum.
pub fn check_sender_balance_bound(
    bounds: &WorstCaseBounds,
    op_index: usize,
    state: &dyn StateRead,
) -> Result<()> {
    let Some(snapshot) = bounds.min_op_burner_balances.get(op_index) else {
        return Ok(());
    };
    for (addr, min) in snapshot {
        let actual = state.balance(*addr);
        if actual < *min {
            return Err(Error::SenderBalanceBelowBound {
                address: *addr,
                actual,
                min: *min,
            });
        }
    }
    Ok(())
}
