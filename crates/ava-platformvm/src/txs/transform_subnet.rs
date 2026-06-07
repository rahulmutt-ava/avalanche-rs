// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.TransformSubnetTx` (type_id 24) — the elastic-subnet transform tx (a
//! no-op post-Etna; specs 08 §2.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::txs::base_tx::BaseTx;
use crate::txs::components::Auth;

/// `txs.TransformSubnetTx` — full elastic-subnet parameters.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct TransformSubnetTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// ID of the subnet to transform (not the Primary Network).
    #[codec]
    pub subnet: Id,
    /// Asset to use when staking on the subnet.
    #[codec]
    pub asset_id: Id,
    /// Amount to initially specify as the current supply.
    #[codec]
    pub initial_supply: u64,
    /// Maximum token supply.
    #[codec]
    pub maximum_supply: u64,
    /// Consumption rate at zero stake duration.
    #[codec]
    pub min_consumption_rate: u64,
    /// Consumption rate at the full minting period.
    #[codec]
    pub max_consumption_rate: u64,
    /// Minimum validator stake.
    #[codec]
    pub min_validator_stake: u64,
    /// Maximum validator stake (incl. delegations).
    #[codec]
    pub max_validator_stake: u64,
    /// Minimum stake duration, in seconds.
    #[codec]
    pub min_stake_duration: u32,
    /// Maximum stake duration, in seconds.
    #[codec]
    pub max_stake_duration: u32,
    /// Minimum delegation fee, in millionths.
    #[codec]
    pub min_delegation_fee: u32,
    /// Minimum delegator stake.
    #[codec]
    pub min_delegator_stake: u64,
    /// Maximum-validator-weight factor.
    #[codec]
    pub max_validator_weight_factor: u8,
    /// Uptime requirement, in millionths.
    #[codec]
    pub uptime_requirement: u32,
    /// Authorizes this transformation.
    #[codec]
    pub subnet_auth: Auth,
}
