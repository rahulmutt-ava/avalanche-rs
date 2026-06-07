// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The secp256k1fx [`FxInstance`] (`secp256k1fx.Fx`, specs 07 §4.1).
//!
//! [`Secp256k1Fx`] is the `ava-vm::fx`-framework instance the VMs drive. It wraps
//! the verification state ([`Fx`]) and implements the dynamic `&dyn Any`
//! verification surface: each method `downcast_ref`s its arguments to the
//! concrete secp256k1fx codec types, mapping a downcast miss to the matching Go
//! wrong-type sentinel, then delegates to [`Fx::verify_credentials`].
//!
//! At [`Secp256k1Fx::initialize`] it registers its five codec types into the host
//! VM's [`CodecRegistry`] in the Go registration order (`TransferInput`(0),
//! `MintOutput`(1), `TransferOutput`(2), `MintOperation`(3), `Credential`(4)).

use std::any::Any;
use std::sync::Arc;

use ava_utils::clock::Clock;
use ava_vm::components::verify::{State, Verifiable, all};
use ava_vm::error::{Error, Result};
use ava_vm::fx::{FxInstance, FxVm, UnsignedTx};

use crate::fx::Fx;
use crate::types::{Credential, Input, OutputOwners, TransferInput, TransferOutput};

/// `secp256k1fx.Fx` — the fx-framework instance for the secp256k1 feature
/// extension (specs 07 §4.1).
///
/// Wraps the [`Fx`] verification state (clock + bootstrapped flag) and exposes
/// the dynamic [`FxInstance`] surface the VMs call into.
pub struct Secp256k1Fx {
    inner: Fx,
}

impl Secp256k1Fx {
    /// Builds a [`Secp256k1Fx`] reading time through `clock`, signature
    /// verification **disabled** (still bootstrapping), matching Go.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Fx::new(clock),
        }
    }

    /// `VerifySpend` — `verify::All(utxo, in, cred)`, then `utxo.amt == in.amt`,
    /// then `VerifyCredentials` (`vms/secp256k1fx/fx.go`).
    fn verify_spend(
        &self,
        tx: &dyn UnsignedTx,
        input: &TransferInput,
        cred: &Credential,
        utxo: &TransferOutput,
    ) -> Result<()> {
        // Go `VerifySpend`: `verify.All(utxo, in, cred)` (in == the TransferInput).
        all(&[utxo as &dyn Verifiable, input, cred])?;
        if utxo.amt != input.amt {
            return Err(Error::MismatchedAmounts);
        }
        self.inner
            .verify_credentials(tx, &input.input, cred, &utxo.owners)
    }
}

impl FxInstance for Secp256k1Fx {
    fn initialize(&mut self, vm: Arc<dyn FxVm>) -> Result<()> {
        // Register the five fx codec types in Go registration order; the typeIDs
        // are assigned sequentially by the host registry (specs 07 §4.2).
        let c = vm.codec_registry();
        c.register_type("TransferInput")?;
        c.register_type("MintOutput")?;
        c.register_type("TransferOutput")?;
        c.register_type("MintOperation")?;
        c.register_type("Credential")?;
        Ok(())
    }

    fn bootstrapping(&mut self) -> Result<()> {
        self.inner.bootstrapping();
        Ok(())
    }

    fn bootstrapped(&mut self) -> Result<()> {
        self.inner.bootstrapped();
        Ok(())
    }

    fn verify_transfer(
        &self,
        tx: &dyn UnsignedTx,
        input: &dyn Any,
        cred: &dyn Any,
        utxo: &dyn Any,
    ) -> Result<()> {
        let input = input
            .downcast_ref::<TransferInput>()
            .ok_or(Error::WrongInputType)?;
        let cred = cred
            .downcast_ref::<Credential>()
            .ok_or(Error::WrongCredentialType)?;
        let utxo = utxo
            .downcast_ref::<TransferOutput>()
            .ok_or(Error::WrongUtxoType)?;
        self.verify_spend(tx, input, cred, utxo)
    }

    fn verify_permission(
        &self,
        tx: &dyn UnsignedTx,
        input: &dyn Any,
        cred: &dyn Any,
        owner: &dyn Any,
    ) -> Result<()> {
        let input = input.downcast_ref::<Input>().ok_or(Error::WrongInputType)?;
        let cred = cred
            .downcast_ref::<Credential>()
            .ok_or(Error::WrongCredentialType)?;
        let owner = owner
            .downcast_ref::<OutputOwners>()
            .ok_or(Error::WrongOwnerType)?;
        all(&[input as &dyn Verifiable, cred, owner])?;
        self.inner.verify_credentials(tx, input, cred, owner)
    }

    fn verify_operation(
        &self,
        _tx: &dyn UnsignedTx,
        op: &dyn Any,
        _cred: &dyn Any,
        _utxos: &[&dyn Any],
    ) -> Result<()> {
        // Go's `VerifyOperation` downcasts `tx`→`op`→`cred`→`utxos` then runs
        // `verifyOperation` (mint). `MintOperation` (typeID 3) is not yet a Rust
        // codec type (M3.19 left it a reserved discriminant), so the `op`
        // downcast can never succeed and this method faithfully reports
        // `WrongOpType` for any argument. The full mint verification lands with
        // the `MintOperation` type — see `tests/PORTING.md`.
        op.downcast_ref::<MintOperationUnported>()
            .map(|_| ())
            .ok_or(Error::WrongOpType)
    }

    fn create_output(&self, amount: u64, owner: &dyn Any) -> Result<Arc<dyn State>> {
        let owner = owner
            .downcast_ref::<OutputOwners>()
            .ok_or(Error::WrongOwnerType)?;
        owner.verify()?;
        Ok(Arc::new(TransferOutput::new(amount, owner.clone())))
    }
}

/// Placeholder concrete type for the not-yet-ported `MintOperation` (typeID 3).
///
/// `verify_operation` downcasts to this so the op path compiles and faithfully
/// reports [`Error::WrongOpType`] for any real argument until `MintOperation` is
/// ported (M3.19 left it a reserved codec discriminant). It is intentionally
/// never constructed: no value can downcast to it, so the op check always fails.
#[allow(dead_code)]
struct MintOperationUnported;
