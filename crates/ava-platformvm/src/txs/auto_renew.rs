// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Helicon auto-renew lifecycle txs (specs 08 §2.2):
//! `AddAutoRenewedValidatorTx` (40), `SetAutoRenewedValidatorConfigTx` (41),
//! `RewardAutoRenewedValidatorTx` (42).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::signer::Signer;
use crate::txs::base_tx::BaseTx;
use crate::txs::components::{Auth, Owner, TransferableOutput};

/// `txs.AddAutoRenewedValidatorTx` (type_id 40).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddAutoRenewedValidatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Node ID of the validator (raw bytes; length-prefixed).
    #[codec]
    pub validator_node_id: Vec<u8>,
    /// The BLS signer for this validator.
    #[codec]
    pub signer: Signer,
    /// Where to send staked tokens when done validating.
    #[codec]
    pub stake_outs: Vec<TransferableOutput>,
    /// Where to send validation rewards.
    #[codec]
    pub validator_rewards_owner: Owner,
    /// Where to send delegation rewards.
    #[codec]
    pub delegator_rewards_owner: Owner,
    /// Who is authorized to manage this validator.
    #[codec]
    pub validator_authority: Owner,
    /// Fee this validator charges delegators, in millionths.
    #[codec]
    pub delegation_shares: u32,
    /// Percentage of rewards to restake at each cycle end, in millionths.
    #[codec]
    pub auto_compound_reward_shares: u32,
    /// The validation cycle duration, in seconds.
    #[codec]
    pub period: u64,
}

/// `txs.SetAutoRenewedValidatorConfigTx` (type_id 41).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct SetAutoRenewedValidatorConfigTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// ID of the tx that created the auto-renewed validator.
    #[codec]
    pub tx_id: Id,
    /// Authorizes this validator to be updated.
    #[codec]
    pub auth: Auth,
    /// Percentage of rewards to restake at each cycle end, in millionths.
    #[codec]
    pub auto_compound_reward_shares: u32,
    /// Period for the next cycle (in seconds); 0 stops at the current cycle end.
    #[codec]
    pub period: u64,
}

/// `txs.RewardAutoRenewedValidatorTx` (type_id 42). No embedded `BaseTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct RewardAutoRenewedValidatorTx {
    /// ID of the tx that created the validator being rewarded.
    #[codec]
    pub tx_id: Id,
    /// End time of the validation cycle.
    #[codec]
    pub timestamp: u64,
}
