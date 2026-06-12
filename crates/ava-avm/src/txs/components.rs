// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Codec-serializable X-Chain UTXO components (`vms/components/avax` +
//! `secp256k1fx` interface fields), specs 09 §3.
//!
//! Mirrors the P-Chain precedent (`ava_platformvm::txs::components`): the runtime
//! UTXO model (`ava_vm::components::avax`) keeps the fx output/input payloads as
//! `Arc<dyn …>` trait objects, which cannot be `#[codec]` fields. These types are
//! the **byte-exact, codec-serializable** shapes the X-Chain txs actually encode:
//!
//! * [`Output`] / [`Input`] — the registered fx interface enums. They marshal the
//!   `u32` typeID then the concrete payload, dispatching through the shared codec
//!   registry (secp256k1fx `TransferInput`=5, `MintOutput`=6, `TransferOutput`=7).
//!   This is exactly how Go's `reflectcodec` encodes an interface field.
//! * [`TransferableOutput`] / [`TransferableInput`] / [`AvaxBaseTx`] — mirror
//!   `avax.TransferableOutput` / `avax.TransferableInput` / `avax.BaseTx`, with
//!   the fx payload replaced by the typed interface enum.
//! * [`Asset`] / [`UtxoId`] — the codec-serializable `avax.Asset` / `avax.UTXOID`.
//!
//! TODO(M5.5): the nftfx / propertyfx output/input/operation variants and the
//! 21-entry `CodecRegistry` + `TypeToFxIndex` routing table land in M5.5. The
//! enums below define only the secp256k1fx variants needed for the
//! BaseTx/Import/Export round-trip; they are the documented extension point.

use std::cmp::Ordering;

use ava_codec::AvaCodec;
use ava_codec::Serializable;
use ava_codec::packer::Packer;
use ava_secp256k1fx::{MintOutput, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

/// The maximum number of bytes in a tx memo field (`avax.MaxMemoSize`).
pub const MAX_MEMO_SIZE: usize = 256;

/// `verify.State` (fx output) — the registered transferable-output interface.
///
/// Marshals the typeID then the payload: `secp256k1fx.TransferOutput` (7) or
/// `secp256k1fx.MintOutput` (6).
///
/// TODO(M5.5): add `nftfx`/`propertyfx` output variants (type_ids 10/11/15/16).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Output {
    /// `secp256k1fx.MintOutput` (type_id 6).
    #[codec(type_id = 6)]
    SecpMint(MintOutput),
    /// `secp256k1fx.TransferOutput` (type_id 7).
    #[codec(type_id = 7)]
    SecpTransfer(TransferOutput),
}

impl Default for Output {
    fn default() -> Self {
        Output::SecpTransfer(TransferOutput::default())
    }
}

impl Output {
    /// The value this output represents (`0` for a mint output).
    #[must_use]
    pub fn amount(&self) -> u64 {
        match self {
            Output::SecpMint(_) => 0,
            Output::SecpTransfer(o) => o.amt,
        }
    }

    /// `verify.State.Verify` — delegate to the concrete output.
    ///
    /// # Errors
    /// Propagates the underlying output's `verify`.
    pub fn verify(&self) -> Result<(), crate::Error> {
        match self {
            Output::SecpMint(o) => verify_fx(o),
            Output::SecpTransfer(o) => verify_fx(o),
        }
    }

    /// `avax.Addressable.Addresses()` — the owning addresses of this output
    /// (the `OutputOwners.Addrs` of the concrete fx output; Go
    /// `vms/components/avax/utxo_state.go` uses this to maintain the
    /// address → UTXO index).
    #[must_use]
    pub fn addresses(&self) -> &[ShortId] {
        match self {
            Output::SecpMint(o) => &o.owners.addrs,
            Output::SecpTransfer(o) => &o.owners.addrs,
        }
    }
}

/// `verify.Verifiable` (fx input) — the registered transferable-input interface.
///
/// Marshals the typeID then the payload: `secp256k1fx.TransferInput` (5).
///
/// TODO(M5.5): no nft/property *inputs* exist (those fxs use operations), so this
/// enum stays secp-only; kept as an enum for parity with the P-Chain shape.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Input {
    /// `secp256k1fx.TransferInput` (type_id 5).
    #[codec(type_id = 5)]
    SecpTransfer(TransferInput),
}

