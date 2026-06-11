// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The RewardManager stateful precompile — port of subnet-evm
//! `precompile/contracts/rewardmanager` (M6.31, spec 10 §8). Stores the fee
//! reward destination (a reward address, the allow-fee-recipients sentinel, or
//! the blackhole = rewards disabled) under one slot, gated by the embedded
//! allow list at its own address.
//!
//! ## Storage layout (`contract.go`)
//!
//! One slot: `rewardAddressStorageKey = common.Hash{'r','a','s','k'}`
//! (left-aligned). Value: the reward address right-aligned
//! (`BytesToHash(addr)`), OR the sentinel
//! `allowFeeRecipientsAddressValue = common.Hash{'a','f','r','a','v'}`, OR the
//! blackhole address (`0x0100..00`, rewards disabled/burned).

use std::sync::Arc;

use ava_evm_reth::{Address, B256, Gas, InterpreterResult, PrecompileError};

use crate::precompile::abi::{failure, out_of_gas, read_addr, success, word_addr, word_bool};
use crate::precompile::allowlist::{dispatch_allowlist, get_allow_list_status, split_selector};
use crate::precompile::registry::{
    PrecompileCtx, PrecompileModule, PrecompileStateOps, StatefulPrecompile,
};

/// `rewardmanager.ContractAddress` (`0x02..04`).
pub const REWARD_MANAGER_ADDRESS: Address = Address::new([
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x04,
]);

/// `constants.BlackholeAddr` (`0x0100..00`) — rewards disabled (fees burned).
pub const BLACKHOLE_ADDRESS: Address = Address::new([
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
]);

/// `AllowFeeRecipientsGasCost = WriteGasCostPerSlot + ReadAllowListGasCost`.
pub const ALLOW_FEE_RECIPIENTS_GAS: u64 = 20_000 + 5_000;
/// `AreFeeRecipientsAllowedGasCost = ReadAllowListGasCost`.
pub const ARE_FEE_RECIPIENTS_ALLOWED_GAS: u64 = 5_000;
/// `CurrentRewardAddressGasCost = ReadAllowListGasCost`.
pub const CURRENT_REWARD_ADDRESS_GAS: u64 = 5_000;
/// `DisableRewardsGasCost = WriteGasCostPerSlot + ReadAllowListGasCost`.
pub const DISABLE_REWARDS_GAS: u64 = 20_000 + 5_000;
/// `SetRewardAddressGasCost = WriteGasCostPerSlot + ReadAllowListGasCost`.
pub const SET_REWARD_ADDRESS_GAS: u64 = 20_000 + 5_000;

/// `FeeRecipientsAllowedEventGasCost = LogGas + 2·LogTopicGas` (`event.go`).
pub const FEE_RECIPIENTS_ALLOWED_EVENT_GAS: u64 = 375 + 375 * 2;
/// `RewardAddressChangedEventGasCost = LogGas + 4·LogTopicGas +
/// ReadGasCostPerSlot` (the read fetches the OLD reward address).
pub const REWARD_ADDRESS_CHANGED_EVENT_GAS: u64 = 375 + 375 * 4 + 5_000;
/// `RewardsDisabledEventGasCost = LogGas + 2·LogTopicGas`.
pub const REWARDS_DISABLED_EVENT_GAS: u64 = 375 + 375 * 2;

/// `allowFeeRecipients()`.
pub const SEL_ALLOW_FEE_RECIPIENTS: [u8; 4] = [0x03, 0x29, 0x09, 0x9f];
/// `areFeeRecipientsAllowed()`.
pub const SEL_ARE_FEE_RECIPIENTS_ALLOWED: [u8; 4] = [0xf6, 0x54, 0x2b, 0x2e];
/// `currentRewardAddress()`.
pub const SEL_CURRENT_REWARD_ADDRESS: [u8; 4] = [0xe9, 0x15, 0x60, 0x8b];
/// `disableRewards()`.
pub const SEL_DISABLE_REWARDS: [u8; 4] = [0xbc, 0x17, 0x86, 0x28];
/// `setRewardAddress(address)`.
pub const SEL_SET_REWARD_ADDRESS: [u8; 4] = [0x5e, 0x00, 0xe6, 0x79];

