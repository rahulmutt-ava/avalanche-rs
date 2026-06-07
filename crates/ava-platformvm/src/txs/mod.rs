// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain transaction model — the [`UnsignedTx`] interface enum, the type_id
//! registry (`codec.rs`), and the signed [`Tx`] envelope (`tx.rs`).
//!
//! Port of `vms/platformvm/txs` (specs 08 §2). The `type_id`s assigned here are
//! **protocol constants** shared with the block codec (specs 08 §2.1): the block
//! codec registers the 5 Apricot block types at 0–4, then the tx types, then the
//! 4 Banff block types at 29–32 — so the tx variants carry explicit
//! `#[codec(type_id = N)]`s with reserved gaps rather than auto-increment.
//!
//! The per-tx structs are **placeholders** for M4.2: empty (unit) structs that
//! derive the codec traits so the enum compiles and the `type_id`s are
//! assertable. M4.3/M4.4 flesh out their fields and verification rules.

use ava_codec::AvaCodec;

use crate::error::Error;

pub mod codec;
pub mod fee;
pub mod tx;

pub use codec::{Codec, GenesisCodec};
pub use tx::{Credential, Tx};

// ---------------------------------------------------------------------------
// Block type-id constants (shared numbering space; specs 08 §2.1 / §4.1)
// ---------------------------------------------------------------------------
//
// The `Block` enum itself is M4.5. The block `type_id`s share this registry's
// numbering space (the tx codec reserves them with `SkipRegistrations`), so they
// are pinned here as named constants and asserted by `golden::type_id_table`.
// TODO(M4.5): replace these constants with the real `block::Block` enum
// discriminants (`Block::codec_type_id`) and assert those instead.

/// `ApricotProposalBlock` — block `type_id` 0.
pub const TYPE_ID_APRICOT_PROPOSAL_BLOCK: u32 = 0;
/// `ApricotAbortBlock` — block `type_id` 1.
pub const TYPE_ID_APRICOT_ABORT_BLOCK: u32 = 1;
/// `ApricotCommitBlock` — block `type_id` 2.
pub const TYPE_ID_APRICOT_COMMIT_BLOCK: u32 = 2;
/// `ApricotStandardBlock` — block `type_id` 3.
pub const TYPE_ID_APRICOT_STANDARD_BLOCK: u32 = 3;
/// `ApricotAtomicBlock` — block `type_id` 4.
pub const TYPE_ID_APRICOT_ATOMIC_BLOCK: u32 = 4;
/// `BanffProposalBlock` — block `type_id` 29.
pub const TYPE_ID_BANFF_PROPOSAL_BLOCK: u32 = 29;
/// `BanffAbortBlock` — block `type_id` 30.
pub const TYPE_ID_BANFF_ABORT_BLOCK: u32 = 30;
/// `BanffCommitBlock` — block `type_id` 31.
pub const TYPE_ID_BANFF_COMMIT_BLOCK: u32 = 31;
/// `BanffStandardBlock` — block `type_id` 32.
pub const TYPE_ID_BANFF_STANDARD_BLOCK: u32 = 32;

// ---------------------------------------------------------------------------
// UTXO component placeholders
// ---------------------------------------------------------------------------
//
// `UnsignedTx::{inputs,outputs}` return slices of the avax UTXO components. The
// real `TransferableInput`/`TransferableOutput` live in `ava_vm::components::avax`
// (not a direct dependency of this crate at M4.2). They are stubbed here as
// empty placeholder types so the accessor signatures compile and return empty
// slices.
// TODO(M4.3): replace with `ava_vm::components::avax::{TransferableInput,
// TransferableOutput}` once the avax components are wired into this crate.

/// Placeholder for `avax::TransferableInput` (M4.3 will replace).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferableInput;

/// Placeholder for `avax::TransferableOutput` (M4.3 will replace).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferableOutput;

// ---------------------------------------------------------------------------
// Per-tx placeholder structs
// ---------------------------------------------------------------------------
//
// Each is a unit struct deriving `AvaCodec` (serializes to zero bytes). M4.3/M4.4
// flesh out the fields per specs 08 §2.2.

