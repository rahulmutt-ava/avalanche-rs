// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.ExportTx` (type_id 4) — exports UTXOs to another chain via shared memory
//! (specs 09 §3.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::TransferableOutput;

/// `txs.ExportTx` — the embedded `BaseTx` (inline), the destination chain id, and
/// the exported outputs.
///
/// Field order = serialization order (TX-AVM-1): the embedded `BaseTx` first
/// (inline), then `destination_chain`, then `exported_outs`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportTx {
    /// The embedded base tx (serialized inline).
    #[codec]
    pub base: BaseTx,
    /// The chain the exported UTXOs are destined for (`DestinationChain`).
    #[codec]
    pub destination_chain: Id,
    /// The outputs sent to the destination chain (non-empty, sorted).
    #[codec]
    pub exported_outs: Vec<TransferableOutput>,
}
