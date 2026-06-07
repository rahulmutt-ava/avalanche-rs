// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The secp256k1fx adapter — the base fx for the X-Chain (specs 09 §4.1; 07
//! §4.3).
//!
//! A thin wrapper over [`ava_secp256k1fx::Fx`] mapping Go's `VerifyTransfer` /
//! `VerifyOperation` (`vms/secp256k1fx/fx.go`) onto the avm error model. The
//! wrapped [`ava_secp256k1fx::Fx`] owns the shared recover-cache and the
//! `bootstrapped` flag (one instance per VM, 09 §4.1); the spend gate
//! ([`ava_secp256k1fx::Fx::verify_credentials`]) does the signature recovery and
//! the threshold/locktime checks.
//!
//! Parity notes vs Go:
//! * `VerifyTransfer` (`VerifySpend`): checks `utxo.amt == in.amt` (else
//!   `ErrMismatchedAmounts`) then `VerifyCredentials(tx, &in.Input, cred,
//!   &utxo.OutputOwners)`.
//! * `verifyOperation`: requires the produced mint output's owners to equal the
//!   consumed `MintOutput` UTXO's owners (else `ErrWrongMintCreated`) then
//!   `VerifyCredentials(tx, &op.MintInput, cred, &utxo.OutputOwners)`.
//! * The `bootstrapped` gate lives inside `verify_credentials`: while the fx is
//!   not bootstrapped, it returns `Ok(())` after the structural checks. Go skips
//!   the structural checks too during bootstrap by never reaching them with
//!   garbage data in practice; to match the `!bootstrapped ⇒ Ok(())` contract
//!   exactly (specs 09 §4, M5.6) we short-circuit before the avm-side amount /
//!   owners checks as well.

use std::sync::Arc;

use ava_secp256k1fx::{
    Credential, Fx as Secp256k1Fx, Input, MintOutput, TransferInput, TransferOutput,
};
use ava_utils::clock::Clock;
use ava_vm::fx::UnsignedTx;

use crate::error::{Error, Result};
use crate::fx::Fx;

/// `secp256k1fx.Fx` — the X-Chain base fx adapter.
///
/// Wraps one [`ava_secp256k1fx::Fx`] (sharing its recover-cache and
/// `bootstrapped` flag); construct via [`SecpFx::new`] with the VM clock.
pub struct SecpFx {
    inner: Secp256k1Fx,
}

impl SecpFx {
    /// Builds a [`SecpFx`] reading time through `clock`; signature verification
    /// starts **disabled** (still bootstrapping), matching Go.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Secp256k1Fx::new(clock),
        }
    }

    /// `Bootstrapping()` — no-op (verification already disabled).
    pub fn bootstrapping(&mut self) {
        self.inner.bootstrapping();
    }

    /// `Bootstrapped()` — enables signature verification.
    pub fn bootstrapped(&mut self) {
        self.inner.bootstrapped();
    }

    /// Whether signature verification is enabled.
    #[must_use]
    pub fn is_bootstrapped(&self) -> bool {
        self.inner.is_bootstrapped()
    }
}

impl Fx for SecpFx {
    fn verify_transfer(
        &self,
        tx: &dyn UnsignedTx,
        input: &TransferInput,
        cred: &Credential,
        utxo: &TransferOutput,
    ) -> Result<()> {
        // Bootstrap skip (Go `!bootstrapped ⇒ nil`): no verification while
        // replaying historical blocks.
        if !self.inner.is_bootstrapped() {
            return Ok(());
        }
        // `VerifySpend`: amount equality first (Go `utxo.Amt != in.Amt`).
        if utxo.amt != input.amt {
            return Err(Error::MismatchedAmounts);
        }
        self.inner
            .verify_credentials(tx, &input.input, cred, &utxo.owners)
            .map_err(Error::Fx)
    }

    fn verify_operation(
        &self,
        tx: &dyn UnsignedTx,
        mint_input: &Input,
        mint_output: &MintOutput,
        cred: &Credential,
        utxo: &MintOutput,
    ) -> Result<()> {
        // Bootstrap skip (Go `!bootstrapped ⇒ nil`).
        if !self.inner.is_bootstrapped() {
            return Ok(());
        }
        // `verifyOperation`: the produced mint output's owners must equal the
        // consumed `MintOutput` UTXO's owners (Go `!utxo.Equals(&op.MintOutput…)`).
        if !utxo.owners.equals(&mint_output.owners) {
            return Err(Error::WrongMintCreated);
        }
        self.inner
            .verify_credentials(tx, mint_input, cred, &utxo.owners)
            .map_err(Error::Fx)
    }
}
