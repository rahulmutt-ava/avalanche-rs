// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.AddPermissionlessDelegatorTx` (type_id 26) — add a permissionless
//! delegator (specs 08 §2.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::{Owner, TransferableOutput};
use crate::txs::validator::Validator;

/// `txs.AddPermissionlessDelegatorTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddPermissionlessDelegatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Describes the validator being delegated to.
    #[codec]
    pub validator: Validator,
    /// ID of the subnet this delegator is delegating on.
    #[codec]
    pub subnet: Id,
    /// Where to send staked tokens when done delegating.
    #[codec]
    pub stake_outs: Vec<TransferableOutput>,
    /// Where to send delegation rewards when done delegating.
    #[codec]
    pub delegation_rewards_owner: Owner,
}
