// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The fx dispatch table — `ParsedFx` + `TypeToFxIndex` routing (specs 09 §2.2,
//! §4; FX-AVM-1).
//!
//! Port of `vms/avm/fxs/fx.go` (`ParsedFx`) plus the routing core of
//! `vms/avm/txs/executor/semantic_verifier.go` (`getFx` / `verifyTransferOfUTXO`
//! / `verifyOperation`). Go keys its `typeToFxIndex map[reflect.Type]int` by the
//! reflect type of a parsed value; the Rust port keys the
//! [`TypeToFxIndex`](crate::txs::codec::TypeToFxIndex) table (built in M5.5's
//! [`type_to_fx_index`](crate::txs::codec::type_to_fx_index)) by the value's
//! **codec type-id** (`u32`). [`resolve_fx_index`] performs the lookup, returning
//! [`Error::UnknownFx`] on a miss (Go `errUnknownFx`).
//!
//! ## Why an enum instead of `Box<dyn Fx>`
//!
//! The plan text (written before M5.6–M5.8) called for `ParsedFx { fx: Box<dyn
//! Fx> }`. In the as-built crate the three fxs do **not** share one object-safe
//! trait: [`crate::fx::secp::SecpFx`] implements the secp-typed [`crate::fx::Fx`]
//! trait (its `verify_operation` takes the concrete secp `Input` / `MintOutput`),
//! whereas [`nftfx::Fx`] and [`propertyfx::Fx`] expose **inherent** methods over
//! their own concrete operation/utxo types (`vms/nftfx/fx.go`,
//! `vms/propertyfx/fx.go`). They are deliberately heterogeneous.
//!
//! Rather than force an object-safe trait via `dyn Any` downcasting, this module
//! holds the three adapters in an [`FxKind`] enum and dispatches via `match` —
//! matching the codebase's enum-over-trait-object preference (the
//! `components::Output` / credential interface enums already do this). The
//! type-id → [`FxIndex`] routing remains the single source of truth, so the enum
//! tag and the routed index can never disagree.

use std::sync::Arc;

use ava_secp256k1fx::{Credential as SecpCredential, Input, MintOutput, TransferInput};
use ava_types::id::Id;
use ava_utils::clock::Clock;
use ava_vm::fx::UnsignedTx;

use crate::error::{Error, Result};
use crate::fx::Fx as SecpFxTrait;
use crate::fx::secp::SecpFx;
use crate::fx_index::FxIndex;
use crate::nftfx;
use crate::propertyfx;
use crate::txs::codec::{TypeToFxIndex, type_to_fx_index};

/// Resolves a codec `type_id` to the [`FxIndex`] of the fx that owns it
/// (specs 09 §2.2). Mirrors Go `getFx` over the `typeToFxIndex` map.
///
/// secp256k1fx owns 5–9, nftfx 10–14, propertyfx 15–19; tx types (0–4) and the
/// block (20) are not fx types.
///
/// # Errors
/// Returns [`Error::UnknownFx`] (`errUnknownFx`) when `type_id` has no registered
/// fx.
pub fn resolve_fx_index(type_id: u32) -> Result<FxIndex> {
    routing_table()
        .get(&type_id)
        .copied()
        .ok_or(Error::UnknownFx)
}

/// Resolves a parsed value to the [`FxIndex`] of its owning fx, via its codec
/// type-id (specs 09 §2.2, §4). The typed analogue of Go's
/// `getFx(val interface{})`.
///
/// # Errors
/// Returns [`Error::UnknownFx`] when the value's type-id has no registered fx.
pub fn resolve_fx_index_of<V: FxValue>(value: &V) -> Result<FxIndex> {
    resolve_fx_index(value.fx_type_id())
}

/// The process-wide `type_id → FxIndex` routing table.
fn routing_table() -> &'static TypeToFxIndex {
    use std::sync::OnceLock;
    static TABLE: OnceLock<TypeToFxIndex> = OnceLock::new();
    TABLE.get_or_init(type_to_fx_index)
}

/// A parsed value that carries its registered codec type-id (specs 09 §2.1).
///
/// The avm verifier routes outputs / inputs / operations / credentials to their
/// owning fx by this type-id (`resolve_fx_index_of`). secp interface enums expose
/// it via the `#[codec(type_registry)]` derive; the nftfx / propertyfx concrete
/// types — which are not part of a type-registry enum — implement it here.
pub trait FxValue {
    /// The value's registered codec type-id (specs 09 §2.1).
    fn fx_type_id(&self) -> u32;
}

// secp256k1fx interface enums already carry `.codec_type_id()` via the
// `#[codec(type_registry)]` derive; forward to it.
impl FxValue for crate::txs::components::Output {
    fn fx_type_id(&self) -> u32 {
        self.codec_type_id()
    }
}

impl FxValue for crate::txs::components::Input {
    fn fx_type_id(&self) -> u32 {
        self.codec_type_id()
    }
}

impl FxValue for crate::txs::credential::Credential {
    fn fx_type_id(&self) -> u32 {
        self.codec_type_id()
    }
}