macro_rules! placeholder_txs {
    ($($(#[$m:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$m])*
        // TODO(M4.3/M4.4): flesh out fields (specs 08 §2.2).
        #[derive(AvaCodec, Debug, Clone, Default, PartialEq, Eq)]
        pub struct $name;
    )+};
}

placeholder_txs!(
    /// `AddValidatorTx` (deprecated, parse-only).
    AddValidatorTx,
    /// `AddSubnetValidatorTx` (deprecated).
    AddSubnetValidatorTx,
    /// `AddDelegatorTx` (deprecated).
    AddDelegatorTx,
    /// `CreateChainTx`.
    CreateChainTx,
    /// `CreateSubnetTx`.
    CreateSubnetTx,
    /// `ImportTx`.
    ImportTx,
    /// `ExportTx`.
    ExportTx,
    /// `AdvanceTimeTx` (proposal; Apricot-only).
    AdvanceTimeTx,
    /// `RewardValidatorTx` (proposal).
    RewardValidatorTx,
    /// `RemoveSubnetValidatorTx`.
    RemoveSubnetValidatorTx,
    /// `TransformSubnetTx` (no-op post-Etna).
    TransformSubnetTx,
    /// `AddPermissionlessValidatorTx`.
    AddPermissionlessValidatorTx,
    /// `AddPermissionlessDelegatorTx`.
    AddPermissionlessDelegatorTx,
    /// `TransferSubnetOwnershipTx`.
    TransferSubnetOwnershipTx,
    /// `BaseTx`.
    BaseTx,
    /// `ConvertSubnetToL1Tx` (ACP-77).
    ConvertSubnetToL1Tx,
    /// `RegisterL1ValidatorTx`.
    RegisterL1ValidatorTx,
    /// `SetL1ValidatorWeightTx`.
    SetL1ValidatorWeightTx,
    /// `IncreaseL1ValidatorBalanceTx`.
    IncreaseL1ValidatorBalanceTx,
    /// `DisableL1ValidatorTx`.
    DisableL1ValidatorTx,
    /// `AddAutoRenewedValidatorTx` (Helicon).
    AddAutoRenewedValidatorTx,
    /// `SetAutoRenewedValidatorConfigTx` (Helicon).
    SetAutoRenewedValidatorConfigTx,
    /// `RewardAutoRenewedValidatorTx` (Helicon).
    RewardAutoRenewedValidatorTx,
);

// ---------------------------------------------------------------------------
// UnsignedTx interface enum
// ---------------------------------------------------------------------------

/// `txs.UnsignedTx` — the Go interface registered into the codec; its concrete
/// types become enum variants (specs 08 §2.2).
///
/// `type_id`s share the block codec numbering space (specs 08 §2.1), hence the
/// explicit `#[codec(type_id = N)]` with reserved gaps (12–20 apricot, 23–26
/// banff, 27/28 are the `signer` types, 33+ durango/etna/helicon).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum UnsignedTx {
    /// `AddValidatorTx` (type_id 12).
    #[codec(type_id = 12)]
    AddValidator(AddValidatorTx),
    /// `AddSubnetValidatorTx` (type_id 13).
    #[codec(type_id = 13)]
    AddSubnetValidator(AddSubnetValidatorTx),
    /// `AddDelegatorTx` (type_id 14).
    #[codec(type_id = 14)]
    AddDelegator(AddDelegatorTx),
    /// `CreateChainTx` (type_id 15).
    #[codec(type_id = 15)]
    CreateChain(CreateChainTx),
    /// `CreateSubnetTx` (type_id 16).
    #[codec(type_id = 16)]
    CreateSubnet(CreateSubnetTx),
    /// `ImportTx` (type_id 17).
    #[codec(type_id = 17)]
    Import(ImportTx),
    /// `ExportTx` (type_id 18).
    #[codec(type_id = 18)]
    Export(ExportTx),
    /// `AdvanceTimeTx` (type_id 19).
    #[codec(type_id = 19)]
    AdvanceTime(AdvanceTimeTx),
    /// `RewardValidatorTx` (type_id 20).
    #[codec(type_id = 20)]
    RewardValidator(RewardValidatorTx),
    /// `RemoveSubnetValidatorTx` (type_id 23).
    #[codec(type_id = 23)]
    RemoveSubnetValidator(RemoveSubnetValidatorTx),
    /// `TransformSubnetTx` (type_id 24).
    #[codec(type_id = 24)]
    TransformSubnet(TransformSubnetTx),
    /// `AddPermissionlessValidatorTx` (type_id 25).
    #[codec(type_id = 25)]
    AddPermissionlessValidator(AddPermissionlessValidatorTx),
    /// `AddPermissionlessDelegatorTx` (type_id 26).
    #[codec(type_id = 26)]
    AddPermissionlessDelegator(AddPermissionlessDelegatorTx),
    /// `TransferSubnetOwnershipTx` (type_id 33).
    #[codec(type_id = 33)]
    TransferSubnetOwnership(TransferSubnetOwnershipTx),
    /// `BaseTx` (type_id 34).
    #[codec(type_id = 34)]
    Base(BaseTx),
    /// `ConvertSubnetToL1Tx` (type_id 35).
    #[codec(type_id = 35)]
    ConvertSubnetToL1(ConvertSubnetToL1Tx),
    /// `RegisterL1ValidatorTx` (type_id 36).
    #[codec(type_id = 36)]
    RegisterL1Validator(RegisterL1ValidatorTx),
    /// `SetL1ValidatorWeightTx` (type_id 37).
    #[codec(type_id = 37)]
    SetL1ValidatorWeight(SetL1ValidatorWeightTx),
    /// `IncreaseL1ValidatorBalanceTx` (type_id 38).
    #[codec(type_id = 38)]
    IncreaseL1ValidatorBalance(IncreaseL1ValidatorBalanceTx),
    /// `DisableL1ValidatorTx` (type_id 39).
    #[codec(type_id = 39)]
    DisableL1Validator(DisableL1ValidatorTx),
    /// `AddAutoRenewedValidatorTx` (type_id 40).
    #[codec(type_id = 40)]
    AddAutoRenewedValidator(AddAutoRenewedValidatorTx),
    /// `SetAutoRenewedValidatorConfigTx` (type_id 41).
    #[codec(type_id = 41)]
    SetAutoRenewedValidatorConfig(SetAutoRenewedValidatorConfigTx),
    /// `RewardAutoRenewedValidatorTx` (type_id 42).
    #[codec(type_id = 42)]
    RewardAutoRenewedValidator(RewardAutoRenewedValidatorTx),
}

