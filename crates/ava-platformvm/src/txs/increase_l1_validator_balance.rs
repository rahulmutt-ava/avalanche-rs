// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.IncreaseL1ValidatorBalanceTx` (type_id 38) — top up an L1 validator's
//! continuous-fee balance (specs 08 §2.2, §6).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;

/// `txs.IncreaseL1ValidatorBalanceTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct IncreaseL1ValidatorBalanceTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// ID corresponding to the validator.
    #[codec]
    pub validation_id: Id,
    /// Balance to add.
    #[codec]
    pub balance: u64,
}
