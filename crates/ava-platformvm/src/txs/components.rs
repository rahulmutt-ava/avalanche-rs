// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Codec-serializable P-Chain UTXO components (`vms/components/avax` +
//! `secp256k1fx` interface fields), specs 08 §2.2.
//!
//! The runtime UTXO model (`ava_vm::components::avax`) keeps the fx output/input
//! payloads as `Arc<dyn TransferableOut>` trait objects, which cannot be embedded
//! as `#[codec]` fields. These types are the **byte-exact, codec-serializable**
//! shapes the P-Chain (and X-Chain) txs actually encode:
//!
//! * [`Owner`] / [`Output`] / [`Input`] — the registered fx interface enums.
//!   They marshal the `u32` typeID then the concrete payload, dispatching through
//!   the same shared registry as the tx codec (secp256k1fx 5/7/11, stakeable
//!   21/22). This is exactly how Go's `reflectcodec` encodes an interface field.
//! * [`TransferableOutput`] / [`TransferableInput`] / [`BaseTx`] — mirror
//!   `avax.TransferableOutput` / `avax.TransferableInput` / `avax.BaseTx`, with
//!   the fx payload replaced by the typed interface enum.
//!
//! Conversions to/from the `ava_vm::components::avax` runtime types are provided
//! where the field shapes line up (the FxID is runtime-only and omitted from the
//! wire, matching Go's `serialize:"false"`).

use std::cmp::Ordering;

use ava_codec::AvaCodec;
use ava_codec::Serializable;
use ava_codec::packer::Packer;
use ava_secp256k1fx::{Input as Secp256k1Input, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

use crate::stakeable::{LockIn, LockOut};

/// `fx.Owner` — the registered reward/subnet owner interface.
///
/// Only `secp256k1fx.OutputOwners` (type_id 11) is a concrete owner on the
/// P-Chain.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Owner {
    /// `secp256k1fx.OutputOwners` (type_id 11).
    #[codec(type_id = 11)]
    Secp256k1(OutputOwners),
}

impl Default for Owner {
    fn default() -> Self {
        Owner::Secp256k1(OutputOwners::default())
    }
}

impl Owner {
    /// `Owner.Verify` — delegates to the concrete owner.
    ///
    /// # Errors
    /// Propagates the underlying `OutputOwners::verify`.
    pub fn verify(&self) -> Result<(), crate::Error> {
        match self {
            Owner::Secp256k1(o) => verify_fx(o),
        }
    }
}

/// `verify.Verifiable` subnet-authorization interface (the `subnet_auth` /
/// `disable_auth` tx fields).
///
/// Concretely a `secp256k1fx.Input` (type_id 10) — the sig-index set proving
/// the issuer controls the subnet owner.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Auth {
    /// `secp256k1fx.Input` (type_id 10).
    #[codec(type_id = 10)]
    Secp256k1(Secp256k1Input),
}

impl Default for Auth {
    fn default() -> Self {
        Auth::Secp256k1(Secp256k1Input::default())
    }
}

impl Auth {
    /// `Verifiable.Verify` — delegate to the concrete authorization.
    ///
    /// # Errors
    /// Propagates the underlying `Input::verify`.
    pub fn verify(&self) -> Result<(), crate::Error> {
        match self {
            Auth::Secp256k1(i) => verify_fx(i),
        }
    }
}

/// `verify.State` (fx output) — the registered transferable-output interface.
///
/// Marshals the typeID then the payload: `secp256k1fx.TransferOutput` (7) or
/// `stakeable.LockOut` (22).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Output {
    /// `secp256k1fx.TransferOutput` (type_id 7).
    #[codec(type_id = 7)]
    Transfer(TransferOutput),
    /// `stakeable.LockOut` (type_id 22).
    #[codec(type_id = 22)]
    StakeableLock(LockOut),
}

impl Default for Output {
    fn default() -> Self {
        Output::Transfer(TransferOutput::default())
    }
}