impl Default for UnsignedTx {
    fn default() -> Self {
        UnsignedTx::Base(BaseTx)
    }
}

impl UnsignedTx {
    /// The avax `TransferableInput`s this tx consumes (the `BaseTx.ins` plus any
    /// tx-specific inputs, e.g. `ImportTx.imported_inputs`).
    ///
    /// Stubbed at M4.2; returns an empty slice until the per-tx structs carry
    /// their fields (M4.3).
    #[must_use]
    pub fn inputs(&self) -> &[TransferableInput] {
        &[]
    }

    /// The avax `TransferableOutput`s this tx produces.
    ///
    /// Stubbed at M4.2 (see [`UnsignedTx::inputs`]).
    #[must_use]
    pub fn outputs(&self) -> &[TransferableOutput] {
        &[]
    }

    /// The set of UTXO IDs this tx consumes (`Tx.InputIDs`).
    ///
    /// Stubbed at M4.2; returns an empty set until inputs carry UTXO IDs (M4.3).
    #[must_use]
    pub fn input_ids(&self) -> std::collections::BTreeSet<ava_types::id::Id> {
        std::collections::BTreeSet::new()
    }

    /// Dispatches to the matching [`Visitor`] method for this variant
    /// (`txs.Visitor`, specs 08 §2.4).
    ///
    /// # Errors
    /// Propagates the visitor method's error; the default `Visitor` impls return
    /// [`Error::WrongTxType`] for unhandled variants.
    pub fn visit<V: Visitor>(&self, v: &mut V) -> Result<(), V::Error> {
        match self {
            UnsignedTx::AddValidator(tx) => v.add_validator(tx),
            UnsignedTx::AddSubnetValidator(tx) => v.add_subnet_validator(tx),
            UnsignedTx::AddDelegator(tx) => v.add_delegator(tx),
            UnsignedTx::CreateChain(tx) => v.create_chain(tx),
            UnsignedTx::CreateSubnet(tx) => v.create_subnet(tx),
            UnsignedTx::Import(tx) => v.import(tx),
            UnsignedTx::Export(tx) => v.export(tx),
            UnsignedTx::AdvanceTime(tx) => v.advance_time(tx),
            UnsignedTx::RewardValidator(tx) => v.reward_validator(tx),
            UnsignedTx::RemoveSubnetValidator(tx) => v.remove_subnet_validator(tx),
            UnsignedTx::TransformSubnet(tx) => v.transform_subnet(tx),
            UnsignedTx::AddPermissionlessValidator(tx) => v.add_permissionless_validator(tx),
            UnsignedTx::AddPermissionlessDelegator(tx) => v.add_permissionless_delegator(tx),
            UnsignedTx::TransferSubnetOwnership(tx) => v.transfer_subnet_ownership(tx),
            UnsignedTx::Base(tx) => v.base(tx),
            UnsignedTx::ConvertSubnetToL1(tx) => v.convert_subnet_to_l1(tx),
            UnsignedTx::RegisterL1Validator(tx) => v.register_l1_validator(tx),
            UnsignedTx::SetL1ValidatorWeight(tx) => v.set_l1_validator_weight(tx),
            UnsignedTx::IncreaseL1ValidatorBalance(tx) => v.increase_l1_validator_balance(tx),
            UnsignedTx::DisableL1Validator(tx) => v.disable_l1_validator(tx),
            UnsignedTx::AddAutoRenewedValidator(tx) => v.add_auto_renewed_validator(tx),
            UnsignedTx::SetAutoRenewedValidatorConfig(tx) => {
                v.set_auto_renewed_validator_config(tx)
            }
            UnsignedTx::RewardAutoRenewedValidator(tx) => v.reward_auto_renewed_validator(tx),
        }
    }
}

