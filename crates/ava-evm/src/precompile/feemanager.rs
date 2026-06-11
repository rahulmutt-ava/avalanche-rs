// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The FeeConfigManager stateful precompile — port of subnet-evm
//! `precompile/contracts/feemanager` (M6.31, spec 10 §8). Stores the dynamic
//! fee config (8 fields + `lastChangedAt`) in its own storage, gated by the
//! embedded allow list at its own address.
//!
//! ## Storage layout (`contract.go`)
//!
//! Field `i` (1-based, `gasLimit`=1 .. `blockGasCostStep`=8) lives at the slot
//! whose FIRST byte is `i` (Go `common.Hash{byte(i)}` — left-aligned), the
//! value the 32-byte big-endian field. `lastChangedAt` lives at the slot whose
//! first three bytes are `"lca"`.

use std::sync::Arc;

use ava_evm_reth::{Address, B256, Gas, InterpreterResult, PrecompileError, U256};

use crate::precompile::abi::{
    check_args_len, failure, out_of_gas, read_u256, success, word_addr, word_u64, word_u256,
};
use crate::precompile::allowlist::{dispatch_allowlist, get_allow_list_status, split_selector};
use crate::precompile::registry::{
    PrecompileCtx, PrecompileModule, PrecompileStateOps, StatefulPrecompile,
};

/// `feemanager.ContractAddress` (`0x02..03`).
pub const FEE_MANAGER_ADDRESS: Address = Address::new([
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x03,
]);

/// `numFeeConfigField` — the 8 stored fee-config fields.
pub const NUM_FEE_CONFIG_FIELDS: usize = 8;

/// `SetFeeConfigGasCost = WriteGasCostPerSlot * (numFeeConfigField + 1)` (the
/// +1 is the `lastChangedAt` slot).
pub const SET_FEE_CONFIG_GAS: u64 = 20_000 * (NUM_FEE_CONFIG_FIELDS as u64 + 1);
/// `GetFeeConfigGasCost = ReadGasCostPerSlot * numFeeConfigField`.
pub const GET_FEE_CONFIG_GAS: u64 = 5_000 * NUM_FEE_CONFIG_FIELDS as u64;
/// `GetLastChangedAtGasCost = ReadGasCostPerSlot`.
pub const GET_LAST_CHANGED_AT_GAS: u64 = 5_000;
/// `FeeConfigChangedEventGasCost = GetFeeConfigGasCost + LogGas + 2·LogTopicGas
/// + 2·(8·32)·LogDataGas` (`event.go`).
pub const FEE_CONFIG_CHANGED_EVENT_GAS: u64 = GET_FEE_CONFIG_GAS + 375 + 375 * 2 + 2 * 256 * 8;

/// `getFeeConfig()`.
pub const SEL_GET_FEE_CONFIG: [u8; 4] = [0x5f, 0xbb, 0xc0, 0xd2];
/// `getFeeConfigLastChangedAt()`.
pub const SEL_GET_FEE_CONFIG_LAST_CHANGED_AT: [u8; 4] = [0x9e, 0x05, 0x54, 0x9a];
/// `setFeeConfig(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256)`.
pub const SEL_SET_FEE_CONFIG: [u8; 4] = [0x8f, 0x10, 0xb5, 0x86];

/// `keccak256("FeeConfigChanged(address,(uint256,…×8),(uint256,…×8))")` —
/// topic0 (indexed: sender; data: `abi.encode(oldConfig, newConfig)`).
pub const FEE_CONFIG_CHANGED_EVENT_TOPIC: [u8; 32] = [
    0x4c, 0x98, 0xe4, 0x3a, 0xdb, 0x59, 0x62, 0xc1, 0x8f, 0x3f, 0x0e, 0x6d, 0xd0, 0x66, 0xe2, 0xa2,
    0xde, 0x25, 0x8d, 0x3b, 0x4f, 0x69, 0x5b, 0x31, 0x7b, 0x77, 0xc8, 0xf2, 0x7c, 0xd0, 0x44, 0xfc,
];

/// The slot of fee-config field `i` (1-based): first byte `i`, rest zero (Go
/// `common.Hash{byte(i)}`).
#[must_use]
pub fn fee_config_field_key(i: u8) -> B256 {
    let mut k = [0u8; 32];
    k[0] = i;
    B256::new(k)
}