/// `keccak256("FeeRecipientsAllowed(address)")` — topic0 (indexed: sender).
pub const FEE_RECIPIENTS_ALLOWED_EVENT_TOPIC: [u8; 32] = [
    0xab, 0xb1, 0x94, 0x9b, 0xd1, 0x29, 0xfe, 0xf9, 0xb8, 0x46, 0x01, 0xa4, 0x8a, 0xee, 0x89, 0xd6,
    0x00, 0xd9, 0x00, 0x74, 0xca, 0x10, 0x21, 0x6a, 0x02, 0xce, 0x43, 0x99, 0x6b, 0xe5, 0x59, 0x91,
];
/// `keccak256("RewardAddressChanged(address,address,address)")` — topic0
/// (indexed: sender, oldRewardAddress, newRewardAddress; no data).
pub const REWARD_ADDRESS_CHANGED_EVENT_TOPIC: [u8; 32] = [
    0xc2, 0xa9, 0xe0, 0x7c, 0xba, 0x6f, 0x49, 0x20, 0xac, 0xaa, 0x59, 0x33, 0xbd, 0x04, 0x06, 0x94,
    0x9d, 0x5d, 0xbe, 0xf7, 0xee, 0x69, 0x8e, 0x78, 0x6e, 0xa2, 0x3e, 0x87, 0x08, 0xf3, 0x2a, 0x6c,
];
/// `keccak256("RewardsDisabled(address)")` — topic0 (indexed: sender).
pub const REWARDS_DISABLED_EVENT_TOPIC: [u8; 32] = [
    0xeb, 0x12, 0x1f, 0x03, 0x35, 0xef, 0xe8, 0xf4, 0xb8, 0xeb, 0xef, 0x77, 0x93, 0xc1, 0x8c, 0x17,
    0x18, 0x34, 0x69, 0x69, 0x89, 0x65, 0x6a, 0x8c, 0x34, 0x5a, 0xcc, 0x55, 0x83, 0x59, 0xfa, 0xbf,
];

/// `rewardAddressStorageKey = common.Hash{'r','a','s','k'}`.
#[must_use]
pub fn reward_address_storage_key() -> B256 {
    let mut k = [0u8; 32];
    k[..4].copy_from_slice(b"rask");
    B256::new(k)
}

/// `allowFeeRecipientsAddressValue = common.Hash{'a','f','r','a','v'}`.
#[must_use]
pub fn allow_fee_recipients_value() -> B256 {
    let mut k = [0u8; 32];
    k[..5].copy_from_slice(b"afrav");
    B256::new(k)
}

/// `GetStoredRewardAddress` — returns `(address, fee_recipients_allowed)`:
/// the slot value's low 20 bytes as an address, and whether the slot equals
/// the allow-fee-recipients sentinel.
///
/// # Errors
/// Propagates a fatal state-read failure.
pub fn get_stored_reward_address(
    state: &mut dyn PrecompileStateOps,
) -> Result<(Address, bool), PrecompileError> {
    let val = state.get_state(REWARD_MANAGER_ADDRESS, reward_address_storage_key())?;
    let addr = Address::from_slice(&val.as_slice()[12..]);
    Ok((addr, val == allow_fee_recipients_value()))
}

/// The RewardManager precompile body.
#[derive(Clone, Copy, Debug, Default)]
pub struct RewardManagerPrecompile;

impl RewardManagerPrecompile {
    /// The registry module at [`REWARD_MANAGER_ADDRESS`], activated at
    /// `activation`.
    #[must_use]
    pub fn module(self, activation: u64) -> PrecompileModule {
        PrecompileModule {
            address: REWARD_MANAGER_ADDRESS,
            activation,
            precompile: Arc::new(self),
        }
    }
}

impl StatefulPrecompile for RewardManagerPrecompile {
    fn run(
        &self,
        input: &[u8],
        gas_limit: u64,
        ctx: &PrecompileCtx,
        state: &mut dyn PrecompileStateOps,
    ) -> Result<InterpreterResult, PrecompileError> {
        let Some((selector, args)) = split_selector(input) else {
            return Ok(failure(gas_limit));
        };
        if let Some(res) = dispatch_allowlist(
            REWARD_MANAGER_ADDRESS,
            selector,
            args,
            gas_limit,
            ctx,
            state,
        ) {
            return res;
        }
        match selector {
            SEL_ALLOW_FEE_RECIPIENTS => allow_fee_recipients(gas_limit, ctx, state),
            SEL_ARE_FEE_RECIPIENTS_ALLOWED => are_fee_recipients_allowed(gas_limit, state),
            SEL_CURRENT_REWARD_ADDRESS => current_reward_address(gas_limit, state),
            SEL_DISABLE_REWARDS => disable_rewards(gas_limit, ctx, state),
            SEL_SET_REWARD_ADDRESS => set_reward_address(args, gas_limit, ctx, state),
            _ => Ok(failure(gas_limit)),
        }
    }
}

