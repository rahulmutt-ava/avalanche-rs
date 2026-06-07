// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/platformvm/stakeable` — time-locked outputs/inputs (specs 08 §2.1).
//!
//! A [`LockOut`] / [`LockIn`] wraps an inner fx output/input with a `Locktime`
//! before which the value cannot be spent as unlocked. On the wire they are
//! registered fx interface types (`stakeable.LockOut` = type_id 22,
//! `stakeable.LockIn` = type_id 21): the `Locktime` `u64` followed by the inner
//! interface payload (which carries its **own** typeID).
//!
//! The inner payload is the [`crate::txs::components::Output`] /
//! [`crate::txs::components::Input`] interface enum (boxed to break the
//! type recursion). Go forbids nesting a stakeable lock inside another; that
//! rule is enforced in [`LockOut::verify`] / [`LockIn::verify`].

use ava_codec::AvaCodec;

use crate::Error;

/// `stakeable.LockOut` (type_id 22) — a time-locked fx output.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct LockOut {
    /// `Locktime` — unix seconds before which the wrapped output is locked.
    #[codec]
    pub locktime: u64,
    /// The wrapped fx output (`avax.TransferableOut` interface).
    #[codec]
    pub transferable_out: Box<crate::txs::components::Output>,
}

impl LockOut {
    /// Builds a [`LockOut`].
    #[must_use]
    pub fn new(locktime: u64, transferable_out: crate::txs::components::Output) -> Self {
        Self {
            locktime,
            transferable_out: Box::new(transferable_out),
        }
    }

    /// `Amount()` — the wrapped output's amount.
    #[must_use]
    pub fn amount(&self) -> u64 {
        self.transferable_out.amount()
    }

    /// `LockOut.Verify` — non-zero locktime, no nested stakeable lock, then the
    /// wrapped output's verification (specs 08 §2.1).
    ///
    /// # Errors
    /// Returns [`Error::InvalidLocktime`] / [`Error::NestedStakeableLock`], else
    /// propagates the wrapped output's `verify`.
    pub fn verify(&self) -> Result<(), Error> {
        if self.locktime == 0 {
            return Err(Error::InvalidLocktime);
        }
        if matches!(
            *self.transferable_out,
            crate::txs::components::Output::StakeableLock(_)
        ) {
            return Err(Error::NestedStakeableLock);
        }
        self.transferable_out.verify()
    }
}

/// `stakeable.LockIn` (type_id 21) — a time-locked fx input.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct LockIn {
    /// `Locktime` — unix seconds before which the wrapped input is locked.
    #[codec]
    pub locktime: u64,
    /// The wrapped fx input (`avax.TransferableIn` interface).
    #[codec]
    pub transferable_in: Box<crate::txs::components::Input>,
}

impl LockIn {
    /// Builds a [`LockIn`].
    #[must_use]
    pub fn new(locktime: u64, transferable_in: crate::txs::components::Input) -> Self {
        Self {
            locktime,
            transferable_in: Box::new(transferable_in),
        }
    }

    /// `Amount()` — the wrapped input's amount.
    #[must_use]
    pub fn amount(&self) -> u64 {
        self.transferable_in.amount()
    }

    /// `LockIn.Verify` — non-zero locktime, no nested stakeable lock, then the
    /// wrapped input's verification.
    ///
    /// # Errors
    /// Returns [`Error::InvalidLocktime`] / [`Error::NestedStakeableLock`], else
    /// propagates the wrapped input's `verify`.
    pub fn verify(&self) -> Result<(), Error> {
        if self.locktime == 0 {
            return Err(Error::InvalidLocktime);
        }
        if matches!(
            *self.transferable_in,
            crate::txs::components::Input::StakeableLock(_)
        ) {
            return Err(Error::NestedStakeableLock);
        }
        self.transferable_in.verify()
    }
}
