// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.BaseTx` (type_id 0) — a plain UTXO transfer and the common preamble every
//! other X-Chain tx embeds inline (specs 09 §3.2).

use ava_codec::AvaCodec;

use crate::txs::components::{AvaxBaseTx, TransferableInput, TransferableOutput};

/// `txs.BaseTx` — wraps the embedded `avax.BaseTx` (network/chain id, ins/outs,
/// memo). Serializes the embedded body **inline** (no extra prefix).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseTx {
    /// The embedded `avax.BaseTx` (network/chain id, ins/outs, memo).
    #[codec]
    pub base: AvaxBaseTx,
}

impl BaseTx {
    /// Builds a [`BaseTx`] over an `avax.BaseTx`.
    #[must_use]
    pub fn new(base: AvaxBaseTx) -> Self {
        Self { base }
    }

    /// The tx outputs.
    #[must_use]
    pub fn outputs(&self) -> &[TransferableOutput] {
        &self.base.outs
    }

    /// The tx inputs.
    #[must_use]
    pub fn inputs(&self) -> &[TransferableInput] {
        &self.base.ins
    }
}
