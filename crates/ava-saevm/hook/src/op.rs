// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The mint/burn [`Op`] applied to state during block execution, and the
//! object-safe [`StateMut`] seam abstracting state mutation (specs/11 §9.1).
//!
//! Faithful port of `vms/saevm/hook/hook.go::{Op, AccountDebit, ApplyTo}`. The
//! real revm-backed [`StateMut`] implementation lands in M7.14; here we define
//! the trait plus the pure-data [`Op`].

use std::collections::BTreeMap;

use ava_saevm_types::{Address, U256};
use ava_vm::components::gas::Gas;

use crate::Transaction;

/// An object-safe abstraction of the state mutations an [`Op`] performs.
///
/// Used so [`Op::apply_to`] is testable without a full revm `StateDB`. The
/// real revm-backed implementation lands in M7.14.
pub trait StateMut {
    /// Returns the balance of `a`.
    fn balance(&self, a: Address) -> U256;
    /// Returns the nonce of `a`.
    fn nonce(&self, a: Address) -> u64;
    /// Sets the nonce of `a` to `n`.
    fn set_nonce(&mut self, a: Address, n: u64);
    /// Subtracts `amount` from the balance of `a`.
    fn sub_balance(&mut self, a: Address, amount: U256);
    /// Adds `amount` to the balance of `a`.
    fn add_balance(&mut self, a: Address, amount: U256);
}

/// An amount that an account should have debited, along with the nonce used to
/// authorize the debit.
///
/// Port of Go's `hook.AccountDebit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountDebit {
    /// Nonce used to authorize the debit.
    pub nonce: u64,
    /// Amount to deduct from the account balance.
    pub amount: U256,
    /// Minimum balance the account must have for the operation to be valid. It
    /// MUST be at least [`AccountDebit::amount`].
    pub min_balance: U256,
}

/// Errors returned by [`Op::apply_to`].
///
/// Port of Go's `errMinBalanceBelowAmount` / `core.ErrInsufficientFunds`.
#[derive(Debug, thiserror::Error)]
pub enum OpError {
    /// An account's minimum balance is below the amount to debit.
    #[error("minimum balance below amount to debit")]
    MinBalanceBelowAmount,
    /// An account has insufficient funds for the debit.
    #[error("insufficient funds")]
    InsufficientFunds,
}

/// An operation that can be applied to state during the execution of a block.
///
/// Port of Go's `hook.Op`. Uses [`BTreeMap`] (not `HashMap`) so iteration order
/// is deterministic, as required for consensus paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    /// ID of this operation, used for logging and debugging.
    pub id: ava_types::id::Id,
    /// Gas consumed by this operation.
    pub gas: Gas,
    /// The maximum gas price this operation is willing to pay.
    pub gas_fee_cap: U256,
    /// Amount to decrease account balances by, with the nonce authorizing the
    /// debit.
    pub burn: BTreeMap<Address, AccountDebit>,
    /// Amount to increase account balances by. These funds are not necessarily
    /// tied to the funds consumed in [`Op::burn`]; the sum of mints may exceed
    /// the sum of burns.
    pub mint: BTreeMap<Address, U256>,
}

impl Op {
    /// Returns an empty [`Op`] (zero id/gas/fee-cap, no burns or mints).
    ///
    /// Convenience for construction; mirrors a zero-valued Go `Op`.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            id: ava_types::id::Id::EMPTY,
            gas: Gas(0),
            gas_fee_cap: U256::ZERO,
            burn: BTreeMap::new(),
            mint: BTreeMap::new(),
        }
    }

    /// Applies the operation to `state`.
    ///
    /// If any account has insufficient funds, [`OpError::InsufficientFunds`] is
    /// returned and `state` is unchanged (all checks run before any mutation).
    /// If any `min_balance < amount`, [`OpError::MinBalanceBelowAmount`] is
    /// returned (likewise before any mutation).
    ///
    /// Faithful port of Go's `Op.ApplyTo`.
    ///
    /// # Errors
    ///
    /// Returns [`OpError`] if a burn is invalid (see above), leaving `state`
    /// unchanged.
    pub fn apply_to(&self, state: &mut dyn StateMut) -> Result<(), OpError> {
        // First pass: validate every burn before mutating anything.
        for (from, acc) in &self.burn {
            if acc.min_balance < acc.amount {
                return Err(OpError::MinBalanceBelowAmount);
            }
            if state.balance(*from) < acc.min_balance {
                return Err(OpError::InsufficientFunds);
            }
        }
        // Second pass: apply burns. The state is the source of truth for the
        // current nonce (not the hook-provided value); this protects against
        // delegated-account replay. If incrementing would overflow, the nonce
        // was already bumped by a delegated account's execution, so we leave it.
        for (from, acc) in &self.burn {
            let nonce = state.nonce(*from);
            if let Some(next) = nonce.checked_add(1) {
                state.set_nonce(*from, next);
            }
            state.sub_balance(*from, acc.amount);
        }
        // Third pass: apply mints.
        for (to, amount) in &self.mint {
            state.add_balance(*to, *amount);
        }
        Ok(())
    }
}

impl Transaction for Op {
    fn as_op(&self) -> Op {
        self.clone()
    }
}