// nftfx concrete types (typeIDs 10–14).
impl FxValue for nftfx::NftOutput {
    fn fx_type_id(&self) -> u32 {
        match self {
            nftfx::NftOutput::Mint(_) => 10,
            nftfx::NftOutput::Transfer(_) => 11,
        }
    }
}

impl FxValue for nftfx::NftOperation {
    fn fx_type_id(&self) -> u32 {
        match self {
            nftfx::NftOperation::Mint(_) => 12,
            nftfx::NftOperation::Transfer(_) => 13,
        }
    }
}

impl FxValue for nftfx::Credential {
    fn fx_type_id(&self) -> u32 {
        14
    }
}

// propertyfx concrete types (typeIDs 15–19).
impl FxValue for propertyfx::PropertyUtxo {
    fn fx_type_id(&self) -> u32 {
        match self {
            propertyfx::PropertyUtxo::Mint(_) => 15,
            propertyfx::PropertyUtxo::Owned(_) => 16,
        }
    }
}

impl FxValue for propertyfx::PropertyOperation {
    fn fx_type_id(&self) -> u32 {
        match self {
            propertyfx::PropertyOperation::Mint(_) => 17,
            propertyfx::PropertyOperation::Burn(_) => 18,
        }
    }
}

impl FxValue for propertyfx::Credential {
    fn fx_type_id(&self) -> u32 {
        19
    }
}

/// A constructed feature extension, tagged by its kind (`vms/avm/fxs/fx.go`
/// `Fx interface`). The three avm fxs are heterogeneous, so they are held as an
/// enum rather than a `dyn Fx` trait object (see the module docs).
pub enum FxKind {
    /// The base secp256k1 fx ([`FxIndex::Secp256k1`]).
    Secp(SecpFx),
    /// The non-fungible-token fx ([`FxIndex::Nft`]).
    Nft(nftfx::Fx),
    /// The property fx ([`FxIndex::Property`]).
    Property(propertyfx::Fx),
}

impl FxKind {
    /// The [`FxIndex`] this fx is registered under.
    #[must_use]
    pub fn index(&self) -> FxIndex {
        match self {
            FxKind::Secp(_) => FxIndex::Secp256k1,
            FxKind::Nft(_) => FxIndex::Nft,
            FxKind::Property(_) => FxIndex::Property,
        }
    }
}

/// `fxs.ParsedFx` — a constructed fx plus its registered fx id
/// (`vms/avm/fxs/fx.go`).
pub struct ParsedFx {
    /// The fx's id (`ParsedFx.ID`).
    pub id: Id,
    /// The constructed fx (`ParsedFx.Fx`).
    pub fx: FxKind,
}

/// The avm fx dispatch table — the parsed fxs indexed by [`FxIndex`] plus the
/// shared `type_id → FxIndex` routing (`vms/avm` `vm.fxs` + `vm.typeToFxIndex`).
///
/// Build the three avm fxs (secp256k1fx, nftfx, propertyfx) in VM-registration
/// order via [`Dispatch::new`]; route a parsed value to its fx with
/// [`Dispatch::route_transfer`] / [`Dispatch::route_operation`] /
/// [`Dispatch::route_output`].
pub struct Dispatch {
    /// `vm.fxs []*ParsedFx` — indexed by [`FxIndex`] (registration order).
    fxs: Vec<ParsedFx>,
}

impl Dispatch {
    /// Builds the dispatch table from the three avm fxs in VM-registration order
    /// (secp256k1fx, nftfx, propertyfx; specs 09 §2.2). All three share the VM
    /// `clock`; each owns its own recover-cache and `bootstrapped` flag.
    #[must_use]
    pub fn new(secp_id: Id, nft_id: Id, property_id: Id, clock: Arc<dyn Clock>) -> Self {
        Self {
            fxs: vec![
                ParsedFx {
                    id: secp_id,
                    fx: FxKind::Secp(SecpFx::new(Arc::clone(&clock))),
                },
                ParsedFx {
                    id: nft_id,
                    fx: FxKind::Nft(nftfx::Fx::new(Arc::clone(&clock))),
                },
                ParsedFx {
                    id: property_id,
                    fx: FxKind::Property(propertyfx::Fx::new(clock)),
                },
            ],
        }
    }

    /// The parsed fxs, indexed by [`FxIndex`] (`vm.fxs`).
    #[must_use]
    pub fn fxs(&self) -> &[ParsedFx] {
        &self.fxs
    }

    /// The [`ParsedFx`] registered under `index` (`vm.fxs[fxIndex]`), or `None`
    /// if no such fx is registered.
    #[must_use]
    pub fn get(&self, index: FxIndex) -> Option<&ParsedFx> {
        self.fxs.get(index as usize)
    }

    /// Transitions every fx out of bootstrapping (`vm.Bootstrapped` enables
    /// signature verification on all fxs).
    pub fn bootstrapped(&mut self) {
        for parsed in &mut self.fxs {
            match &mut parsed.fx {
                FxKind::Secp(fx) => fx.bootstrapped(),
                FxKind::Nft(fx) => fx.bootstrapped(),
                FxKind::Property(fx) => fx.bootstrapped(),
            }
        }
    }

