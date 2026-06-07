// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.CreateChainTx` (type_id 15) — create a blockchain on a subnet (specs 08
//! §2.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::Auth;

/// `txs.CreateChainTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateChainTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// ID of the subnet that validates this blockchain.
    #[codec]
    pub subnet_id: Id,
    /// A human-readable name for the chain (need not be unique).
    #[codec]
    pub chain_name: String,
    /// ID of the VM running on the new chain.
    #[codec]
    pub vm_id: Id,
    /// IDs of the feature extensions running on the new chain.
    #[codec]
    pub fx_ids: Vec<Id>,
    /// Byte representation of the genesis state of the new chain.
    #[codec]
    pub genesis_data: Vec<u8>,
    /// Authorizes this blockchain to be added to the subnet.
    #[codec]
    pub subnet_auth: Auth,
}
