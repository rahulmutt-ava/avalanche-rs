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
//! The per-tx structs carry their real fields and verification rules (M4.3 /
//! M4.4); each lives in its own module and is re-exported here.

use ava_codec::AvaCodec;

use crate::error::Error;

pub mod add_delegator;
pub mod add_permissionless_delegator;
pub mod add_permissionless_validator;
pub mod add_subnet_validator;
pub mod add_validator;
pub mod advance_time;
pub mod auto_renew;
pub mod base_tx;
pub mod codec;
pub mod components;
pub mod convert_subnet_to_l1;
pub mod create_chain;
pub mod create_subnet;
pub mod disable_l1_validator;
pub mod executor;
pub mod fee;
pub mod import_export;
pub mod increase_l1_validator_balance;
pub mod priorities;
pub mod register_l1_validator;
pub mod remove_subnet_validator;
pub mod reward_validator;
pub mod set_l1_validator_weight;
pub mod transfer_subnet_ownership;
pub mod transform_subnet;
pub mod tx;
pub mod validator;

pub use add_delegator::AddDelegatorTx;
pub use add_permissionless_delegator::AddPermissionlessDelegatorTx;
pub use add_permissionless_validator::AddPermissionlessValidatorTx;
pub use add_subnet_validator::AddSubnetValidatorTx;
pub use add_validator::AddValidatorTx;
pub use advance_time::AdvanceTimeTx;
pub use auto_renew::{
    AddAutoRenewedValidatorTx, RewardAutoRenewedValidatorTx, SetAutoRenewedValidatorConfigTx,
};
pub use base_tx::BaseTx;
pub use codec::{Codec, GenesisCodec};
pub use components::{TransferableInput, TransferableOutput};
pub use convert_subnet_to_l1::{ConvertSubnetToL1Tx, ConvertSubnetToL1Validator};
pub use create_chain::CreateChainTx;
pub use create_subnet::CreateSubnetTx;
pub use disable_l1_validator::DisableL1ValidatorTx;
pub use import_export::{ExportTx, ImportTx};
pub use increase_l1_validator_balance::IncreaseL1ValidatorBalanceTx;
pub use priorities::Priority;
pub use register_l1_validator::RegisterL1ValidatorTx;
pub use remove_subnet_validator::RemoveSubnetValidatorTx;
pub use reward_validator::RewardValidatorTx;
pub use set_l1_validator_weight::SetL1ValidatorWeightTx;
pub use transfer_subnet_ownership::TransferSubnetOwnershipTx;
pub use transform_subnet::TransformSubnetTx;
pub use tx::{Credential, Tx};
pub use validator::{SubnetValidator, Validator};

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
        UnsignedTx::Base(BaseTx::default())
    }
}

impl UnsignedTx {
    /// The embedded `avax.BaseTx`, if this tx has one (every tx except the two
    /// Apricot proposal txs and `RewardAutoRenewedValidatorTx`).
    #[must_use]
    pub fn base(&self) -> Option<&components::BaseTx> {
        match self {
            UnsignedTx::AddValidator(tx) => Some(&tx.base.base),
            UnsignedTx::AddSubnetValidator(tx) => Some(&tx.base.base),
            UnsignedTx::AddDelegator(tx) => Some(&tx.base.base),
            UnsignedTx::CreateChain(tx) => Some(&tx.base.base),
            UnsignedTx::CreateSubnet(tx) => Some(&tx.base.base),
            UnsignedTx::Import(tx) => Some(&tx.base.base),
            UnsignedTx::Export(tx) => Some(&tx.base.base),
            UnsignedTx::RemoveSubnetValidator(tx) => Some(&tx.base.base),
            UnsignedTx::TransformSubnet(tx) => Some(&tx.base.base),
            UnsignedTx::AddPermissionlessValidator(tx) => Some(&tx.base.base),
            UnsignedTx::AddPermissionlessDelegator(tx) => Some(&tx.base.base),
            UnsignedTx::TransferSubnetOwnership(tx) => Some(&tx.base.base),
            UnsignedTx::Base(tx) => Some(&tx.base),
            UnsignedTx::ConvertSubnetToL1(tx) => Some(&tx.base.base),
            UnsignedTx::RegisterL1Validator(tx) => Some(&tx.base.base),
            UnsignedTx::SetL1ValidatorWeight(tx) => Some(&tx.base.base),
            UnsignedTx::IncreaseL1ValidatorBalance(tx) => Some(&tx.base.base),
            UnsignedTx::DisableL1Validator(tx) => Some(&tx.base.base),
            UnsignedTx::AddAutoRenewedValidator(tx) => Some(&tx.base.base),
            UnsignedTx::SetAutoRenewedValidatorConfig(tx) => Some(&tx.base.base),
            UnsignedTx::AdvanceTime(_)
            | UnsignedTx::RewardValidator(_)
            | UnsignedTx::RewardAutoRenewedValidator(_) => None,
        }
    }