    /// `fx.VerifyTransfer(tx, in, cred, utxo.Out)` — routes a transferable-input
    /// spend to the fx that owns the credential and runs its transfer check
    /// (`verifyTransferOfUTXO`). Only the secp fx authorizes plain transfers;
    /// nftfx / propertyfx return [`Error::CantTransfer`].
    ///
    /// # Errors
    /// Returns [`Error::UnknownFx`] when the credential type has no registered
    /// fx, [`Error::CantTransfer`] when the routed fx does not support transfers,
    /// or the routed fx's transfer verification error.
    pub fn route_transfer(
        &self,
        tx: &dyn UnsignedTx,
        input: &TransferInput,
        cred: &crate::txs::credential::Credential,
        utxo: &ava_secp256k1fx::TransferOutput,
    ) -> Result<()> {
        let index = resolve_fx_index_of(cred)?;
        let parsed = self.get(index).ok_or(Error::UnknownFx)?;
        match &parsed.fx {
            FxKind::Secp(fx) => fx.verify_transfer(tx, input, secp_credential(cred)?, utxo),
            // nftfx / propertyfx do not authorize plain transfers (errCantTransfer).
            FxKind::Nft(_) | FxKind::Property(_) => Err(Error::CantTransfer),
        }
    }

    /// `fx.VerifyOperation(tx, op, cred, utxos)` for a **secp256k1fx** mint
    /// operation — routes to the secp fx and runs its operation check.
    ///
    /// # Errors
    /// Returns [`Error::UnknownFx`] when the credential does not route to the
    /// secp fx, or the secp fx's operation verification error.
    pub fn route_secp_operation(
        &self,
        tx: &dyn UnsignedTx,
        mint_input: &Input,
        mint_output: &MintOutput,
        cred: &crate::txs::credential::Credential,
        utxo: &MintOutput,
    ) -> Result<()> {
        let FxKind::Secp(fx) = &self.routed_fx(cred)? else {
            return Err(Error::UnknownFx);
        };
        fx.verify_operation(tx, mint_input, mint_output, secp_credential(cred)?, utxo)
    }

    /// `fx.VerifyOperation(tx, op, cred, utxos)` for an **nftfx** operation —
    /// routes to the nftfx and runs its operation check.
    ///
    /// # Errors
    /// Returns [`Error::UnknownFx`] when `cred` does not route to the nftfx, or
    /// the nftfx's operation verification error.
    pub fn route_nft_operation(
        &self,
        tx: &dyn UnsignedTx,
        op: &nftfx::NftOperation,
        cred: &nftfx::Credential,
        utxo: &nftfx::NftOutput,
    ) -> Result<()> {
        let FxKind::Nft(fx) = &self.routed_fx(cred)? else {
            return Err(Error::UnknownFx);
        };
        fx.verify_operation(tx, op, cred, utxo)
    }

    /// `fx.VerifyOperation(tx, op, cred, utxos)` for a **propertyfx** operation —
    /// routes to the propertyfx and runs its operation check.
    ///
    /// # Errors
    /// Returns [`Error::UnknownFx`] when `cred` does not route to the propertyfx,
    /// or the propertyfx's operation verification error.
    pub fn route_property_operation(
        &self,
        tx: &dyn UnsignedTx,
        op: &propertyfx::PropertyOperation,
        cred: &propertyfx::Credential,
        utxo: &propertyfx::PropertyUtxo,
    ) -> Result<()> {
        let FxKind::Property(fx) = &self.routed_fx(cred)? else {
            return Err(Error::UnknownFx);
        };
        fx.verify_operation(tx, op, cred, utxo)
    }

    /// Resolves the [`FxIndex`] of a parsed output and returns its registered
    /// [`FxIndex`] (`getFx(out.Out)`). The output's owning fx is whichever fx the
    /// type-id routes to; the caller then checks fx-usage against the asset's
    /// `InitialState`.
    ///
    /// # Errors
    /// Returns [`Error::UnknownFx`] when the output type has no registered fx.
    pub fn route_output<V: FxValue>(&self, out: &V) -> Result<FxIndex> {
        let index = resolve_fx_index_of(out)?;
        // Guard that an fx is actually registered under the routed index.
        self.get(index).ok_or(Error::UnknownFx)?;
        Ok(index)
    }

    /// Resolves the [`FxKind`] a parsed value routes to (`vm.fxs[getFx(val)]`).
    fn routed_fx<V: FxValue>(&self, value: &V) -> Result<&FxKind> {
        let index = resolve_fx_index_of(value)?;
        self.get(index).map(|p| &p.fx).ok_or(Error::UnknownFx)
    }
}

/// Extracts the embedded `secp256k1fx.Credential` from an avm tx credential
/// (the only credential the secp transfer/operation gate accepts).
fn secp_credential(cred: &crate::txs::credential::Credential) -> Result<&SecpCredential> {
    match cred {
        crate::txs::credential::Credential::Secp256k1(c) => Ok(c),
    }
}
