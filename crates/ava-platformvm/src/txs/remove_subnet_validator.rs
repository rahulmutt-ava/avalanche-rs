// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.RemoveSubnetValidatorTx` (type_id 23) — remove a node from a
//! permissioned subnet (specs 08 §2.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::Auth;

/// `txs.RemoveSubnetValidatorTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoveSubnetValidatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// The node to remove from the subnet.
    #[codec]
    pub node_id: NodeId,
    /// The subnet to remove the node from.
    #[codec]
    pub subnet: Id,
    /// Proves the issuer can remove the node from the subnet.
    #[codec]
    pub subnet_auth: Auth,
}
