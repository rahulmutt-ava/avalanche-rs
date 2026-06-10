// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Codec-serializable `avax`/`secp256k1fx` UTXO components for the C-Chain
//! atomic Import/Export txs (specs 27 §3.1).
//!
//! Mirrors `ava_avm::txs::components`: the runtime UTXO model
//! (`ava_vm::components::avax`) keeps fx output/input payloads as `Arc<dyn …>`
//! trait objects, which cannot be `#[codec]` fields. These types are the
//! **byte-exact, codec-serializable** shapes the C-Chain atomic txs actually
//! encode. The C-Chain shares the X-Chain/P-Chain UTXO serialization so that
//! UTXOs in shared memory have the same format on every chain (Go
//! `cchain/tx/codec.go` skips registrations to align typeIDs).
//!
//! The skip-aligned registration order is reproduced exactly via the
//! `#[codec(type_id = N)]` annotations:
//!
//! * `Import` = 0, `Export` = 1 (the two unsigned tx types),
//! * `secp256k1fx.TransferInput` = 5 (after 3 skips),
//! * `secp256k1fx.TransferOutput` = 7 (after 1 more skip),
//! * `secp256k1fx.Credential` = 9 (after 1 more skip).

use std::cmp::Ordering;

use ava_codec::AvaCodec;
use ava_codec::Serializable;
use ava_codec::packer::Packer;
use ava_types::id::Id;

// Re-export the fx leaf types so callers build components without a second dep.
pub use ava_secp256k1fx::{TransferInput, TransferOutput};

/// `verify.State` (fx output) — the registered transferable-output interface.
///
/// Marshals the typeID then the payload: `secp256k1fx.TransferOutput` (7). Only
/// the secp transfer-output variant is used by the C-Chain atomic export.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Output {
    /// `secp256k1fx.TransferOutput` (`type_id` 7).
    #[codec(type_id = 7)]
    SecpTransfer(TransferOutput),
}

impl Default for Output {
    fn default() -> Self {
        Output::SecpTransfer(TransferOutput::default())
    }
}

impl Output {
    /// The value this output represents.
    #[must_use]
    pub fn amount(&self) -> u64 {
        match self {
            Output::SecpTransfer(o) => o.amt,
        }
    }
}

/// `verify.Verifiable` (fx input) — the registered transferable-input interface.
///
/// Marshals the typeID then the payload: `secp256k1fx.TransferInput` (5).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Input {
    /// `secp256k1fx.TransferInput` (`type_id` 5).
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

/// `avax.TransferableInput` — a UTXOID + asset + fx input (specs 07 §3.1).
///
/// Wire layout: `tx_id [32] | output_index u32 | asset_id [32] | in
/// (typeid-prefixed fx input)`.
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

/// `avax.TransferableOutput` — an asset id + fx output (specs 07 §3.1).
///
/// Wire layout: `asset_id [32] | out (typeid-prefixed fx output)`.
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
}

/// `avax.UTXO` — an on-chain unspent output, codec-serializable.
///
/// Wire layout: `tx_id [32] | output_index u32 | asset_id [32] | out
/// (typeid-prefixed fx output)` — the format `cchain/tx.MarshalUTXO` produces
/// and the peer chain parses out of shared memory.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Utxo {
    /// The id of the tx that produced this UTXO (`UTXOID.TxID`).
    #[codec]
    pub tx_id: Id,
    /// The index of this output within the producing tx (`UTXOID.OutputIndex`).
    #[codec]
    pub output_index: u32,
    /// The asset this UTXO holds (`avax.Asset.ID`).
    #[codec]
    pub asset_id: Id,
    /// The fx output state (interface; carries its own typeID).
    #[codec]
    pub out: Output,
}

impl Utxo {
    /// `InputID()` — the unique id of this UTXO, `tx_id.prefix(output_index)`.
    #[must_use]
    pub fn input_id(&self) -> Id {
        self.tx_id.prefix(&[u64::from(self.output_index)])
    }
}

/// `SortTransferableOutputs` — sort by `(asset_id, codec(out) bytes)`.
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
