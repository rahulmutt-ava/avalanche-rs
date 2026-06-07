// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.Operation` — the `OperationTx` body (specs 09 §3.4).
//!
//! TODO(M5.5): the concrete [`FxOperation`] variants (`secp256k1fx.MintOperation`
//! =8, `nftfx.MintOperation`=12 / `TransferOperation`=13, `propertyfx.MintOperation`
//! =17 / `BurnOperation`=18) and the `fx_id` routing via `TypeToFxIndex` land in
//! M5.5. None of those concrete fx-operation Rust types exist yet (the
//! secp256k1fx crate only reserves typeID 3/8 for `MintOperation`), so this module
//! defines the [`Operation`] envelope and a deferred [`FxOperation`] extension
//! point. BaseTx/Import/Export round-trips (the M5.2 scope) never touch it.

use std::cmp::Ordering;

use ava_codec::AvaCodec;
use ava_codec::Serializable;
use ava_codec::packer::Packer;
use ava_types::id::Id;

use crate::txs::components::{Asset, UtxoId};

/// `fxs.FxOperation` — the registered fx-operation interface (typeid-prefixed).
///
/// TODO(M5.5): replace [`FxOperation::Unsupported`] with the real
/// `secp256k1fx.MintOperation` (8) / `nftfx.*` (12,13) / `propertyfx.*` (17,18)
/// variants once those fx-operation Rust types exist. The placeholder keeps
/// [`Operation`] derivable and documents the extension point; it is **not** wired
/// into any codec registry (M5.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FxOperation {
    /// A not-yet-modeled fx operation. Carries the raw typeid-prefixed bytes so a
    /// parsed-but-unrouted operation can still round-trip losslessly.
    Unsupported(Vec<u8>),
}

impl Default for FxOperation {
    fn default() -> Self {
        FxOperation::Unsupported(Vec::new())
    }
}

impl ava_codec::Serializable for FxOperation {
    fn marshal_into(&self, p: &mut Packer) {
        // The bytes already include the fx-operation's typeID prefix; the M5.5
        // registry replaces this passthrough with typed dispatch.
        match self {
            FxOperation::Unsupported(raw) => {
                for b in raw {
                    p.pack_byte(*b);
                }
            }
        }
    }

    fn size(&self) -> usize {
        match self {
            FxOperation::Unsupported(raw) => raw.len(),
        }
    }
}

impl ava_codec::Deserializable for FxOperation {
    fn unmarshal_from(&mut self, _p: &mut Packer) {
        // TODO(M5.5): typed fx-operation decode via the codec registry. Until
        // then an `OperationTx` cannot be decoded (it is out of M5.2 scope), so
        // this is intentionally a no-op leaving the default empty payload.
    }
}

/// `avm/txs/operation.go` — one fx operation over a set of consumed UTXOs.
///
/// Wire layout (codec v0): `asset Asset | utxo_ids []UTXOID | op FxOperation`.
/// The `fx_id` is **derived** (`serialize:"false"`) from the op's concrete type.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Operation {
    /// The asset this operation acts on (`avax.Asset`).
    #[codec]
    pub asset: Asset,
    /// The UTXOs this operation consumes (sorted + unique).
    #[codec]
    pub utxo_ids: Vec<UtxoId>,
    /// The fx operation (interface; carries its own typeID).
    #[codec]
    pub op: FxOperation,
    /// `FxID` — runtime-only (`serialize:"false"`); never encoded.
    pub fx_id: Id,
}

impl Operation {
    /// The canonical codec bytes of this operation (the sort key used by
    /// `SortOperations` / `IsSortedAndUniqueOperations`).
    #[must_use]
    pub fn marshaled_bytes(&self) -> Vec<u8> {
        let mut p = Packer::with_max_size(usize::MAX);
        self.marshal_into(&mut p);
        p.into_bytes()
    }
}

/// `IsSortedAndUniqueOperations` — true iff `ops` are strictly increasing by
/// their marshaled bytes.
#[must_use]
pub fn is_sorted_and_unique_operations(ops: &[Operation]) -> bool {
    ops.windows(2).all(|w| match w {
        [a, b] => a.marshaled_bytes().cmp(&b.marshaled_bytes()) == Ordering::Less,
        _ => true,
    })
}

/// `SortOperations` — sort by marshaled bytes.
pub fn sort_operations(ops: &mut [Operation]) {
    ops.sort_by_key(|a| a.marshaled_bytes());
}
