// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.CreateAssetTx` (type_id 1) — defines a new asset + its initial fx state
//! (specs 09 §3.2/§3.3).

use ava_codec::AvaCodec;

use crate::txs::base_tx::BaseTx;
use crate::txs::initial_state::InitialState;

/// `txs.CreateAssetTx` — the embedded `BaseTx` (inline) followed by the asset's
/// name/symbol/denomination and per-fx initial states.
///
/// Field order = serialization order (TX-AVM-1): the embedded `BaseTx` first
/// (inline, no prefix), then `name`, `symbol`, `denomination`, `states`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateAssetTx {
    /// The embedded base tx (serialized inline).
    #[codec]
    pub base: BaseTx,
    /// The asset's human-readable name (1..=128 chars; ASCII letters/digits/space,
    /// no leading/trailing whitespace).
    #[codec]
    pub name: String,
    /// The asset's short symbol (1..=4 chars; ASCII uppercase).
    #[codec]
    pub symbol: String,
    /// The number of `10^-denomination` units in one whole asset (<= 32).
    #[codec]
    pub denomination: u8,
    /// The per-fx initial UTXO states (sorted + unique by `fx_index`, non-empty).
    #[codec]
    pub states: Vec<InitialState>,
}