/// `feeConfigLastChangedAtKey = common.Hash{'l','c','a'}`.
#[must_use]
pub fn fee_config_last_changed_at_key() -> B256 {
    let mut k = [0u8; 32];
    k[0] = b'l';
    k[1] = b'c';
    k[2] = b'a';
    B256::new(k)
}

/// The 8-field fee config (`commontype.FeeConfig`), each field one ABI word.
/// `target_block_rate` is stored/encoded as the LOW 64 bits of the supplied
/// word (Go `.Uint64()` truncation in `UnpackSetFeeConfigInput`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeeConfig {
    /// `gasLimit` (field 1).
    pub gas_limit: U256,
    /// `targetBlockRate` (field 2; uint64 semantics).
    pub target_block_rate: u64,
    /// `minBaseFee` (field 3).
    pub min_base_fee: U256,
    /// `targetGas` (field 4).
    pub target_gas: U256,
    /// `baseFeeChangeDenominator` (field 5).
    pub base_fee_change_denominator: U256,
    /// `minBlockGasCost` (field 6).
    pub min_block_gas_cost: U256,
    /// `maxBlockGasCost` (field 7).
    pub max_block_gas_cost: U256,
    /// `blockGasCostStep` (field 8).
    pub block_gas_cost_step: U256,
}

impl FeeConfig {
    /// `commontype.FeeConfig.Verify` over ABI-sourced (unsigned) fields: the
    /// `nil` and negative branches cannot fire, leaving the positivity /
    /// ordering / uint64-fit checks.
    #[must_use]
    pub fn verify(&self) -> bool {
        !self.gas_limit.is_zero()
            && self.target_block_rate > 0
            && !self.target_gas.is_zero()
            && !self.base_fee_change_denominator.is_zero()
            && self.min_block_gas_cost <= self.max_block_gas_cost
            && self.max_block_gas_cost <= U256::from(u64::MAX)
    }

    /// The 8 storage words in field order (`StoreFeeConfig`).
    #[must_use]
    pub fn words(&self) -> [B256; NUM_FEE_CONFIG_FIELDS] {
        [
            B256::new(word_u256(self.gas_limit)),
            B256::new(word_u64(self.target_block_rate)),
            B256::new(word_u256(self.min_base_fee)),
            B256::new(word_u256(self.target_gas)),
            B256::new(word_u256(self.base_fee_change_denominator)),
            B256::new(word_u256(self.min_block_gas_cost)),
            B256::new(word_u256(self.max_block_gas_cost)),
            B256::new(word_u256(self.block_gas_cost_step)),
        ]
    }

    /// Parse from 8 consecutive ABI words (`UnpackSetFeeConfigInput`).
    #[must_use]
    pub fn from_args(args: &[u8]) -> Option<Self> {
        Some(Self {
            gas_limit: read_u256(args, 0)?,
            target_block_rate: read_u256(args, 1)?.as_limbs()[0],
            min_base_fee: read_u256(args, 2)?,
            target_gas: read_u256(args, 3)?,
            base_fee_change_denominator: read_u256(args, 4)?,
            min_block_gas_cost: read_u256(args, 5)?,
            max_block_gas_cost: read_u256(args, 6)?,
            block_gas_cost_step: read_u256(args, 7)?,
        })
    }
}

/// `GetStoredFeeConfig` — read the 8 field slots.
///
/// # Errors
/// Propagates a fatal state-read failure.
pub fn get_stored_fee_config(
    state: &mut dyn PrecompileStateOps,
) -> Result<FeeConfig, PrecompileError> {
    let mut words = [B256::ZERO; NUM_FEE_CONFIG_FIELDS];
    for (i, w) in words.iter_mut().enumerate() {
        #[allow(clippy::cast_possible_truncation)] // i < 8
        let key = fee_config_field_key(i as u8 + 1);
        *w = state.get_state(FEE_MANAGER_ADDRESS, key)?;
    }
    Ok(FeeConfig {
        gas_limit: U256::from_be_bytes(words[0].0),
        target_block_rate: U256::from_be_bytes(words[1].0).as_limbs()[0],
        min_base_fee: U256::from_be_bytes(words[2].0),
        target_gas: U256::from_be_bytes(words[3].0),
        base_fee_change_denominator: U256::from_be_bytes(words[4].0),
        min_block_gas_cost: U256::from_be_bytes(words[5].0),
        max_block_gas_cost: U256::from_be_bytes(words[6].0),
        block_gas_cost_step: U256::from_be_bytes(words[7].0),
    })
}