impl Output {
    /// `TransferableOut.Amount` — the value this output represents.
    #[must_use]
    pub fn amount(&self) -> u64 {
        match self {
            Output::Transfer(o) => o.amt,
            Output::StakeableLock(o) => o.amount(),
        }
    }

    /// `verify.State.Verify` — delegate to the concrete output.
    ///
    /// # Errors
    /// Propagates the underlying output's `verify`.
    pub fn verify(&self) -> Result<(), crate::Error> {
        match self {
            Output::Transfer(o) => verify_fx(o),
            Output::StakeableLock(o) => o.verify(),
        }
    }
}

/// `verify.Verifiable` (fx input) — the registered transferable-input interface.
///
/// Marshals the typeID then the payload: `secp256k1fx.TransferInput` (5) or
/// `stakeable.LockIn` (21).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Input {
    /// `secp256k1fx.TransferInput` (type_id 5).
    #[codec(type_id = 5)]
    Transfer(TransferInput),
    /// `stakeable.LockIn` (type_id 21).
    #[codec(type_id = 21)]
    StakeableLock(LockIn),
}

impl Default for Input {
    fn default() -> Self {
        Input::Transfer(TransferInput::default())
    }
}

impl Input {
    /// `TransferableIn.Amount` — the value this input represents.
    #[must_use]
    pub fn amount(&self) -> u64 {
        match self {
            Input::Transfer(i) => i.amt,
            Input::StakeableLock(i) => i.amount(),
        }
    }

    /// `verify.Verifiable.Verify` — delegate to the concrete input.
    ///
    /// # Errors
    /// Propagates the underlying input's `verify`.
    pub fn verify(&self) -> Result<(), crate::Error> {
        match self {
            Input::Transfer(i) => verify_fx(i),
            Input::StakeableLock(i) => i.verify(),
        }
    }
}

/// Runs an `ava_vm::components::verify::Verifiable` and maps its error into the
/// P-Chain error model (the fx errors carry no payload we surface, so they
/// collapse to [`crate::Error::InvalidComponent`]).
fn verify_fx<V: ava_vm::components::verify::Verifiable>(v: &V) -> Result<(), crate::Error> {
    v.verify().map_err(|_| crate::Error::InvalidComponent)
}

/// `avax.TransferableOutput` — an asset id + fx output, sorted by
/// `(asset_id, codec(out) bytes)` (specs 07 §3.1).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferableOutput {
    /// The asset being transferred (`avax.Asset.ID`).
    #[codec]
    pub asset_id: Id,
    /// The fx output payload (interface; carries its own typeID).
    #[codec]
    pub out: Output,
}

impl TransferableOutput {
    /// `AssetID()`.
    #[must_use]
    pub fn asset_id(&self) -> Id {
        self.asset_id
    }

    /// `Output().Amount()`.
    #[must_use]
    pub fn amount(&self) -> u64 {
        self.out.amount()
    }

    /// The canonical codec bytes of the fx output payload (incl. its typeID),
    /// the secondary sort key in `SortTransferableOutputs`.
    #[must_use]
    pub fn out_bytes(&self) -> Vec<u8> {
        let mut p = Packer::with_max_size(usize::MAX);
        self.out.marshal_into(&mut p);
        p.into_bytes()
    }

    /// `Verify()` — verify the fx output.
    ///
    /// # Errors
    /// Propagates the output's `verify`.
    pub fn verify(&self) -> Result<(), crate::Error> {
        self.out.verify()
    }
}

/// `SortTransferableOutputs` — sort by `(asset_id, codec(out) bytes)`
/// (consensus-affecting; matches `innerSortTransferableOutputs.Less`).
pub fn sort_transferable_outputs(outs: &mut [TransferableOutput]) {
    outs.sort_by(cmp_transferable_outputs);
}

fn cmp_transferable_outputs(a: &TransferableOutput, b: &TransferableOutput) -> Ordering {
    a.asset_id
        .to_bytes()
        .cmp(&b.asset_id.to_bytes())
        .then_with(|| a.out_bytes().cmp(&b.out_bytes()))
}