impl Default for Input {
    fn default() -> Self {
        Input::SecpTransfer(TransferInput::default())
    }
}

impl Input {
    /// The value this input represents.
    #[must_use]
    pub fn amount(&self) -> u64 {
        match self {
            Input::SecpTransfer(i) => i.amt,
        }
    }

    /// `verify.Verifiable.Verify` — delegate to the concrete input.
    ///
    /// # Errors
    /// Propagates the underlying input's `verify`.
    pub fn verify(&self) -> Result<(), crate::Error> {
        match self {
            Input::SecpTransfer(i) => verify_fx(i),
        }
    }
}

/// Runs an `ava_vm::components::verify::Verifiable` and maps its error into the
/// X-Chain error model.
fn verify_fx<V: ava_vm::components::verify::Verifiable>(v: &V) -> Result<(), crate::Error> {
    v.verify().map_err(crate::Error::from)
}

/// `avax.Asset` — the asset an output/input/UTXO is denominated in.
#[derive(AvaCodec, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Asset {
    /// The asset id (`avax.Asset.ID`).
    #[codec]
    pub id: Id,
}

impl Asset {
    /// Builds an [`Asset`] from its id.
    #[must_use]
    pub fn new(id: Id) -> Self {
        Self { id }
    }
}

/// `avax.UTXOID` — references the UTXO an input/operation spends (the
/// `serialize:"true"` `(tx_id, output_index)` pair; the runtime `Symbol`/cached
/// id are not on the wire).
#[derive(AvaCodec, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UtxoId {
    /// The id of the tx that produced the referenced UTXO (`UTXOID.TxID`).
    #[codec]
    pub tx_id: Id,
    /// The index of the referenced output within that tx (`UTXOID.OutputIndex`).
    #[codec]
    pub output_index: u32,
}

impl UtxoId {
    /// Builds a [`UtxoId`] from a tx id and output index.
    #[must_use]
    pub fn new(tx_id: Id, output_index: u32) -> Self {
        Self {
            tx_id,
            output_index,
        }
    }

    /// `InputID()` — the unique id of the UTXO this references,
    /// `tx_id.prefix(output_index)`.
    #[must_use]
    pub fn input_id(&self) -> Id {
        self.tx_id.prefix(&[u64::from(self.output_index)])
    }

    /// `Compare(other)` — order by `tx_id` bytes, then `output_index`.
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        self.tx_id
            .to_bytes()
            .cmp(&other.tx_id.to_bytes())
            .then_with(|| self.output_index.cmp(&other.output_index))
    }
}

/// `IsSortedAndUniqueUTXOIDs` — true iff `ids` are strictly increasing.
#[must_use]
pub fn is_sorted_and_unique_utxo_ids(ids: &[UtxoId]) -> bool {
    ids.windows(2).all(|w| match w {
        [a, b] => a.compare(b) == Ordering::Less,
        _ => true,
    })
}

/// `SortUTXOIDs` — sort by `(tx_id, output_index)`.
pub fn sort_utxo_ids(ids: &mut [UtxoId]) {
    ids.sort_by(UtxoId::compare);
}

/// `avax.TransferableOutput` — an asset id + fx output, sorted by
/// `(asset_id, codec(out) bytes)` (specs 09 §3).
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

/// `avax.TransferableInput` — a UTXOID + asset + fx input (specs 09 §3).
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

/// `avax.BaseTx` — the common preamble embedded inline by every X-Chain tx
/// (specs 09 §3.2). Byte layout (codec v0):
///   `network_id u32 | blockchain_id [32] | outs []TransferableOutput
///   | ins []TransferableInput | memo []u8 (<=256)`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AvaxBaseTx {
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

impl AvaxBaseTx {
    /// `InputIDs()` — the set of UTXO ids this tx consumes.
    #[must_use]
    pub fn input_ids(&self) -> std::collections::BTreeSet<Id> {
        self.ins.iter().map(TransferableInput::input_id).collect()
    }
}