// ---------------------------------------------------------------------------
// Visitor trait
// ---------------------------------------------------------------------------

/// `txs.Visitor` — the executor dispatch interface (specs 08 §2.4).
///
/// Each method defaults to [`Error::WrongTxType`] (Go `errWrongTxType`); an
/// executor overrides only the variants it accepts. `Self::Error` lets an
/// executor use its own error type while still mapping the default to a
/// wrong-type sentinel via the [`WrongTxType`] bound.
pub trait Visitor {
    /// The executor's error type. Must be constructible from the wrong-tx-type
    /// sentinel so the default methods can reject unhandled variants.
    type Error: WrongTxType;

    /// `AddValidatorTx` (default: wrong type).
    fn add_validator(&mut self, _tx: &AddValidatorTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `AddSubnetValidatorTx` (default: wrong type).
    fn add_subnet_validator(&mut self, _tx: &AddSubnetValidatorTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `AddDelegatorTx` (default: wrong type).
    fn add_delegator(&mut self, _tx: &AddDelegatorTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `CreateChainTx` (default: wrong type).
    fn create_chain(&mut self, _tx: &CreateChainTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `CreateSubnetTx` (default: wrong type).
    fn create_subnet(&mut self, _tx: &CreateSubnetTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `ImportTx` (default: wrong type).
    fn import(&mut self, _tx: &ImportTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `ExportTx` (default: wrong type).
    fn export(&mut self, _tx: &ExportTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `AdvanceTimeTx` (default: wrong type).
    fn advance_time(&mut self, _tx: &AdvanceTimeTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `RewardValidatorTx` (default: wrong type).
    fn reward_validator(&mut self, _tx: &RewardValidatorTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `RemoveSubnetValidatorTx` (default: wrong type).
    fn remove_subnet_validator(
        &mut self,
        _tx: &RemoveSubnetValidatorTx,
    ) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `TransformSubnetTx` (default: wrong type).
    fn transform_subnet(&mut self, _tx: &TransformSubnetTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `AddPermissionlessValidatorTx` (default: wrong type).
    fn add_permissionless_validator(
        &mut self,
        _tx: &AddPermissionlessValidatorTx,
    ) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `AddPermissionlessDelegatorTx` (default: wrong type).
    fn add_permissionless_delegator(
        &mut self,
        _tx: &AddPermissionlessDelegatorTx,
    ) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `TransferSubnetOwnershipTx` (default: wrong type).
    fn transfer_subnet_ownership(
        &mut self,
        _tx: &TransferSubnetOwnershipTx,
    ) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `BaseTx` (default: wrong type).
    fn base(&mut self, _tx: &BaseTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `ConvertSubnetToL1Tx` (default: wrong type).
    fn convert_subnet_to_l1(&mut self, _tx: &ConvertSubnetToL1Tx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `RegisterL1ValidatorTx` (default: wrong type).
    fn register_l1_validator(&mut self, _tx: &RegisterL1ValidatorTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `SetL1ValidatorWeightTx` (default: wrong type).
    fn set_l1_validator_weight(&mut self, _tx: &SetL1ValidatorWeightTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `IncreaseL1ValidatorBalanceTx` (default: wrong type).
    fn increase_l1_validator_balance(
        &mut self,
        _tx: &IncreaseL1ValidatorBalanceTx,
    ) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `DisableL1ValidatorTx` (default: wrong type).
    fn disable_l1_validator(&mut self, _tx: &DisableL1ValidatorTx) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `AddAutoRenewedValidatorTx` (default: wrong type).
    fn add_auto_renewed_validator(
        &mut self,
        _tx: &AddAutoRenewedValidatorTx,
    ) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `SetAutoRenewedValidatorConfigTx` (default: wrong type).
    fn set_auto_renewed_validator_config(
        &mut self,
        _tx: &SetAutoRenewedValidatorConfigTx,
    ) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
    /// `RewardAutoRenewedValidatorTx` (default: wrong type).
    fn reward_auto_renewed_validator(
        &mut self,
        _tx: &RewardAutoRenewedValidatorTx,
    ) -> Result<(), Self::Error> {
        Err(Self::Error::wrong_tx_type())
    }
}

/// Lets a [`Visitor::Error`] be constructed from the wrong-tx-type sentinel so
/// the default [`Visitor`] methods can reject unhandled variants generically.
pub trait WrongTxType {
    /// The `errWrongTxType` sentinel for this error type.
    fn wrong_tx_type() -> Self;
}

impl WrongTxType for Error {
    fn wrong_tx_type() -> Self {
        Error::WrongTxType
    }
}

#[cfg(test)]
mod golden {
    use ava_codec::Serializable;

    use super::*;

    /// Asserts every `UnsignedTx` variant's codec `type_id` equals the specs 08
    /// §2.1 table value, plus the shared block `type_id`s and the secp256k1fx /
    /// stakeable / signer positions in the registry.
    #[test]
    fn type_id_table() {
        // --- block type_ids (shared numbering space, specs 08 §2.1 / §4.1) ---
        assert_eq!(TYPE_ID_APRICOT_PROPOSAL_BLOCK, 0);
        assert_eq!(TYPE_ID_APRICOT_ABORT_BLOCK, 1);
        assert_eq!(TYPE_ID_APRICOT_COMMIT_BLOCK, 2);
        assert_eq!(TYPE_ID_APRICOT_STANDARD_BLOCK, 3);
        assert_eq!(TYPE_ID_APRICOT_ATOMIC_BLOCK, 4);
        assert_eq!(TYPE_ID_BANFF_PROPOSAL_BLOCK, 29);
        assert_eq!(TYPE_ID_BANFF_ABORT_BLOCK, 30);
        assert_eq!(TYPE_ID_BANFF_COMMIT_BLOCK, 31);
        assert_eq!(TYPE_ID_BANFF_STANDARD_BLOCK, 32);

        // --- secp256k1fx / stakeable / signer positions in the registry ---
        // These are asserted via the registration-order assigner the codec
        // module builds (codec::type_id_registry); ids 5,7,9,10,11 + the
        // MintInput(6)/MintOutput(8) gaps, stakeable 21,22, signer 27,28.
        let table = codec::type_id_registry_table();
        let lookup = |name: &str| -> u32 {
            table
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| panic!("{name} not registered"))
        };
        assert_eq!(lookup("secp256k1fx.TransferInput"), 5);
        assert_eq!(lookup("secp256k1fx.TransferOutput"), 7);
        assert_eq!(lookup("secp256k1fx.Credential"), 9);
        assert_eq!(lookup("secp256k1fx.Input"), 10);
        assert_eq!(lookup("secp256k1fx.OutputOwners"), 11);
        assert_eq!(lookup("stakeable.LockIn"), 21);
        assert_eq!(lookup("stakeable.LockOut"), 22);
        assert_eq!(lookup("signer.Empty"), 27);
        assert_eq!(lookup("signer.ProofOfPossession"), 28);

        // The tx types occupy the same registry positions as their enum
        // `#[codec(type_id = N)]`; cross-check a couple against the assigner.
        assert_eq!(lookup("AddValidatorTx"), 12);
        assert_eq!(lookup("RewardValidatorTx"), 20);
        assert_eq!(lookup("RemoveSubnetValidatorTx"), 23);
        assert_eq!(lookup("AddPermissionlessDelegatorTx"), 26);
        assert_eq!(lookup("TransferSubnetOwnershipTx"), 33);
        assert_eq!(lookup("BaseTx"), 34);
        assert_eq!(lookup("RewardAutoRenewedValidatorTx"), 42);

        // --- UnsignedTx enum discriminants (the derive-generated codec_type_id) ---
        let cases: &[(UnsignedTx, u32)] = &[
            (UnsignedTx::AddValidator(AddValidatorTx), 12),
            (UnsignedTx::AddSubnetValidator(AddSubnetValidatorTx), 13),
            (UnsignedTx::AddDelegator(AddDelegatorTx), 14),
            (UnsignedTx::CreateChain(CreateChainTx), 15),
            (UnsignedTx::CreateSubnet(CreateSubnetTx), 16),
            (UnsignedTx::Import(ImportTx), 17),
            (UnsignedTx::Export(ExportTx), 18),
            (UnsignedTx::AdvanceTime(AdvanceTimeTx), 19),
            (UnsignedTx::RewardValidator(RewardValidatorTx), 20),
            (
                UnsignedTx::RemoveSubnetValidator(RemoveSubnetValidatorTx),
                23,
            ),
            (UnsignedTx::TransformSubnet(TransformSubnetTx), 24),
            (
                UnsignedTx::AddPermissionlessValidator(AddPermissionlessValidatorTx),
                25,
            ),
            (
                UnsignedTx::AddPermissionlessDelegator(AddPermissionlessDelegatorTx),
                26,
            ),
            (
                UnsignedTx::TransferSubnetOwnership(TransferSubnetOwnershipTx),
                33,
            ),
            (UnsignedTx::Base(BaseTx), 34),
            (UnsignedTx::ConvertSubnetToL1(ConvertSubnetToL1Tx), 35),
            (UnsignedTx::RegisterL1Validator(RegisterL1ValidatorTx), 36),
            (UnsignedTx::SetL1ValidatorWeight(SetL1ValidatorWeightTx), 37),
            (
                UnsignedTx::IncreaseL1ValidatorBalance(IncreaseL1ValidatorBalanceTx),
                38,
            ),
            (UnsignedTx::DisableL1Validator(DisableL1ValidatorTx), 39),
            (
                UnsignedTx::AddAutoRenewedValidator(AddAutoRenewedValidatorTx),
                40,
            ),
            (
                UnsignedTx::SetAutoRenewedValidatorConfig(SetAutoRenewedValidatorConfigTx),
                41,
            ),
            (
                UnsignedTx::RewardAutoRenewedValidator(RewardAutoRenewedValidatorTx),
                42,
            ),
        ];
        for (variant, want) in cases {
            assert_eq!(
                variant.codec_type_id(),
                *want,
                "type_id mismatch for {variant:?}"
            );
            // The wire encoding writes the u32 typeID first; confirm the marshaled
            // prefix matches the discriminant for a representative variant.
            let mut p = ava_codec::packer::Packer::with_max_size(64);
            variant.marshal_into(&mut p);
            let bytes = p.into_bytes();
            assert_eq!(
                bytes.get(..4),
                Some(want.to_be_bytes().as_slice()),
                "wire typeID prefix"
            );
        }
    }
}
