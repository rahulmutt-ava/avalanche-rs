// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.AddSubnetValidatorTx` (type_id 13) — the deprecated permissioned-subnet
//! validator tx (specs 08 §2.2).

use ava_codec::AvaCodec;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::Auth;
use crate::txs::validator::SubnetValidator;

/// `txs.AddSubnetValidatorTx` (deprecated).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddSubnetValidatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// The subnet validator.
    #[codec]
    pub subnet_validator: SubnetValidator,
    /// Auth that allows this validator into the subnet.
    #[codec]
    pub subnet_auth: Auth,
}
