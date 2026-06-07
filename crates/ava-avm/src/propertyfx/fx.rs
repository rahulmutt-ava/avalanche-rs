// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `propertyfx` operation-verification adapter (specs 09 §4.3, FX-AVM-1).
//!
//! Mirrors Go `vms/propertyfx/fx.go`. propertyfx authorizes the minting and
//! burning of *property* outputs; unlike secp256k1fx it does **not** support
//! plain transfers (`VerifyTransfer` returns `errCantTransfer`).
//!
//! Because propertyfx operations use propertyfx's own concrete wire types
//! ([`MintOperation`], [`BurnOperation`], [`MintOutput`], [`OwnedOutput`],
//! [`Credential`]) — not secp's — this is **not** an implementor of the
//! secp-typed [`crate::fx::Fx`] trait. It is its own struct with inherent
//! methods; the unifying polymorphic dispatch across all three fxs is M5.9's
//! job.
//!
//! The heavy lifting — signature recovery, the threshold/locktime spend gate —
//! is delegated to [`ava_secp256k1fx::Fx::verify_credentials`] (the propertyfx
//! [`Credential`] embeds secp's `Credential`), sharing one recover-cache per fx
//! instance (specs 09 §4.1).
//!
//! Parity notes vs Go (`VerifyOperation` / `verifyOperationMint` /
//! `verifyOperationBurn` / `VerifyTransfer`):
//! * `Mint` requires the consumed UTXO to be a `MintOutput` (else
//!   [`Error::WrongUtxoType`]), the produced `mint_output.owners` to equal the
//!   consumed UTXO's owners (else [`Error::WrongMintOutput`]), then runs the
//!   spend gate over `mint_input`;
//! * `Burn` requires the consumed UTXO to be an `OwnedOutput` (else
//!   `WrongUtxoType`), then runs the spend gate over `input`;
//! * `VerifyTransfer` is unsupported and always returns [`Error::CantTransfer`];
//! * the `bootstrapped` gate lives inside `verify_credentials`: while the fx is
//!   not bootstrapped, the structural type/owner checks still run but the
//!   signature gate short-circuits to `Ok(())` (Go bootstrap-replay parity).

use std::sync::Arc;

use ava_secp256k1fx::Fx as Secp256k1Fx;
use ava_utils::clock::Clock;
use ava_vm::fx::UnsignedTx;

use crate::error::{Error, Result};
use crate::propertyfx::types::{BurnOperation, Credential, MintOperation, MintOutput, OwnedOutput};

/// A propertyfx operation, tagged by its registered type (matching Go's
/// `switch op := opIntf.(type)` over `*MintOperation` / `*BurnOperation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyOperation {
    /// A [`MintOperation`] (typeID 17) — continues mint authority and mints an
    /// owned output.
    Mint(MintOperation),
    /// A [`BurnOperation`] (typeID 18) — burns an owned output.
    Burn(BurnOperation),
}

/// The consumed UTXO output a propertyfx operation references, tagged by its
/// registered type (matching Go's `utxoIntf.(*MintOutput)` / `.(*OwnedOutput)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyUtxo {
    /// A [`MintOutput`] (typeID 15) — continuing mint authority.
    Mint(MintOutput),
    /// An [`OwnedOutput`] (typeID 16) — a held property output.
    Owned(OwnedOutput),
}

/// `propertyfx.Fx` — the property feature-extension verifier.
///
/// Wraps one [`ava_secp256k1fx::Fx`] (sharing its recover-cache and
/// `bootstrapped` flag); construct via [`Fx::new`] with the VM clock.
pub struct Fx {
    inner: Secp256k1Fx,
}

impl Fx {
    /// Builds a [`Fx`] reading time through `clock`; signature verification
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

    /// `VerifyOperation(tx, op, cred, utxo)` — dispatches on the operation type
    /// (Go `VerifyOperation`).
    ///
    /// # Errors
    /// Returns [`Error::WrongUtxoType`] when the consumed UTXO is the wrong
    /// output type for the operation, [`Error::WrongMintOutput`] when a mint
    /// operation's produced owners differ from the consumed UTXO's, or the
    /// wrapped fx spend-gate sentinel ([`Error::Fx`]) on a failed credential
    /// check.
    pub fn verify_operation(
        &self,
        tx: &dyn UnsignedTx,
        op: &PropertyOperation,
        cred: &Credential,
        utxo: &PropertyUtxo,
    ) -> Result<()> {
        match op {
            PropertyOperation::Mint(op) => self.verify_mint(tx, op, cred, utxo),
            PropertyOperation::Burn(op) => self.verify_burn(tx, op, cred, utxo),
        }
    }

    /// `verifyOperationMint` — mint requires a `MintOutput` UTXO whose owners
    /// equal the produced `mint_output.owners`, then runs the spend gate.
    fn verify_mint(
        &self,
        tx: &dyn UnsignedTx,
        op: &MintOperation,
        cred: &Credential,
        utxo: &PropertyUtxo,
    ) -> Result<()> {
        let PropertyUtxo::Mint(out) = utxo else {
            return Err(Error::WrongUtxoType);
        };
        if !out.owners.equals(&op.mint_output.owners) {
            return Err(Error::WrongMintOutput);
        }
        self.inner
            .verify_credentials(tx, &op.mint_input, &cred.0, &out.owners)
            .map_err(Error::Fx)
    }

    /// `verifyOperationBurn` — burn requires an `OwnedOutput` UTXO, then runs the
    /// spend gate over the burn input.
    fn verify_burn(
        &self,
        tx: &dyn UnsignedTx,
        op: &BurnOperation,
        cred: &Credential,
        utxo: &PropertyUtxo,
    ) -> Result<()> {
        let PropertyUtxo::Owned(out) = utxo else {
            return Err(Error::WrongUtxoType);
        };
        self.inner
            .verify_credentials(tx, &op.input, &cred.0, &out.owners)
            .map_err(Error::Fx)
    }

    /// `VerifyTransfer` — unsupported by propertyfx (Go `errCantTransfer`).
    ///
    /// # Errors
    /// Always returns [`Error::CantTransfer`].
    #[expect(clippy::unused_self, reason = "mirrors Go's method receiver")]
    pub fn verify_transfer(&self) -> Result<()> {
        Err(Error::CantTransfer)
    }
}