    /// The `avax.TransferableInput`s this tx consumes from the embedded
    /// `BaseTx` (the `BaseTx.ins`). Tx-specific extra inputs (e.g.
    /// `ImportTx.imported_inputs`) are surfaced through [`UnsignedTx::input_ids`].
    #[must_use]
    pub fn inputs(&self) -> &[TransferableInput] {
        self.base().map_or(&[], |b| b.ins.as_slice())
    }

    /// The `avax.TransferableOutput`s this tx's embedded `BaseTx` produces.
    #[must_use]
    pub fn outputs(&self) -> &[TransferableOutput] {
        self.base().map_or(&[], |b| b.outs.as_slice())
    }

    /// The set of UTXO IDs this tx consumes (`Tx.InputIDs`) — the `BaseTx.ins`
    /// plus the `ImportTx.imported_inputs`.
    #[must_use]
    pub fn input_ids(&self) -> std::collections::BTreeSet<ava_types::id::Id> {
        let mut ids: std::collections::BTreeSet<ava_types::id::Id> = self
            .inputs()
            .iter()
            .map(components::TransferableInput::input_id)
            .collect();
        if let UnsignedTx::Import(tx) = self {
            ids.extend(
                tx.imported_inputs
                    .iter()
                    .map(components::TransferableInput::input_id),
            );
        }
        ids
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
        let cases: Vec<(UnsignedTx, u32)> = vec![
            (UnsignedTx::AddValidator(AddValidatorTx::default()), 12),
            (
                UnsignedTx::AddSubnetValidator(AddSubnetValidatorTx::default()),
                13,
            ),
            (UnsignedTx::AddDelegator(AddDelegatorTx::default()), 14),
            (UnsignedTx::CreateChain(CreateChainTx::default()), 15),
            (UnsignedTx::CreateSubnet(CreateSubnetTx::default()), 16),
            (UnsignedTx::Import(ImportTx::default()), 17),
            (UnsignedTx::Export(ExportTx::default()), 18),
            (UnsignedTx::AdvanceTime(AdvanceTimeTx::default()), 19),
            (
                UnsignedTx::RewardValidator(RewardValidatorTx::default()),
                20,
            ),
            (
                UnsignedTx::RemoveSubnetValidator(RemoveSubnetValidatorTx::default()),
                23,
            ),
            (
                UnsignedTx::TransformSubnet(TransformSubnetTx::default()),
                24,
            ),
            (
                UnsignedTx::AddPermissionlessValidator(AddPermissionlessValidatorTx::default()),
                25,
            ),
            (
                UnsignedTx::AddPermissionlessDelegator(AddPermissionlessDelegatorTx::default()),
                26,
            ),
            (
                UnsignedTx::TransferSubnetOwnership(TransferSubnetOwnershipTx::default()),
                33,
            ),
            (UnsignedTx::Base(BaseTx::default()), 34),
            (
                UnsignedTx::ConvertSubnetToL1(ConvertSubnetToL1Tx::default()),
                35,
            ),
            (
                UnsignedTx::RegisterL1Validator(RegisterL1ValidatorTx::default()),
                36,
            ),
            (
                UnsignedTx::SetL1ValidatorWeight(SetL1ValidatorWeightTx::default()),
                37,
            ),
            (
                UnsignedTx::IncreaseL1ValidatorBalance(IncreaseL1ValidatorBalanceTx::default()),
                38,
            ),
            (
                UnsignedTx::DisableL1Validator(DisableL1ValidatorTx::default()),
                39,
            ),
            (
                UnsignedTx::AddAutoRenewedValidator(AddAutoRenewedValidatorTx::default()),
                40,
            ),
            (
                UnsignedTx::SetAutoRenewedValidatorConfig(
                    SetAutoRenewedValidatorConfigTx::default(),
                ),
                41,
            ),
            (
                UnsignedTx::RewardAutoRenewedValidator(RewardAutoRenewedValidatorTx::default()),
                42,
            ),
        ];
        for (variant, want) in &cases {
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

    use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_types::short_id::ShortId;

    use crate::signer::{ProofOfPossession, Signer};
    use crate::stakeable::{LockIn, LockOut};
    use crate::txs::components::{
        self, BaseTx as AvaxBaseTx, Input, Output, Owner, TransferableInput, TransferableOutput,
    };

    /// The Mainnet AVAX asset id used throughout the Go serialization vectors.
    const AVAX_ASSET_ID: [u8; 32] = [
        0x21, 0xe6, 0x73, 0x17, 0xcb, 0xc4, 0xbe, 0x2a, 0xeb, 0x00, 0x67, 0x7a, 0xd6, 0x46, 0x27,
        0x78, 0xa8, 0xf5, 0x22, 0x74, 0xb9, 0xd6, 0x05, 0xdf, 0x25, 0x91, 0xb2, 0x30, 0x27, 0xa8,
        0x7d, 0xff,
    ];
    const CUSTOM_ASSET_ID: [u8; 32] = [
        0x99, 0x77, 0x55, 0x77, 0x11, 0x33, 0x55, 0x31, 0x99, 0x77, 0x55, 0x77, 0x11, 0x33, 0x55,
        0x31, 0x99, 0x77, 0x55, 0x77, 0x11, 0x33, 0x55, 0x31, 0x99, 0x77, 0x55, 0x77, 0x11, 0x33,
        0x55, 0x31,
    ];
    const TX_ID: [u8; 32] = [
        0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99,
        0x88, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa,
        0x99, 0x88,
    ];
    const ADDR: [u8; 20] = [
        0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
        0xbb, 0x44, 0x55, 0x66, 0x77,
    ];
    /// The BLS compressed public key from the Go vectors (`localsigner.FromBytes`
    /// of the fixed test secret key).
    const BLS_PUBKEY: [u8; 48] = [
        0xaf, 0xf4, 0xac, 0xb4, 0xc5, 0x43, 0x9b, 0x5d, 0x42, 0x6c, 0xad, 0xf9, 0xe9, 0x46, 0xd3,
        0xa4, 0x52, 0xf7, 0xde, 0x34, 0x14, 0xd1, 0xad, 0x27, 0x33, 0x61, 0x33, 0x21, 0x1d, 0x8b,
        0x90, 0xcf, 0x49, 0xfb, 0x97, 0xee, 0xbc, 0xde, 0xee, 0xf7, 0x14, 0xdc, 0x20, 0xf5, 0x4e,
        0xd0, 0xd4, 0xd1,
    ];
    /// The BLS proof-of-possession signature from the Go vectors.
    const BLS_SIG: [u8; 96] = [
        0x8c, 0xfd, 0x79, 0x09, 0xd1, 0x53, 0xb9, 0x60, 0x4b, 0x62, 0xb1, 0x43, 0xba, 0x36, 0x20,
        0x7b, 0xb7, 0xe6, 0x48, 0x67, 0x42, 0x44, 0x80, 0x20, 0x2a, 0x67, 0xdc, 0x68, 0x76, 0x83,
        0x46, 0xd9, 0x5c, 0x90, 0x98, 0x3c, 0x2d, 0x27, 0x9c, 0x64, 0xc4, 0x3c, 0x51, 0x13, 0x6b,
        0x2a, 0x05, 0xe0, 0x16, 0x02, 0xd5, 0x2a, 0xa6, 0x37, 0x6f, 0xda, 0x17, 0xfa, 0x6e, 0x2a,
        0x18, 0xa0, 0x83, 0xe4, 0x9d, 0x9c, 0x45, 0x0e, 0xab, 0x7b, 0x89, 0xb1, 0xd5, 0x55, 0x5d,
        0xa5, 0xc4, 0x89, 0x87, 0x2e, 0x02, 0xb7, 0xe5, 0x22, 0x7b, 0x77, 0x55, 0x0a, 0xf1, 0x33,
        0x0e, 0x5a, 0x71, 0xf8, 0xc3, 0x68,
    ];

    fn id(bytes: [u8; 32]) -> Id {
        Id::from(bytes)
    }

    fn owners_one_addr() -> OutputOwners {
        OutputOwners::new(0, 1, vec![ShortId::from(ADDR)])
    }

    /// TDD ENTRY POINT (M4.3). Reproduces the Go
    /// `TestAddPermissionlessPrimaryValidator` "simple" serialization vector
    /// (`vms/platformvm/txs/add_permissionless_validator_tx_test.go`,
    /// `expectedUnsignedSimpleAddPrimaryTxBytes`): a Primary-Network
    /// `AddPermissionlessValidatorTx` with a BLS PoP signer, one input, one
    /// stake output, and reward owners. Asserts byte-exact `Codec.Marshal`,
    /// round-trip decode, and that `syntactic_verify` passes.
    #[test]
    fn pchain_tx_codec_app_validator() {
        const KILO_AVAX: u64 = 2_000 * 1_000_000_000; // 2k AVAX in nAVAX.

        let tx = AddPermissionlessValidatorTx {
            base: BaseTx::new(AvaxBaseTx {
                network_id: 1, // Mainnet.
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![TransferableInput {
                    tx_id: id(TX_ID),
                    output_index: 1,
                    asset_id: id(AVAX_ASSET_ID),
                    r#in: Input::Transfer(TransferInput::new(KILO_AVAX, vec![1])),
                }],
                memo: vec![],
            }),
            validator: Validator {
                node_id: NodeId::from([
                    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x11, 0x22, 0x33, 0x44, 0x55,
                    0x66, 0x77, 0x88, 0x11, 0x22, 0x33, 0x44,
                ]),
                start: 12345,
                end: 12345 + 200 * 24 * 60 * 60,
                wght: KILO_AVAX,
            },
            subnet: Id::EMPTY, // Primary Network.
            signer: Signer::ProofOfPossession(ProofOfPossession::new(BLS_PUBKEY, BLS_SIG)),
            stake_outs: vec![TransferableOutput {
                asset_id: id(AVAX_ASSET_ID),
                out: Output::Transfer(TransferOutput::new(KILO_AVAX, owners_one_addr())),
            }],
            validator_rewards_owner: Owner::Secp256k1(owners_one_addr()),
            delegator_rewards_owner: Owner::Secp256k1(owners_one_addr()),
            delegation_shares: 1_000_000, // reward.PercentDenominator
            verified: std::cell::OnceCell::new(),
        };

        // Syntactic verification passes for a well-formed primary validator.
        tx.syntactic_verify().expect("syntactic verify");

        let unsigned = UnsignedTx::AddPermissionlessValidator(tx);
        let c = codec::codec().expect("codec");
        let got = c.marshal(crate::CODEC_VERSION, &unsigned).expect("marshal");

        let expected = expected_app_validator_bytes();
        assert_eq!(got, expected, "byte-exact Codec.Marshal");

        // encode(decode(bytes)) == bytes.
        let mut decoded = UnsignedTx::default();
        c.unmarshal(&got, &mut decoded).expect("unmarshal");
        assert_eq!(decoded, unsigned, "round-trip equality");
        let reencoded = c
            .marshal(crate::CODEC_VERSION, &decoded)
            .expect("re-marshal");
        assert_eq!(reencoded, expected, "encode(decode(bytes)) == bytes");
    }

    /// TDD ENTRY POINT (M4.4). Reproduces the Go
    /// `TestRegisterL1ValidatorTxSerialization` vector
    /// (`vms/platformvm/txs/register_l1_validator_tx_test.go`, `expectedBytes`):
    /// a `RegisterL1ValidatorTx` exercising `stakeable.LockOut` (22),
    /// `stakeable.LockIn` (21), multiple inputs/outputs, a memo, a BLS PoP, and
    /// a Warp message. Asserts byte-exact `Codec.Marshal` + round-trip.
    #[test]
    fn pchain_tx_codec_l1() {
        let out0 = TransferableOutput {
            asset_id: id(AVAX_ASSET_ID),
            out: Output::StakeableLock(LockOut::new(
                87_654_321,
                Output::Transfer(TransferOutput::new(
                    1,
                    OutputOwners::new(12_345_678, 0, vec![]),
                )),
            )),
        };
        let out1 = TransferableOutput {
            asset_id: id(CUSTOM_ASSET_ID),
            out: Output::StakeableLock(LockOut::new(
                876_543_210,
                Output::Transfer(TransferOutput::new(
                    0xffff_ffff_ffff_ffff,
                    owners_one_addr(),
                )),
            )),
        };
        let in0 = TransferableInput {
            tx_id: id(TX_ID),
            output_index: 1,
            asset_id: id(AVAX_ASSET_ID),
            r#in: Input::Transfer(TransferInput::new(1_000_000_000, vec![2, 5])),
        };
        let in1 = TransferableInput {
            tx_id: id(TX_ID),
            output_index: 2,
            asset_id: id(CUSTOM_ASSET_ID),
            r#in: Input::StakeableLock(LockIn::new(
                876_543_210,
                Input::Transfer(TransferInput::new(0xefff_ffff_ffff_ffff, vec![0])),
            )),
        };
        let in2 = TransferableInput {
            tx_id: id(TX_ID),
            output_index: 3,
            asset_id: id(CUSTOM_ASSET_ID),
            r#in: Input::Transfer(TransferInput::new(0x1000_0000_0000_0000, vec![])),
        };

        let tx = RegisterL1ValidatorTx {
            base: BaseTx::new(AvaxBaseTx {
                network_id: 10, // constants.UnitTestID
                blockchain_id: Id::EMPTY,
                outs: vec![out0, out1],
                ins: vec![in0, in1, in2],
                memo: "😅\nwell that's\x01\x23\x45!".as_bytes().to_vec(),
            }),
            balance: 1_000_000_000, // units.Avax
            proof_of_possession: BLS_SIG,
            message: b"message".to_vec(),
        };

        let unsigned = UnsignedTx::RegisterL1Validator(tx);
        let c = codec::codec().expect("codec");
        let got = c.marshal(crate::CODEC_VERSION, &unsigned).expect("marshal");

        let expected = expected_register_l1_bytes();
        assert_eq!(got, expected, "byte-exact Codec.Marshal");

        let mut decoded = UnsignedTx::default();
        c.unmarshal(&got, &mut decoded).expect("unmarshal");
        assert_eq!(decoded, unsigned, "round-trip equality");

        // The components helpers see the stakeable wrappers as the registered
        // interface types (sanity check on the type-id dispatch).
        let _ = components::is_sorted_transferable_outputs(unsigned.outputs());
    }

