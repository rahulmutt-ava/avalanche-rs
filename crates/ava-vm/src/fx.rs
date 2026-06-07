// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The feature-extension (`fx`) framework (`vms/fx`, `vms/secp256k1fx`, specs
//! 07 §4.1).
//!
//! A VM holds a `Vec<`[`Fx`]`>` (Go `common.Fx{ ID, Fx }`) and calls into each
//! fx to verify spends. The fx registers its codec types into the host VM's
//! [`CodecRegistry`] at [`FxInstance::initialize`].
//!
//! # The `&dyn Any` boundary
//!
//! Go passes `interface{}` to the verification methods and type-asserts inside
//! the fx (`txIntf.(UnsignedTx)`, `inIntf.(*TransferInput)`). The Rust port keeps
//! that dynamic boundary with [`std::any::Any`] + `downcast_ref`, mapping a
//! failed downcast to the matching Go sentinel ([`Error::WrongTxType`],
//! [`Error::WrongInputType`], [`Error::WrongCredentialType`],
//! [`Error::WrongOwnerType`], [`Error::WrongUtxoType`], [`Error::WrongOpType`]).
//! This preserves the exact error semantics while letting P/X-Chain pass their
//! own tx/input/output concrete types through one fx.

use std::any::Any;
use std::sync::Arc;
use std::time::SystemTime;

use ava_types::id::Id;

use crate::components::verify::State;
use crate::error::Result;

/// `common.Fx` — an fx instance bound to its id (Go `common.Fx{ ID, Fx }`).
///
/// The full fx framework lands here in M3.20: `fx` is the
/// [`FxInstance`](crate::fx::FxInstance) the VM drives. Cloning shares the
/// underlying fx via the `Arc`.
#[derive(Clone)]
pub struct Fx {
    /// The fx's id.
    pub id: Id,
    /// The fx instance the VM calls into to verify spends.
    pub fx: Arc<dyn FxInstance>,
}

impl Fx {
    /// Builds a [`Fx`] binding `fx` to its `id`.
    #[must_use]
    pub fn new(id: Id, fx: Arc<dyn FxInstance>) -> Self {
        Self { id, fx }
    }
}

impl std::fmt::Debug for Fx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fx").field("id", &self.id).finish()
    }
}

/// `secp256k1fx.UnsignedTx` — the bytes a credential signs over.
///
/// Generalized across fxs: every fx hashes `bytes()` to recover signatures
/// (`sha256(unsigned_tx_bytes)`). A concrete tx type (P/X-Chain) implements this
/// and is passed as a `&dyn UnsignedTx` through the fx verification surface.
pub trait UnsignedTx: Send + Sync {
    /// `Bytes()` — the unsigned-transaction bytes hashed for signature recovery.
    fn bytes(&self) -> &[u8];
}

impl UnsignedTx for Vec<u8> {
    fn bytes(&self) -> &[u8] {
        self
    }
}

impl UnsignedTx for &[u8] {
    fn bytes(&self) -> &[u8] {
        self
    }
}

/// `codec.Registry` — the host VM's typeID registry, the surface an fx registers
/// its codec types into at [`FxInstance::initialize`].
///
/// Go's `codec.Registry.RegisterType(v)` assigns the next sequential `u32`
/// typeID in registration order. Here `register_type(name)` records the named
/// type and returns its assigned typeID, so a host can assert the fx registered
/// the expected types in the expected order (specs 07 §4.2).
pub trait CodecRegistry: Send + Sync {
    /// `RegisterType` — register a type by name, returning its sequential typeID.
    ///
    /// # Errors
    /// Returns an [`Error`](crate::error::Error) if registration fails (e.g. a
    /// duplicate type or a full registry).
    fn register_type(&self, name: &str) -> Result<u32>;
}

/// `secp256k1fx.VM` — the host callbacks an fx needs (specs 07 §4.1).
///
/// The fx's view of its host VM: the codec registry it registers types into and
/// the clock it reads for locktime checks. (Go also exposes `Logger()`; there is
/// no `Logger` type in the workspace yet, so it is omitted — see
/// `tests/PORTING.md`.)
pub trait FxVm: Send + Sync {
    /// `CodecRegistry()` — the host's typeID registry.
    fn codec_registry(&self) -> &dyn CodecRegistry;

    /// `Clock()` — the host clock, read for locktime maturity checks.
    fn clock(&self) -> SystemTime;
}

/// `vms/fx.Fx` + the verification surface from `secp256k1fx.Fx` (specs 07 §4.1).
///
/// All Go `*Intf interface{}` params become `&dyn Any`; the runtime `ok`
/// type-asserts in Go become `downcast_ref` returning the matching
/// `Error::Wrong*Type` variant.
pub trait FxInstance: Send + Sync {
    /// `Initialize(vm)` — register the fx's codec types into the host VM's
    /// registry and stash the host handles (clock, recover-cache).
    ///
    /// # Errors
    /// Returns an [`Error`](crate::error::Error) if codec registration fails or
    /// the host VM is the wrong type ([`Error::WrongVmType`]).
    fn initialize(&mut self, vm: Arc<dyn FxVm>) -> Result<()>;

    /// `Bootstrapping()` — entering the bootstrap phase (signature verification
    /// stays disabled).
    ///
    /// # Errors
    /// Returns an [`Error`](crate::error::Error) if the transition fails.
    fn bootstrapping(&mut self) -> Result<()>;

    /// `Bootstrapped()` — bootstrap complete; enables signature verification.
    ///
    /// # Errors
    /// Returns an [`Error`](crate::error::Error) if the transition fails.
    fn bootstrapped(&mut self) -> Result<()>;

    /// `VerifyTransfer(tx, in, cred, utxo)` — `Ok(())` iff `cred` proves the
    /// `utxo` owners assent to spending it under `input`.
    ///
    /// # Errors
    /// Returns the matching `Wrong*Type` sentinel on a downcast miss, or a
    /// verification error from the underlying spend check.
    fn verify_transfer(
        &self,
        tx: &dyn UnsignedTx,
        input: &dyn Any,
        cred: &dyn Any,
        utxo: &dyn Any,
    ) -> Result<()>;

    /// `VerifyPermission(tx, in, cred, owner)` — `Ok(())` iff `cred` proves
    /// `owner` assents to `tx`.
    ///
    /// # Errors
    /// Returns the matching `Wrong*Type` sentinel on a downcast miss, or a
    /// verification error.
    fn verify_permission(
        &self,
        tx: &dyn UnsignedTx,
        input: &dyn Any,
        cred: &dyn Any,
        owner: &dyn Any,
    ) -> Result<()>;

    /// `VerifyOperation(tx, op, cred, utxos)` — `Ok(())` iff `cred` authorizes
    /// the operation `op` consuming `utxos`.
    ///
    /// # Errors
    /// Returns the matching `Wrong*Type` sentinel on a downcast miss,
    /// [`Error::WrongNumberOfUtxos`](crate::error::Error::WrongNumberOfUtxos) on
    /// a bad utxo count, or a verification error.
    fn verify_operation(
        &self,
        tx: &dyn UnsignedTx,
        op: &dyn Any,
        cred: &dyn Any,
        utxos: &[&dyn Any],
    ) -> Result<()>;

    /// `CreateOutput(amount, owner)` — build a transferable output worth
    /// `amount` controlled by `owner`.
    ///
    /// # Errors
    /// Returns [`Error::WrongOwnerType`](crate::error::Error::WrongOwnerType) on
    /// a downcast miss, or the `owner`'s verification error.
    fn create_output(&self, amount: u64, owner: &dyn Any) -> Result<Arc<dyn State>>;
}
