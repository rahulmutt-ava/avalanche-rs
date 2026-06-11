// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The GasPriceManager stateful precompile — port of subnet-evm
//! `precompile/contracts/gaspricemanager` (M6.31, spec 10 §8). Stores the
//! dynamic gas limit/price config (5 fields packed into ONE slot) +
//! `lastChangedAt`, gated by the embedded allow list at its own address.
//!
//! ## Storage layout (`contract.go` `storageSlot`)
//!
//! Slots are namespaced with the left-aligned prefix `"gasprm"` (avoiding the
//! right-aligned address-keyed AllowList role slots): the config at
//! `"gasprm" ++ "gp"`, `lastChangedAt` at `"gasprm" ++ "lca"`. The config
//! value packs (`commontype.GasPriceConfig.Pack`): byte 0
//! `validatorTargetGas` (bool), bytes 1..9 `targetGas` (u64 BE), byte 9
//! `staticPricing` (bool), bytes 10..18 `minGasPrice` (u64 BE), bytes 18..26
//! `timeToDouble` (u64 BE), bytes 26..32 zero.
//!
//! Unlike FeeManager/RewardManager, this precompile post-dates Durango: there
//! is no strict-ABI/pre-Durango branch and the update event is ALWAYS emitted.

use std::sync::Arc;

use ava_evm_reth::{Address, B256, Gas, InterpreterResult, PrecompileError};

use crate::precompile::abi::{
    failure, out_of_gas, read_bool, read_u64, success, word_addr, word_bool, word_u64,
};
use crate::precompile::allowlist::{dispatch_allowlist, get_allow_list_status, split_selector};
use crate::precompile::registry::{
    PrecompileCtx, PrecompileModule, PrecompileStateOps, StatefulPrecompile,
};

/// `gaspricemanager.ContractAddress` (`0x02..06`).
pub const GAS_PRICE_MANAGER_ADDRESS: Address = Address::new([
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x06,
]);

/// `numGasPriceConfigField` — validatorTargetGas, targetGas, staticPricing,
/// minGasPrice, timeToDouble.
pub const NUM_GAS_PRICE_CONFIG_FIELDS: usize = 5;
/// `commontype.MinTargetGas`.
pub const MIN_TARGET_GAS: u64 = 1_000_000;

/// `getGasPriceConfigGasCost = ReadGasCostPerSlot`.
pub const GET_GAS_PRICE_CONFIG_GAS: u64 = 5_000;
/// `getGasPriceConfigLastChangedAtGasCost = ReadGasCostPerSlot`.
pub const GET_LAST_CHANGED_AT_GAS: u64 = 5_000;
/// `gasPriceConfigUpdatedEventGasCost = LogGas + 2·LogTopicGas +
/// LogDataGas·5·32·2` (old + new config, 5 words each).
pub const GAS_PRICE_CONFIG_UPDATED_EVENT_GAS: u64 = 375 + 375 * 2 + 8 * 5 * 32 * 2;
/// `setGasPriceConfigGasCost = ReadAllowListGasCost + 2·WriteGasCostPerSlot +
/// getGasPriceConfigGasCost + gasPriceConfigUpdatedEventGasCost`.
pub const SET_GAS_PRICE_CONFIG_GAS: u64 =
    5_000 + 2 * 20_000 + GET_GAS_PRICE_CONFIG_GAS + GAS_PRICE_CONFIG_UPDATED_EVENT_GAS;

/// `getGasPriceConfig()`.
pub const SEL_GET_GAS_PRICE_CONFIG: [u8; 4] = [0x44, 0x58, 0x21, 0xe3];
/// `getGasPriceConfigLastChangedAt()`.
pub const SEL_GET_GAS_PRICE_CONFIG_LAST_CHANGED_AT: [u8; 4] = [0xeb, 0x8d, 0xf3, 0x96];
/// `setGasPriceConfig((bool,uint64,bool,uint64,uint64))`.
pub const SEL_SET_GAS_PRICE_CONFIG: [u8; 4] = [0x65, 0xc9, 0x7a, 0x28];

/// `keccak256("GasPriceConfigUpdated(address,(bool,uint64,bool,uint64,uint64),
/// (bool,uint64,bool,uint64,uint64))")` — topic0 (indexed: sender; data:
/// `abi.encode(oldConfig, newConfig)`).
pub const GAS_PRICE_CONFIG_UPDATED_EVENT_TOPIC: [u8; 32] = [
    0x94, 0x56, 0x35, 0x6f, 0x1b, 0x11, 0xd0, 0x7d, 0x47, 0xf9, 0xbd, 0x3d, 0x56, 0x82, 0x79, 0xa9,
    0x26, 0xea, 0x8c, 0xac, 0xee, 0xc0, 0xe6, 0x07, 0x8e, 0x42, 0x6e, 0xae, 0xa5, 0x79, 0xbe, 0x66,
];

