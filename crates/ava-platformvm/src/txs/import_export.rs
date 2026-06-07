// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.ImportTx` (type_id 17) / `txs.ExportTx` (type_id 18) — cross-chain
//! atomic import/export (specs 08 §2.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::{TransferableInput, TransferableOutput};

/// `txs.ImportTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Which chain to consume the funds from.
    #[codec]
    pub source_chain: Id,
    /// Inputs that consume UTXOs produced on the source chain.
    #[codec]
    pub imported_inputs: Vec<TransferableInput>,
}

/// `txs.ExportTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Which chain to send the funds to.
    #[codec]
    pub destination_chain: Id,
    /// Outputs that are exported to the destination chain.
    #[codec]
    pub exported_outputs: Vec<TransferableOutput>,
}
