// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.OperationTx` (type_id 2) — applies fx operations over the UTXO set
//! (specs 09 §3.2/§3.4).

use ava_codec::AvaCodec;

use crate::txs::base_tx::BaseTx;
use crate::txs::operation::Operation;

/// `txs.OperationTx` — the embedded `BaseTx` (inline) followed by its operations.
///
/// Field order = serialization order (TX-AVM-1): the embedded `BaseTx` first
/// (inline), then `ops`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationTx {
    /// The embedded base tx (serialized inline).
    #[codec]
    pub base: BaseTx,
    /// The fx operations (non-empty, sorted + unique by marshaled bytes).
    #[codec]
    pub ops: Vec<Operation>,
}
