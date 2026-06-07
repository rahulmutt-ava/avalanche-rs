// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.SetL1ValidatorWeightTx` (type_id 37) — set an L1 validator's weight from
//! a signed Warp `L1ValidatorWeight` message (specs 08 §2.2, §6).

use ava_codec::AvaCodec;

use crate::txs::base_tx::BaseTx;

/// `txs.SetL1ValidatorWeightTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct SetL1ValidatorWeightTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// The signed Warp `L1ValidatorWeight` message (raw bytes).
    #[codec]
    pub message: Vec<u8>,
}
