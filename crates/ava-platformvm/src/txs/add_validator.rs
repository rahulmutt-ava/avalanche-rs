// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.AddValidatorTx` (type_id 12) — the deprecated, parse-only Apricot
//! primary-network validator tx (specs 08 §2.2).

use ava_codec::AvaCodec;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::{Owner, TransferableOutput};
use crate::txs::validator::Validator;

/// `txs.AddValidatorTx` (deprecated; retained for parse compatibility).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddValidatorTx {
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
    pub rewards_owner: Owner,
    /// Fee this validator charges delegators, in millionths.
    #[codec]
    pub delegation_shares: u32,
}
