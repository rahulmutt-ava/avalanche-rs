// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (AVM) transaction model — the [`UnsignedTx`] interface enum, the
//! per-tx structs, the codec-serializable `avax`/fx components, and the signed
//! [`Tx`] envelope (specs 09 §3).
//!
//! Port of `vms/avm/txs`. Every X-Chain tx embeds an `avax.BaseTx` (`network_id`,
//! `blockchain_id`, `outs`, `ins`, `memo`) which serializes **inline** (TX-AVM-1).
//! The `type_id`s assigned to the five tx variants are the protocol constants from
//! specs 09 §2.1 (0–4); the fx component type-ids (secp256k1fx 5/6/7/9) live on
//! the [`components`] / [`credential`] interface enums.
//!
//! TODO(M5.5): the 21-entry standard/genesis `CodecRegistry` pair, the
//! `TypeToFxIndex` routing table, the nftfx/propertyfx output/operation/credential
//! variants, and the byte-exact golden tx-codec vectors land in M5.5. M5.2 wires
//! only the secp256k1fx variants needed for the BaseTx/Import/Export round-trip.

use ava_codec::AvaCodec;

pub mod base_tx;
pub mod codec;
pub mod components;
pub mod create_asset;
pub mod credential;
pub mod executor;
pub mod export;
pub mod import;
pub mod initial_state;
pub mod operation;
pub mod operation_tx;
pub mod tx;

pub use base_tx::BaseTx;
pub use create_asset::CreateAssetTx;
pub use credential::{Credential, FxCredential};
pub use export::ExportTx;
pub use import::ImportTx;
pub use initial_state::InitialState;
pub use operation::{FxOperation, Operation};
pub use operation_tx::OperationTx;
pub use tx::{CODEC_VERSION, Tx};

// ---------------------------------------------------------------------------
// UnsignedTx interface enum
// ---------------------------------------------------------------------------

/// `txs.UnsignedTx` — the Go interface registered into the codec; its concrete
/// types become enum variants (specs 09 §2.1 / §3.2).
///
/// `type_id`s are the protocol constants 0–4 (specs 09 §2.1); each variant
/// carries its explicit `#[codec(type_id = N)]`.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum UnsignedTx {
    /// `BaseTx` (type_id 0) — a pure UTXO transfer and the embedded base of every
    /// other tx.
    #[codec(type_id = 0)]
    Base(BaseTx),
    /// `CreateAssetTx` (type_id 1) — defines a new asset + its initial fx state.
    #[codec(type_id = 1)]
    CreateAsset(CreateAssetTx),
    /// `OperationTx` (type_id 2) — fx operations (mint/transfer/burn).
    #[codec(type_id = 2)]
    Operation(OperationTx),
    /// `ImportTx` (type_id 3) — import UTXOs from another chain via shared memory.
    #[codec(type_id = 3)]
    Import(ImportTx),
    /// `ExportTx` (type_id 4) — export UTXOs to another chain via shared memory.
    #[codec(type_id = 4)]
    Export(ExportTx),
}

impl Default for UnsignedTx {
    fn default() -> Self {
        UnsignedTx::Base(BaseTx::default())
    }
}

impl UnsignedTx {
    /// The embedded `avax.BaseTx` (every X-Chain tx has one; specs 09 §3.2).
    #[must_use]
    pub fn base(&self) -> &components::AvaxBaseTx {
        match self {
            UnsignedTx::Base(tx) => &tx.base,
            UnsignedTx::CreateAsset(tx) => &tx.base.base,
            UnsignedTx::Operation(tx) => &tx.base.base,
            UnsignedTx::Import(tx) => &tx.base.base,
            UnsignedTx::Export(tx) => &tx.base.base,
        }
    }

    /// The `avax.TransferableInput`s in the embedded `BaseTx` (`BaseTx.ins`).
    /// Tx-specific extra inputs (`ImportTx.imported_ins`) are surfaced via
    /// [`UnsignedTx::input_ids`].
    #[must_use]
    pub fn inputs(&self) -> &[components::TransferableInput] {
        &self.base().ins
    }

    /// The `avax.TransferableOutput`s in the embedded `BaseTx` (`BaseTx.outs`).
    #[must_use]
    pub fn outputs(&self) -> &[components::TransferableOutput] {
        &self.base().outs
    }

    /// `Tx.UTXOs()` (`vms/avm/txs/visitor.go:utxoGetter`) — the UTXOs this tx
    /// produces. Base `outs` occupy indices `0..len(outs)` (asset = the output's
    /// own asset id); a `CreateAssetTx`'s `states[*].outs` then continue the
    /// running index with asset id = `tx_id` (the asset is itself).
    #[must_use]
    pub fn utxos(&self, tx_id: ava_types::id::Id) -> Vec<executor::semantic::Utxo> {
        let mut utxos = Vec::new();
        let base = self.base();
        for (i, out) in base.outs.iter().enumerate() {
            // `i` is bounded by the decoded vec length; the codec caps it well
            // below u32::MAX, so the cast is safe.
            let output_index = u32::try_from(i).unwrap_or(u32::MAX);
            utxos.push(executor::semantic::Utxo {
                tx_id,
                output_index,
                asset_id: out.asset_id,
                out: out.out.clone(),
            });
        }
        if let UnsignedTx::CreateAsset(tx) = self {
            for state in &tx.states {
                for out in &state.outs {
                    let output_index = u32::try_from(utxos.len()).unwrap_or(u32::MAX);
                    utxos.push(executor::semantic::Utxo {
                        tx_id,
                        output_index,
                        asset_id: tx_id,
                        out: out.clone(),
                    });
                }
            }
        }
        utxos
    }

    /// The set of UTXO ids this tx consumes (`Tx.InputIDs`) — the `BaseTx.ins`
    /// plus the `ImportTx.imported_ins`.
    #[must_use]
    pub fn input_ids(&self) -> std::collections::BTreeSet<ava_types::id::Id> {
        let mut ids: std::collections::BTreeSet<ava_types::id::Id> = self
            .inputs()
            .iter()
            .map(components::TransferableInput::input_id)
            .collect();
        if let UnsignedTx::Import(tx) = self {
            ids.extend(
                tx.imported_ins
                    .iter()
                    .map(components::TransferableInput::input_id),
            );
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant's codec `type_id` matches the specs 09 §2.1 table (0–4).
    #[test]
    fn unsigned_tx_type_ids() {
        assert_eq!(UnsignedTx::Base(BaseTx::default()).codec_type_id(), 0);
        assert_eq!(
            UnsignedTx::CreateAsset(CreateAssetTx::default()).codec_type_id(),
            1
        );
        assert_eq!(
            UnsignedTx::Operation(OperationTx::default()).codec_type_id(),
            2
        );
        assert_eq!(UnsignedTx::Import(ImportTx::default()).codec_type_id(), 3);
        assert_eq!(UnsignedTx::Export(ExportTx::default()).codec_type_id(), 4);
    }
}
