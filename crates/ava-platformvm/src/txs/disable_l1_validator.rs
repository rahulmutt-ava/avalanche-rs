// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.DisableL1ValidatorTx` (type_id 39) — disable an L1 validator (specs 08
//! §2.2, §6).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::Auth;

/// `txs.DisableL1ValidatorTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct DisableL1ValidatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// ID corresponding to the validator.
    #[codec]
    pub validation_id: Id,
    /// Authorizes this validator to be disabled.
    #[codec]
    pub disable_auth: Auth,
}
