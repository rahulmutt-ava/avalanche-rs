// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.ConvertSubnetToL1Tx` (type_id 35) — ACP-77 subnet→L1 conversion (specs
//! 08 §2.2, §6).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::signer::ProofOfPossession;
use crate::txs::base_tx::BaseTx;
use crate::txs::components::{Auth, PChainOwner};

/// `txs.ConvertSubnetToL1Validator` — an initial pay-as-you-go L1 validator.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConvertSubnetToL1Validator {
    /// NodeID of this validator (raw bytes; length-prefixed).
    #[codec]
    pub node_id: Vec<u8>,
    /// Weight of this validator used when sampling.
    #[codec]
    pub weight: u64,
    /// Initial balance for this validator.
    #[codec]
    pub balance: u64,
    /// The BLS key for this validator (with proof of possession).
    #[codec]
    pub signer: ProofOfPossession,
    /// Owner of leftover $AVAX once removed from the validator set.
    #[codec]
    pub remaining_balance_owner: PChainOwner,
    /// Owner with authority to manually deactivate this validator.
    #[codec]
    pub deactivation_owner: PChainOwner,
}

/// `txs.ConvertSubnetToL1Tx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConvertSubnetToL1Tx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// ID of the subnet to convert.
    #[codec]
    pub subnet: Id,
    /// Chain where the subnet manager lives.
    #[codec]
    pub chain_id: Id,
    /// Address of the subnet manager (raw bytes; length-prefixed).
    #[codec]
    pub address: Vec<u8>,
    /// Initial pay-as-you-go validators for the subnet.
    #[codec]
    pub validators: Vec<ConvertSubnetToL1Validator>,
    /// Authorizes this conversion.
    #[codec]
    pub subnet_auth: Auth,
}