/// `allowFeeRecipients` — deduct → write-protection → allow-list gate →
/// Durango event → store the sentinel.
fn allow_fee_recipients(
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(ALLOW_FEE_RECIPIENTS_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    if ctx.read_only {
        return Ok(failure(gas_limit));
    }
    if !get_allow_list_status(state, REWARD_MANAGER_ADDRESS, ctx.caller)?.is_enabled() {
        // Go `ErrCannotAllowFeeRecipients`.
        return Ok(failure(gas_limit));
    }
    if ctx.block.is_durango {
        if !g.record_regular_cost(FEE_RECIPIENTS_ALLOWED_EVENT_GAS) {
            return Ok(out_of_gas(gas_limit));
        }
        state.add_log(
            REWARD_MANAGER_ADDRESS,
            vec![
                B256::from(FEE_RECIPIENTS_ALLOWED_EVENT_TOPIC),
                B256::new(word_addr(ctx.caller)),
            ],
            Vec::new(),
        );
    }
    state.set_state(
        REWARD_MANAGER_ADDRESS,
        reward_address_storage_key(),
        allow_fee_recipients_value(),
    )?;
    Ok(success(Vec::new(), g))
}

/// `areFeeRecipientsAllowed` — deduct → read the slot → bool word.
fn are_fee_recipients_allowed(
    gas_limit: u64,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(ARE_FEE_RECIPIENTS_ALLOWED_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    let (_, allowed) = get_stored_reward_address(state)?;
    Ok(success(word_bool(allowed).to_vec(), g))
}

/// `currentRewardAddress` — deduct → read the slot → address word.
fn current_reward_address(
    gas_limit: u64,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(CURRENT_REWARD_ADDRESS_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    let (addr, _) = get_stored_reward_address(state)?;
    Ok(success(word_addr(addr).to_vec(), g))
}

/// `disableRewards` — deduct → write-protection → allow-list gate → Durango
/// event → store the blackhole address (fees burned).
fn disable_rewards(
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(DISABLE_REWARDS_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    if ctx.read_only {
        return Ok(failure(gas_limit));
    }
    if !get_allow_list_status(state, REWARD_MANAGER_ADDRESS, ctx.caller)?.is_enabled() {
        // Go `ErrCannotDisableRewards`.
        return Ok(failure(gas_limit));
    }
    if ctx.block.is_durango {
        if !g.record_regular_cost(REWARDS_DISABLED_EVENT_GAS) {
            return Ok(out_of_gas(gas_limit));
        }
        state.add_log(
            REWARD_MANAGER_ADDRESS,
            vec![
                B256::from(REWARDS_DISABLED_EVENT_TOPIC),
                B256::new(word_addr(ctx.caller)),
            ],
            Vec::new(),
        );
    }
    state.set_state(
        REWARD_MANAGER_ADDRESS,
        reward_address_storage_key(),
        B256::new(word_addr(BLACKHOLE_ADDRESS)),
    )?;
    Ok(success(Vec::new(), g))
}

/// `setRewardAddress` — deduct → write-protection → unpack (strict pre-Durango:
/// length divisible by 32) → allow-list gate → empty-address check → Durango
/// event → store.
fn set_reward_address(
    args: &[u8],
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(SET_REWARD_ADDRESS_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    if ctx.read_only {
        return Ok(failure(gas_limit));
    }
    // Pre-Durango strict mode only requires `len % 32 == 0` (Go
    // `UnpackSetRewardAddressInput`), unlike the other precompiles' exact-len.
    if !ctx.block.is_durango && !args.len().is_multiple_of(32) {
        return Ok(failure(gas_limit));
    }
    let Some(reward_address) = read_addr(args, 0) else {
        return Ok(failure(gas_limit));
    };
    if !get_allow_list_status(state, REWARD_MANAGER_ADDRESS, ctx.caller)?.is_enabled() {
        // Go `ErrCannotSetRewardAddress`.
        return Ok(failure(gas_limit));
    }
    if reward_address == Address::ZERO {
        // Go `ErrEmptyRewardAddress`.
        return Ok(failure(gas_limit));
    }
    if ctx.block.is_durango {
        if !g.record_regular_cost(REWARD_ADDRESS_CHANGED_EVENT_GAS) {
            return Ok(out_of_gas(gas_limit));
        }
        let (old, _) = get_stored_reward_address(state)?;
        state.add_log(
            REWARD_MANAGER_ADDRESS,
            vec![
                B256::from(REWARD_ADDRESS_CHANGED_EVENT_TOPIC),
                B256::new(word_addr(ctx.caller)),
                B256::new(word_addr(old)),
                B256::new(word_addr(reward_address)),
            ],
            Vec::new(),
        );
    }
    state.set_state(
        REWARD_MANAGER_ADDRESS,
        reward_address_storage_key(),
        B256::new(word_addr(reward_address)),
    )?;
    Ok(success(Vec::new(), g))
}
