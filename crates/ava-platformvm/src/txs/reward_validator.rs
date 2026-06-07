// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.RewardValidatorTx` (type_id 20) — the proposal tx that rewards (or not)
//! a staker leaving the validator set (specs 08 §2.2). No embedded `BaseTx`.

use ava_codec::AvaCodec;
use ava_types::id::Id;

/// `txs.RewardValidatorTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct RewardValidatorTx {
    /// ID of the tx that created the staker being removed/rewarded.
    #[codec]
    pub tx_id: Id,
}