/// `storageSlot(key…)` — `"gasprm"` left-aligned, then `key` (`contract.go`).
fn storage_slot(key: &[u8]) -> B256 {
    let mut k = [0u8; 32];
    k[..6].copy_from_slice(b"gasprm");
    k[6..6 + key.len()].copy_from_slice(key);
    B256::new(k)
}

/// `gasPriceConfigStorageKey = storageSlot('g','p')`.
#[must_use]
pub fn gas_price_config_storage_key() -> B256 {
    storage_slot(b"gp")
}

/// `gasPriceConfigLastChangedAtKey = storageSlot('l','c','a')`.
#[must_use]
pub fn gas_price_config_last_changed_at_key() -> B256 {
    storage_slot(b"lca")
}

/// `commontype.GasPriceConfig` — the 5-field dynamic gas price config.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GasPriceConfig {
    /// `validatorTargetGas` — validators control targetGas via preferences.
    pub validator_target_gas: bool,
    /// `targetGas` — target gas consumption per second.
    pub target_gas: u64,
    /// `staticPricing` — gas price is always `minGasPrice`.
    pub static_pricing: bool,
    /// `minGasPrice` — minimum gas price in wei.
    pub min_gas_price: u64,
    /// `timeToDouble` — seconds for the price to double at max capacity.
    pub time_to_double: u64,
}

impl GasPriceConfig {
    /// `commontype.GasPriceConfig.Verify`.
    #[must_use]
    pub fn verify(&self) -> bool {
        if self.min_gas_price == 0 {
            return false;
        }
        if self.validator_target_gas {
            if self.target_gas != 0 {
                return false;
            }
        } else if self.target_gas < MIN_TARGET_GAS {
            return false;
        }
        if self.static_pricing {
            self.time_to_double == 0
        } else {
            self.time_to_double != 0
        }
    }

    /// `Pack` — the single 32-byte storage word (layout in the module docs).
    #[must_use]
    pub fn pack(&self) -> B256 {
        let mut h = [0u8; 32];
        if self.validator_target_gas {
            h[0] = 1;
        }
        h[1..9].copy_from_slice(&self.target_gas.to_be_bytes());
        if self.static_pricing {
            h[9] = 1;
        }
        h[10..18].copy_from_slice(&self.min_gas_price.to_be_bytes());
        h[18..26].copy_from_slice(&self.time_to_double.to_be_bytes());
        B256::new(h)
    }

    /// `UnpackFrom` — decode the packed storage word.
    #[must_use]
    pub fn unpack(h: B256) -> Self {
        let b = h.as_slice();
        let u64_at = |i: usize| {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&b[i..i + 8]);
            u64::from_be_bytes(buf)
        };
        Self {
            validator_target_gas: b[0] != 0,
            target_gas: u64_at(1),
            static_pricing: b[9] != 0,
            min_gas_price: u64_at(10),
            time_to_double: u64_at(18),
        }
    }

    /// The 5 ABI words of the `(bool,uint64,bool,uint64,uint64)` tuple, in
    /// field order (used for both the call output and the event data).
    #[must_use]
    pub fn abi_words(&self) -> [B256; NUM_GAS_PRICE_CONFIG_FIELDS] {
        [
            B256::new(word_bool(self.validator_target_gas)),
            B256::new(word_u64(self.target_gas)),
            B256::new(word_bool(self.static_pricing)),
            B256::new(word_u64(self.min_gas_price)),
            B256::new(word_u64(self.time_to_double)),
        ]
    }

    /// Parse from the 5 consecutive ABI words of the call args (the static
    /// tuple is encoded inline).
    #[must_use]
    pub fn from_args(args: &[u8]) -> Option<Self> {
        Some(Self {
            validator_target_gas: read_bool(args, 0)?,
            target_gas: read_u64(args, 1)?,
            static_pricing: read_bool(args, 2)?,
            min_gas_price: read_u64(args, 3)?,
            time_to_double: read_u64(args, 4)?,
        })
    }
}

