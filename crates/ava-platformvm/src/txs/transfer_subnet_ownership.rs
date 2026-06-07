// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.TransferSubnetOwnershipTx` (type_id 33) — transfer subnet ownership
//! (specs 08 §2.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::{Auth, Owner};

/// `txs.TransferSubnetOwnershipTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferSubnetOwnershipTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// ID of the subnet this tx modifies.
    #[codec]
    pub subnet: Id,
    /// Proves the issuer controls the current subnet owner.
    #[codec]
    pub subnet_auth: Auth,
    /// Who is now authorized to manage this subnet.
    #[codec]
    pub owner: Owner,
}
