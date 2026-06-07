// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.AddDelegatorTx` (type_id 14) — the deprecated Apricot primary-network
//! delegator tx (specs 08 §2.2).

use ava_codec::AvaCodec;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::{Owner, TransferableOutput};
use crate::txs::validator::Validator;

/// `txs.AddDelegatorTx` (deprecated).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddDelegatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Describes the delegatee.
    #[codec]
    pub validator: Validator,
    /// Where to send staked tokens when done validating.
    #[codec]
    pub stake_outs: Vec<TransferableOutput>,
    /// Where to send staking rewards when done validating.
    #[codec]
    pub delegation_rewards_owner: Owner,
}