/// `GetStoredGasPriceConfig` — read + unpack the config slot.
///
/// # Errors
/// Propagates a fatal state-read failure.
pub fn get_stored_gas_price_config(
    state: &mut dyn PrecompileStateOps,
) -> Result<GasPriceConfig, PrecompileError> {
    let word = state.get_state(GAS_PRICE_MANAGER_ADDRESS, gas_price_config_storage_key())?;
    Ok(GasPriceConfig::unpack(word))
}

/// The GasPriceManager precompile body.
#[derive(Clone, Copy, Debug, Default)]
pub struct GasPriceManagerPrecompile;

impl GasPriceManagerPrecompile {
    /// The registry module at [`GAS_PRICE_MANAGER_ADDRESS`], activated at
    /// `activation`.
    #[must_use]
    pub fn module(self, activation: u64) -> PrecompileModule {
        PrecompileModule {
            address: GAS_PRICE_MANAGER_ADDRESS,
            activation,
            precompile: Arc::new(self),
        }
    }
}

impl StatefulPrecompile for GasPriceManagerPrecompile {
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
            GAS_PRICE_MANAGER_ADDRESS,
            selector,
            args,
            gas_limit,
            ctx,
            state,
        ) {
            return res;
        }
        match selector {
            SEL_GET_GAS_PRICE_CONFIG => get_gas_price_config(gas_limit, state),
            SEL_GET_GAS_PRICE_CONFIG_LAST_CHANGED_AT => {
                get_gas_price_config_last_changed_at(gas_limit, state)
            }
            SEL_SET_GAS_PRICE_CONFIG => set_gas_price_config(args, gas_limit, ctx, state),
            _ => Ok(failure(gas_limit)),
        }
    }
}

/// `getGasPriceConfig` — deduct → read + unpack → 5-word tuple output.
fn get_gas_price_config(
    gas_limit: u64,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(GET_GAS_PRICE_CONFIG_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    let config = get_stored_gas_price_config(state)?;
    let mut out = Vec::with_capacity(32 * NUM_GAS_PRICE_CONFIG_FIELDS);
    for w in config.abi_words() {
        out.extend_from_slice(w.as_slice());
    }
    Ok(success(out, g))
}

/// `getGasPriceConfigLastChangedAt` — deduct → read the `lca` slot.
fn get_gas_price_config_last_changed_at(
    gas_limit: u64,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(GET_LAST_CHANGED_AT_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    let val = state.get_state(
        GAS_PRICE_MANAGER_ADDRESS,
        gas_price_config_last_changed_at_key(),
    )?;
    Ok(success(val.as_slice().to_vec(), g))
}

/// `setGasPriceConfig` — deduct (all upfront) → write-protection → allow-list
/// gate (BEFORE unpack, unlike FeeManager) → unpack → read old → verify +
/// store config + `lastChangedAt` → emit `GasPriceConfigUpdated` (always — no
/// Durango branch).
fn set_gas_price_config(
    args: &[u8],
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(SET_GAS_PRICE_CONFIG_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    if ctx.read_only {
        return Ok(failure(gas_limit));
    }
    if !get_allow_list_status(state, GAS_PRICE_MANAGER_ADDRESS, ctx.caller)?.is_enabled() {
        // Go `errCannotSetGasPriceConfig`.
        return Ok(failure(gas_limit));
    }
    let Some(config) = GasPriceConfig::from_args(args) else {
        return Ok(failure(gas_limit));
    };
    let old = get_stored_gas_price_config(state)?;

    // `StoreGasPriceConfig`: Verify, then write config + lastChangedAt.
    if !config.verify() {
        return Ok(failure(gas_limit));
    }
    state.set_state(
        GAS_PRICE_MANAGER_ADDRESS,
        gas_price_config_storage_key(),
        config.pack(),
    )?;
    state.set_state(
        GAS_PRICE_MANAGER_ADDRESS,
        gas_price_config_last_changed_at_key(),
        B256::new(word_u64(ctx.block.block_number)),
    )?;

    let mut data = Vec::with_capacity(32 * NUM_GAS_PRICE_CONFIG_FIELDS * 2);
    for w in old.abi_words() {
        data.extend_from_slice(w.as_slice());
    }
    for w in config.abi_words() {
        data.extend_from_slice(w.as_slice());
    }
    state.add_log(
        GAS_PRICE_MANAGER_ADDRESS,
        vec![
            B256::from(GAS_PRICE_CONFIG_UPDATED_EVENT_TOPIC),
            B256::new(word_addr(ctx.caller)),
        ],
        data,
    );
    Ok(success(Vec::new(), g))
}