/// `IsSortedTransferableOutputs` — true iff `outs` are in canonical order.
#[must_use]
pub fn is_sorted_transferable_outputs(outs: &[TransferableOutput]) -> bool {
    outs.windows(2).all(|w| match w {
        [a, b] => cmp_transferable_outputs(a, b) != Ordering::Greater,
        _ => true,
    })
}

/// `avax.TransferableInput` — a UTXOID + asset + fx input (specs 07 §3.1).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferableInput {
    /// The id of the tx that produced the referenced UTXO (`UTXOID.TxID`).
    #[codec]
    pub tx_id: Id,
    /// The index of the referenced output within that tx (`UTXOID.OutputIndex`).
    #[codec]
    pub output_index: u32,
    /// The asset being spent (`avax.Asset.ID`).
    #[codec]
    pub asset_id: Id,
    /// The fx input payload (interface; carries its own typeID).
    #[codec]
    pub r#in: Input,
}

impl TransferableInput {
    /// `AssetID()`.
    #[must_use]
    pub fn asset_id(&self) -> Id {
        self.asset_id
    }

    /// `Input().Amount()`.
    #[must_use]
    pub fn amount(&self) -> u64 {
        self.r#in.amount()
    }

    /// `InputID()` — the unique id of the UTXO this input spends,
    /// `tx_id.prefix(output_index)`.
    #[must_use]
    pub fn input_id(&self) -> Id {
        self.tx_id.prefix(&[u64::from(self.output_index)])
    }

    /// `Compare(other)` — order by UTXOID (`tx_id` bytes, then `output_index`).
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        self.tx_id
            .to_bytes()
            .cmp(&other.tx_id.to_bytes())
            .then_with(|| self.output_index.cmp(&other.output_index))
    }

    /// `Verify()` — verify the fx input.
    ///
    /// # Errors
    /// Propagates the input's `verify`.
    pub fn verify(&self) -> Result<(), crate::Error> {
        self.r#in.verify()
    }
}

/// `IsSortedAndUniqueTransferableInputs` — true iff `ins` are strictly
/// increasing by UTXOID.
#[must_use]
pub fn is_sorted_and_unique_transferable_inputs(ins: &[TransferableInput]) -> bool {
    ins.windows(2).all(|w| match w {
        [a, b] => a.compare(b) == Ordering::Less,
        _ => true,
    })
}

/// `SortTransferableInputs` — sort by UTXOID.
pub fn sort_transferable_inputs(ins: &mut [TransferableInput]) {
    ins.sort_by(TransferableInput::compare);
}

/// `avax.BaseTx` — the common preamble embedded by every P-Chain tx (specs 08
/// §2.2).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseTx {
    /// The network this chain lives on.
    #[codec]
    pub network_id: u32,
    /// The chain this tx exists on — prevents cross-chain replay.
    #[codec]
    pub blockchain_id: Id,
    /// The tx outputs.
    #[codec]
    pub outs: Vec<TransferableOutput>,
    /// The tx inputs.
    #[codec]
    pub ins: Vec<TransferableInput>,
    /// Arbitrary memo bytes (up to `avax.MaxMemoSize` = 256).
    #[codec]
    pub memo: Vec<u8>,
}

/// The maximum number of bytes in a tx memo field (`avax.MaxMemoSize`).
pub const MAX_MEMO_SIZE: usize = 256;

impl BaseTx {
    /// `InputIDs()` — the set of UTXO ids this tx consumes.
    #[must_use]
    pub fn input_ids(&self) -> std::collections::BTreeSet<Id> {
        self.ins.iter().map(TransferableInput::input_id).collect()
    }
}

/// `message.PChainOwner` (specs 20 §3.1) — a threshold + sorted addresses owner
/// embedded in `ConvertSubnetToL1Validator` (ACP-77).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct PChainOwner {
    /// Threshold number of `addresses` that must sign.
    #[codec]
    pub threshold: u32,
    /// The addresses allowed to sign to authenticate this owner.
    #[codec]
    pub addresses: Vec<ShortId>,
}
