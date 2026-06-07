// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The nftfx operation-verification adapter (specs 09 §4.2; FX-AVM-1).
//!
//! A port of `vms/nftfx/fx.go`. nftfx authorizes **minting** and **transferring**
//! non-fungible tokens; it deliberately does **not** support a plain transfer
//! spend (`VerifyTransfer` returns `errCantTransfer`).
//!
//! The fx is intentionally *not* an impl of the secp-typed [`crate::fx::Fx`]
//! trait (that trait's `verify_operation` takes the concrete secp `Input` /
//! `MintOutput`; nftfx operates over its own typeIDs). Instead [`Fx`] exposes its
//! own inherent `verify_operation` / `verify_transfer` over the nftfx concrete
//! types, mirroring Go's `VerifyMintOperation` / `VerifyTransferOperation` /
//! `VerifyTransfer`. The unifying polymorphic dispatch across all three fxs is
//! M5.9's job.
//!
//! The actual signature/threshold spend gate is delegated to the shared
//! [`ava_secp256k1fx::Fx::verify_credentials`] (the nftfx `Credential`(14) embeds
//! a secp `Credential`), reusing one recover-cache and the `bootstrapped` flag
//! per VM (specs 09 §4.1). While not bootstrapped, `verify_credentials` returns
//! `Ok(())` after the structural checks — matching Go's bootstrap-replay skip.

use std::sync::Arc;

use ava_secp256k1fx::Fx as Secp256k1Fx;
use ava_utils::clock::Clock;
use ava_vm::components::verify::Verifiable;
use ava_vm::fx::UnsignedTx;

use crate::error::{Error, Result};
use crate::nftfx::types::{
    Credential, MintOperation, MintOutput, TransferOperation, TransferOutput,
};

/// An nftfx operation — either minting new NFTs or transferring an existing one
/// (`vms/nftfx/fx.go` `VerifyOperation`'s `opIntf` type switch).
#[derive(Debug, Clone)]
pub enum NftOperation {
    /// `*nftfx.MintOperation` (typeID 12).
    Mint(MintOperation),
    /// `*nftfx.TransferOperation` (typeID 13).
    Transfer(TransferOperation),
}

/// An nftfx UTXO output — the consumed output an operation spends
/// (`VerifyOperation`'s `utxoIntf` type assertion).
#[derive(Debug, Clone)]
pub enum NftOutput {
    /// `*nftfx.MintOutput` (typeID 10).
    Mint(MintOutput),
    /// `*nftfx.TransferOutput` (typeID 11).
    Transfer(TransferOutput),
}

/// `nftfx.Fx` — the Non-Fungible Token feature extension.
///
/// Wraps one [`ava_secp256k1fx::Fx`] (sharing its recover-cache and
/// `bootstrapped` flag); construct via [`Fx::new`] with the VM clock.
pub struct Fx {
    inner: Secp256k1Fx,
}

impl Fx {
    /// Builds an nftfx [`Fx`] reading time through `clock`; signature
    /// verification starts **disabled** (still bootstrapping), matching Go.
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

    /// `VerifyTransfer(...)` — nftfx never authorizes a plain transfer spend.
    ///
    /// # Errors
    /// Always returns [`Error::CantTransfer`] (Go `errCantTransfer`).
    #[allow(clippy::unused_self)]
    pub fn verify_transfer(
        &self,
        _tx: &dyn UnsignedTx,
        _input: &ava_secp256k1fx::types::Input,
        _cred: &Credential,
        _utxo: &TransferOutput,
    ) -> Result<()> {
        Err(Error::CantTransfer)
    }

    /// `VerifyOperation(tx, op, cred, utxo)` — routes to the mint/transfer check
    /// per the operation type (`vms/nftfx/fx.go` `VerifyOperation`).
    ///
    /// # Errors
    /// Returns [`Error::WrongUtxoType`] when the consumed UTXO's type does not
    /// match the operation, [`Error::WrongUniqueId`] on a `group_id` mismatch,
    /// [`Error::WrongBytes`] on a transfer `payload` mismatch, the structural
    /// [`Error::Fx`] from `verify::all`, or the spend-gate sentinel
    /// ([`Error::Fx`]) on a failed credential check.
    pub fn verify_operation(
        &self,
        tx: &dyn UnsignedTx,
        op: &NftOperation,
        cred: &Credential,
        utxo: &NftOutput,
    ) -> Result<()> {
        match op {
            NftOperation::Mint(op) => self.verify_mint_operation(tx, op, cred, utxo),
            NftOperation::Transfer(op) => self.verify_transfer_operation(tx, op, cred, utxo),
        }
    }

    /// `VerifyMintOperation` — the consumed UTXO must be a [`MintOutput`] sharing
    /// the operation's `group_id`; delegates the credential check to the secp
    /// spend gate over the consumed owners.
    fn verify_mint_operation(
        &self,
        tx: &dyn UnsignedTx,
        op: &MintOperation,
        cred: &Credential,
        utxo: &NftOutput,
    ) -> Result<()> {
        let NftOutput::Mint(out) = utxo else {
            return Err(Error::WrongUtxoType);
        };

        // verify.All(op, cred, out)
        verify_all(&[op, cred, out])?;

        if out.group_id != op.group_id {
            return Err(Error::WrongUniqueId);
        }
        self.inner
            .verify_credentials(tx, &op.mint_input, &cred.0, &out.owners)
            .map_err(Error::Fx)
    }

    /// `VerifyTransferOperation` — the consumed UTXO must be a [`TransferOutput`]
    /// sharing the operation output's `group_id` and `payload`; delegates the
    /// credential check to the secp spend gate over the consumed owners.
    fn verify_transfer_operation(
        &self,
        tx: &dyn UnsignedTx,
        op: &TransferOperation,
        cred: &Credential,
        utxo: &NftOutput,
    ) -> Result<()> {
        let NftOutput::Transfer(out) = utxo else {
            return Err(Error::WrongUtxoType);
        };

        // verify.All(op, cred, out)
        verify_all(&[op, cred, out])?;

        if out.group_id != op.output.group_id {
            return Err(Error::WrongUniqueId);
        }
        if out.payload != op.output.payload {
            return Err(Error::WrongBytes);
        }
        self.inner
            .verify_credentials(tx, &op.input, &cred.0, &out.owners)
            .map_err(Error::Fx)
    }
}

/// `verify.All(items...)` — verify each item, short-circuiting on the first
/// error and folding it onto the avm error model.
fn verify_all(items: &[&dyn Verifiable]) -> Result<()> {
    ava_vm::components::verify::all(items).map_err(Error::Fx)
}
