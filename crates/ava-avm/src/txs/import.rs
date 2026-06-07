// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.ImportTx` (type_id 3) — imports UTXOs from another chain via shared
//! memory (specs 09 §3.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::TransferableInput;

/// `txs.ImportTx` — the embedded `BaseTx` (inline), the source chain id, and the
/// imported inputs.
///
/// Field order = serialization order (TX-AVM-1): the embedded `BaseTx` first
/// (inline), then `source_chain`, then `imported_ins`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportTx {
    /// The embedded base tx (serialized inline).
    #[codec]
    pub base: BaseTx,
    /// The chain the imported UTXOs originate from (`SourceChain`).
    #[codec]
    pub source_chain: Id,
    /// The inputs spending imported UTXOs (non-empty; from shared memory).
    #[codec]
    pub imported_ins: Vec<TransferableInput>,
}
