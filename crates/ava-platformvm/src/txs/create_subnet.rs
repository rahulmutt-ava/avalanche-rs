// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.CreateSubnetTx` (type_id 16) — create a new permissioned subnet (specs
//! 08 §2.2).

use ava_codec::AvaCodec;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::Owner;

/// `txs.CreateSubnetTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateSubnetTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Who is authorized to manage this subnet.
    #[codec]
    pub owner: Owner,
}