/// The FeeManager precompile body.
#[derive(Clone, Copy, Debug, Default)]
pub struct FeeManagerPrecompile;

impl FeeManagerPrecompile {
    /// The registry module at [`FEE_MANAGER_ADDRESS`], activated at
    /// `activation`.
    #[must_use]
    pub fn module(self, activation: u64) -> PrecompileModule {
        PrecompileModule {
            address: FEE_MANAGER_ADDRESS,
            activation,
            precompile: Arc::new(self),
        }
    }
}

impl StatefulPrecompile for FeeManagerPrecompile {
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
        if let Some(res) =
            dispatch_allowlist(FEE_MANAGER_ADDRESS, selector, args, gas_limit, ctx, state)
        {
            return res;
        }
        match selector {
            SEL_GET_FEE_CONFIG => get_fee_config(gas_limit, state),
            SEL_GET_FEE_CONFIG_LAST_CHANGED_AT => get_fee_config_last_changed_at(gas_limit, state),
            SEL_SET_FEE_CONFIG => set_fee_config(args, gas_limit, ctx, state),
            _ => Ok(failure(gas_limit)),
        }
    }
}

/// `getFeeConfig` — deduct gas → read the 8 slots → 8-word output.
fn get_fee_config(
    gas_limit: u64,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(GET_FEE_CONFIG_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    let config = get_stored_fee_config(state)?;
    let mut out = Vec::with_capacity(32 * NUM_FEE_CONFIG_FIELDS);
    for w in config.words() {
        out.extend_from_slice(w.as_slice());
    }
    Ok(success(out, g))
}

/// `getFeeConfigLastChangedAt` — deduct gas → read the `lca` slot.
fn get_fee_config_last_changed_at(
    gas_limit: u64,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(GET_LAST_CHANGED_AT_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    let val = state.get_state(FEE_MANAGER_ADDRESS, fee_config_last_changed_at_key())?;
    Ok(success(val.as_slice().to_vec(), g))
}

/// `setFeeConfig` (`contract.go`): deduct gas → write-protection → unpack
/// (strict pre-Durango) → allow-list gate → Durango event → verify → store the
/// 8 fields + `lastChangedAt = block number`.
fn set_fee_config(
    args: &[u8],
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(SET_FEE_CONFIG_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    if ctx.read_only {
        return Ok(failure(gas_limit));
    }
    if !check_args_len(args, NUM_FEE_CONFIG_FIELDS, !ctx.block.is_durango) {
        return Ok(failure(gas_limit));
    }
    let Some(config) = FeeConfig::from_args(args) else {
        return Ok(failure(gas_limit));
    };
    if !get_allow_list_status(state, FEE_MANAGER_ADDRESS, ctx.caller)?.is_enabled() {
        // Go `ErrCannotChangeFee`.
        return Ok(failure(gas_limit));
    }
    if ctx.block.is_durango {
        if !g.record_regular_cost(FEE_CONFIG_CHANGED_EVENT_GAS) {
            return Ok(out_of_gas(gas_limit));
        }
        let old = get_stored_fee_config(state)?;
        let mut data = Vec::with_capacity(32 * NUM_FEE_CONFIG_FIELDS * 2);
        for w in old.words() {
            data.extend_from_slice(w.as_slice());
        }
        for w in config.words() {
            data.extend_from_slice(w.as_slice());
        }
        state.add_log(
            FEE_MANAGER_ADDRESS,
            vec![
                B256::from(FEE_CONFIG_CHANGED_EVENT_TOPIC),
                B256::new(word_addr(ctx.caller)),
            ],
            data,
        );
    }
    // `StoreFeeConfig`: Verify, then write the 8 fields + lastChangedAt.
    if !config.verify() {
        return Ok(failure(gas_limit));
    }
    for (i, w) in config.words().iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)] // i < 8
        let key = fee_config_field_key(i as u8 + 1);
        state.set_state(FEE_MANAGER_ADDRESS, key, *w)?;
    }
    state.set_state(
        FEE_MANAGER_ADDRESS,
        fee_config_last_changed_at_key(),
        B256::new(word_u64(ctx.block.block_number)),
    )?;
    Ok(success(Vec::new(), g))
}