    /// `expectedUnsignedSimpleAddPrimaryTxBytes` from the Go test (verbatim).
    fn expected_app_validator_bytes() -> Vec<u8> {
        let mut v = vec![
            0x00, 0x00, // codec version
            0x00, 0x00, 0x00, 0x19, // AddPermissionlessValidatorTx type id (25)
            0x00, 0x00, 0x00, 0x01, // network id = 1
        ];
        v.extend_from_slice(&[0u8; 32]); // blockchain id (P-Chain)
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // num outputs
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num inputs
        v.extend_from_slice(&TX_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // output index
        v.extend_from_slice(&AVAX_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]); // TransferInput type id
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xd1, 0xa9, 0x4a, 0x20, 0x00]); // amount
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num sig indices
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // sig index
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // memo len
        v.extend_from_slice(&[
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88, 0x11, 0x22, 0x33, 0x44,
        ]); // node id
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x39]); // start
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x01, 0x07, 0xdc, 0x39]); // end
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xd1, 0xa9, 0x4a, 0x20, 0x00]); // weight
        v.extend_from_slice(&[0u8; 32]); // primary network subnet id
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x1c]); // BLS PoP type id (28)
        v.extend_from_slice(&BLS_PUBKEY);
        v.extend_from_slice(&BLS_SIG);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num stake outs
        v.extend_from_slice(&AVAX_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]); // TransferOutput type id
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xd1, 0xa9, 0x4a, 0x20, 0x00]); // amount
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // locktime
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // threshold
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // num addrs
        v.extend_from_slice(&ADDR);
        // validator rewards owner (OutputOwners, type id 11)
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x0b]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&ADDR);
        // delegator rewards owner (OutputOwners, type id 11)
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x0b]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&ADDR);
        v.extend_from_slice(&[0x00, 0x0f, 0x42, 0x40]); // delegation shares = 1_000_000
        v
    }

    /// `expectedBytes` from `TestRegisterL1ValidatorTxSerialization` (verbatim).
    fn expected_register_l1_bytes() -> Vec<u8> {
        let mut v = vec![
            0x00, 0x00, // codec version
            0x00, 0x00, 0x00, 0x24, // RegisterL1ValidatorTx type id (36)
            0x00, 0x00, 0x00, 0x0a, // network id = 10
        ];
        v.extend_from_slice(&[0u8; 32]); // blockchain id
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // num outputs
        // outputs[0]
        v.extend_from_slice(&AVAX_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x16]); // LockOut type id (22)
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x05, 0x39, 0x7f, 0xb1]); // lock locktime
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]); // TransferOutput type id
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]); // amount
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0xbc, 0x61, 0x4e]); // owner locktime
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // threshold
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // num addrs
        // outputs[1]
        v.extend_from_slice(&CUSTOM_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x16]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x34, 0x3e, 0xfc, 0xea]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]);
        v.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&ADDR);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x03]); // num inputs
        // inputs[0]
        v.extend_from_slice(&TX_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&AVAX_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x3b, 0x9a, 0xca, 0x00]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
        // inputs[1]
        v.extend_from_slice(&TX_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
        v.extend_from_slice(&CUSTOM_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x15]); // LockIn type id (21)
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x34, 0x3e, 0xfc, 0xea]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
        v.extend_from_slice(&[0xef, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        // inputs[2]
        v.extend_from_slice(&TX_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x03]);
        v.extend_from_slice(&CUSTOM_ASSET_ID);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
        v.extend_from_slice(&[0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        // memo
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x14]); // len 20
        v.extend_from_slice(&[
            0xf0, 0x9f, 0x98, 0x85, 0x0a, 0x77, 0x65, 0x6c, 0x6c, 0x20, 0x74, 0x68, 0x61, 0x74,
            0x27, 0x73, 0x01, 0x23, 0x45, 0x21,
        ]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x3b, 0x9a, 0xca, 0x00]); // balance
        v.extend_from_slice(&BLS_SIG); // proof of possession
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x07]); // message len
        v.extend_from_slice(b"message");
        v
    }
}
