// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! ConfigKey precompile golden tests (M6.31, spec 10 §8): byte-exact parity of
//! FeeManager / RewardManager / GasPriceManager (and the embedded allow-list
//! role gate + NativeMinter mint) against Go-oracle vectors emitted from
//! subnet-evm's own ABI packers (`tests/vectors/cchain/precompile/
//! configkey_golden.json`): calldata, outputs, storage slots/values, event
//! topics/data, and the gas tables.

use std::str::FromStr;
use std::sync::Arc;

use ava_evm::precompile::allowlist::{Role, allow_list_key};
use ava_evm::precompile::feemanager::{FEE_MANAGER_ADDRESS, FeeManagerPrecompile};
use ava_evm::precompile::gaspricemanager::{GAS_PRICE_MANAGER_ADDRESS, GasPriceManagerPrecompile};
use ava_evm::precompile::nativeminter::{NATIVE_MINTER_ADDRESS, NativeMinterPrecompile};
use ava_evm::precompile::registry::{
    AvaBlockCtx, MemStateOps, PrecompileCtx, PredicateResults, StatefulPrecompile,
};
use ava_evm::precompile::rewardmanager::{REWARD_MANAGER_ADDRESS, RewardManagerPrecompile};
use ava_evm_reth::{Address, B256, InstructionResult, U256};

const GAS: u64 = 10_000_000;

#[derive(serde::Deserialize)]
struct Golden {
    caller: String,
    feemanager: FeeManagerGolden,
    rewardmanager: RewardManagerGolden,
    gaspricemanager: GasPriceManagerGolden,
}

#[derive(serde::Deserialize)]
struct FeeManagerGolden {
    address: String,
    set_fee_config_calldata: String,
    get_fee_config_output: String,
    get_last_changed_at_output_block7: String,
    event_topics: Vec<String>,
    event_data: String,
    set_fee_config_gas: u64,
    get_fee_config_gas: u64,
    get_last_changed_at_gas: u64,
    event_gas: u64,
    field_keys: Vec<String>,
    last_changed_at_key: String,
    stored_words: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RewardManagerGolden {
    address: String,
    set_reward_address_calldata: String,
    current_reward_address_output: String,
    are_fee_recipients_allowed_output_true: String,
    reward_address_changed_topics: Vec<String>,
    reward_address_changed_data: String,
    fee_recipients_allowed_topics: Vec<String>,
    rewards_disabled_topics: Vec<String>,
    allow_fee_recipients_gas: u64,
    are_fee_recipients_allowed_gas: u64,
    current_reward_address_gas: u64,
    disable_rewards_gas: u64,
    set_reward_address_gas: u64,
    fee_recipients_allowed_event_gas: u64,
    reward_address_changed_event_gas: u64,
    rewards_disabled_event_gas: u64,
    reward_address_storage_key: String,
    allow_fee_recipients_value: String,
    blackhole_value: String,
    stored_reward_value: String,
}

#[derive(serde::Deserialize)]
struct GasPriceManagerGolden {
    address: String,
    set_gas_price_config_calldata: String,
    get_gas_price_config_output: String,
    get_last_changed_at_output_block7: String,
    event_topics: Vec<String>,
    event_data: String,
    packed_config_word: String,
    packed_default_word: String,
}

fn golden() -> Golden {
    let raw = include_str!("vectors/cchain/precompile/configkey_golden.json");
    serde_json::from_str(raw).expect("parse golden")
}

fn hex_bytes(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).expect("hex")
}

fn b256(s: &str) -> B256 {
    B256::from_str(s).expect("b256")
}

fn addr(s: &str) -> Address {
    Address::from_str(s).expect("addr")
}

/// A Durango-active block-7 ctx for `caller`.
fn ctx(caller: Address) -> PrecompileCtx {
    PrecompileCtx {
        caller,
        value: U256::ZERO,
        read_only: false,
        predicates: Arc::new(PredicateResults::default()),
        block: AvaBlockCtx {
            pchain_height: 0,
            timestamp: 0,
            current_tx_index: 0,
            block_number: 7,
            is_durango: true,
        },
    }
}

/// Grants `role` to `caller` in `contract`'s embedded allow list.
fn grant(ops: &mut MemStateOps, contract: Address, caller: Address, role: Role) {
    ops.storage
        .insert((contract, allow_list_key(caller)), role.word());
}

#[test]
fn feemanager_golden() {
    let g = golden();
    let caller = addr(&g.caller);
    assert_eq!(addr(&g.feemanager.address), FEE_MANAGER_ADDRESS);

    let fm = FeeManagerPrecompile;
    let mut ops = MemStateOps::default();
    grant(&mut ops, FEE_MANAGER_ADDRESS, caller, Role::Admin);

    // Seed the OLD config the golden event encodes (all-ones-ish baseline).
    let old_words = [1u64, 1, 1, 1, 1, 0, 10, 1];
    for (i, v) in old_words.iter().enumerate() {
        let key = b256(&g.feemanager.field_keys[i]);
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&v.to_be_bytes());
        ops.storage.insert((FEE_MANAGER_ADDRESS, key), B256::new(w));
    }

    // setFeeConfig: success, Go gas (base + Durango event), storage + event
    // byte-exact.
    let out = fm
        .run(
            &hex_bytes(&g.feemanager.set_fee_config_calldata),
            GAS,
            &ctx(caller),
            &mut ops,
        )
        .expect("setFeeConfig");
    assert_eq!(out.result, InstructionResult::Return);
    assert_eq!(
        out.gas.total_gas_spent(),
        g.feemanager.set_fee_config_gas + g.feemanager.event_gas
    );
    for (i, want) in g.feemanager.stored_words.iter().enumerate() {
        let key = b256(&g.feemanager.field_keys[i]);
        assert_eq!(
            ops.storage[&(FEE_MANAGER_ADDRESS, key)],
            b256(want),
            "fee config field {} storage parity",
            i + 1
        );
    }
    assert_eq!(
        ops.storage[&(FEE_MANAGER_ADDRESS, b256(&g.feemanager.last_changed_at_key))],
        b256(&g.feemanager.get_last_changed_at_output_block7),
        "lastChangedAt = block number"
    );
    let (log_addr, topics, data) = ops.logs.last().expect("FeeConfigChanged log").clone();
    assert_eq!(log_addr, FEE_MANAGER_ADDRESS);
    let want_topics: Vec<B256> = g.feemanager.event_topics.iter().map(|t| b256(t)).collect();
    assert_eq!(topics, want_topics);
    assert_eq!(data, hex_bytes(&g.feemanager.event_data));

    // getFeeConfig: output + gas byte-exact.
    let out = fm
        .run(&[0x5f, 0xbb, 0xc0, 0xd2], GAS, &ctx(caller), &mut ops)
        .expect("getFeeConfig");
    assert_eq!(out.result, InstructionResult::Return);
    assert_eq!(
        out.output.as_ref(),
        hex_bytes(&g.feemanager.get_fee_config_output)
    );
    assert_eq!(out.gas.total_gas_spent(), g.feemanager.get_fee_config_gas);

    // getFeeConfigLastChangedAt.
    let out = fm
        .run(&[0x9e, 0x05, 0x54, 0x9a], GAS, &ctx(caller), &mut ops)
        .expect("getFeeConfigLastChangedAt");
    assert_eq!(
        out.output.as_ref(),
        hex_bytes(&g.feemanager.get_last_changed_at_output_block7)
    );
    assert_eq!(
        out.gas.total_gas_spent(),
        g.feemanager.get_last_changed_at_gas
    );

    // Allow-list role gate: a non-enabled caller cannot set the config.
    let stranger = Address::from([0x99u8; 20]);
    let out = fm
        .run(
            &hex_bytes(&g.feemanager.set_fee_config_calldata),
            GAS,
            &ctx(stranger),
            &mut ops,
        )
        .expect("setFeeConfig stranger");
    assert_eq!(out.result, InstructionResult::PrecompileError);
    assert_eq!(out.gas.total_gas_spent(), GAS, "failure consumes all gas");
}

#[test]
fn rewardmanager_golden() {
    let g = golden();
    let caller = addr(&g.caller);
    assert_eq!(addr(&g.rewardmanager.address), REWARD_MANAGER_ADDRESS);

    let rm = RewardManagerPrecompile;
    let mut ops = MemStateOps::default();
    grant(&mut ops, REWARD_MANAGER_ADDRESS, caller, Role::Admin);
    let rask = b256(&g.rewardmanager.reward_address_storage_key);

    // Seed the OLD reward address the golden event encodes (0x3333…).
    let mut old_word = [0u8; 32];
    old_word[12..].copy_from_slice(&[0x33u8; 20]);
    ops.storage
        .insert((REWARD_MANAGER_ADDRESS, rask), B256::new(old_word));

    // setRewardAddress: storage + 4-topic event + gas.
    let out = rm
        .run(
            &hex_bytes(&g.rewardmanager.set_reward_address_calldata),
            GAS,
            &ctx(caller),
            &mut ops,
        )
        .expect("setRewardAddress");
    assert_eq!(out.result, InstructionResult::Return);
    assert_eq!(
        out.gas.total_gas_spent(),
        g.rewardmanager.set_reward_address_gas + g.rewardmanager.reward_address_changed_event_gas
    );
    assert_eq!(
        ops.storage[&(REWARD_MANAGER_ADDRESS, rask)],
        b256(&g.rewardmanager.stored_reward_value)
    );
    let (_, topics, data) = ops.logs.last().expect("RewardAddressChanged log").clone();
    let want: Vec<B256> = g
        .rewardmanager
        .reward_address_changed_topics
        .iter()
        .map(|t| b256(t))
        .collect();
    assert_eq!(topics, want);
    assert_eq!(
        data,
        hex_bytes(&g.rewardmanager.reward_address_changed_data)
    );

    // currentRewardAddress.
    let out = rm
        .run(&[0xe9, 0x15, 0x60, 0x8b], GAS, &ctx(caller), &mut ops)
        .expect("currentRewardAddress");
    assert_eq!(
        out.output.as_ref(),
        hex_bytes(&g.rewardmanager.current_reward_address_output)
    );
    assert_eq!(
        out.gas.total_gas_spent(),
        g.rewardmanager.current_reward_address_gas
    );

    // allowFeeRecipients: sentinel stored + 2-topic event; then the read
    // reports true.
    let out = rm
        .run(&[0x03, 0x29, 0x09, 0x9f], GAS, &ctx(caller), &mut ops)
        .expect("allowFeeRecipients");
    assert_eq!(out.result, InstructionResult::Return);
    assert_eq!(
        out.gas.total_gas_spent(),
        g.rewardmanager.allow_fee_recipients_gas + g.rewardmanager.fee_recipients_allowed_event_gas
    );
    assert_eq!(
        ops.storage[&(REWARD_MANAGER_ADDRESS, rask)],
        b256(&g.rewardmanager.allow_fee_recipients_value)
    );
    let (_, topics, _) = ops.logs.last().expect("FeeRecipientsAllowed log").clone();
    let want: Vec<B256> = g
        .rewardmanager
        .fee_recipients_allowed_topics
        .iter()
        .map(|t| b256(t))
        .collect();
    assert_eq!(topics, want);

    let out = rm
        .run(&[0xf6, 0x54, 0x2b, 0x2e], GAS, &ctx(caller), &mut ops)
        .expect("areFeeRecipientsAllowed");
    assert_eq!(
        out.output.as_ref(),
        hex_bytes(&g.rewardmanager.are_fee_recipients_allowed_output_true)
    );
    assert_eq!(
        out.gas.total_gas_spent(),
        g.rewardmanager.are_fee_recipients_allowed_gas
    );

    // disableRewards: blackhole stored + 2-topic event.
    let out = rm
        .run(&[0xbc, 0x17, 0x86, 0x28], GAS, &ctx(caller), &mut ops)
        .expect("disableRewards");
    assert_eq!(out.result, InstructionResult::Return);
    assert_eq!(
        out.gas.total_gas_spent(),
        g.rewardmanager.disable_rewards_gas + g.rewardmanager.rewards_disabled_event_gas
    );
    assert_eq!(
        ops.storage[&(REWARD_MANAGER_ADDRESS, rask)],
        b256(&g.rewardmanager.blackhole_value)
    );
    let (_, topics, _) = ops.logs.last().expect("RewardsDisabled log").clone();
    let want: Vec<B256> = g
        .rewardmanager
        .rewards_disabled_topics
        .iter()
        .map(|t| b256(t))
        .collect();
    assert_eq!(topics, want);

    // Role gate: stranger cannot set the reward address.
    let stranger = Address::from([0x99u8; 20]);
    let out = rm
        .run(
            &hex_bytes(&g.rewardmanager.set_reward_address_calldata),
            GAS,
            &ctx(stranger),
            &mut ops,
        )
        .expect("setRewardAddress stranger");
    assert_eq!(out.result, InstructionResult::PrecompileError);
}

#[test]
fn gaspricemanager_golden() {
    let g = golden();
    let caller = addr(&g.caller);
    assert_eq!(addr(&g.gaspricemanager.address), GAS_PRICE_MANAGER_ADDRESS);

    let gpm = GasPriceManagerPrecompile;
    let mut ops = MemStateOps::default();
    grant(&mut ops, GAS_PRICE_MANAGER_ADDRESS, caller, Role::Admin);

    // Seed the activation-time default config (module Configure stores it) —
    // the golden event's OLD config.
    let gp_key = ava_evm::precompile::gaspricemanager::gas_price_config_storage_key();
    ops.storage.insert(
        (GAS_PRICE_MANAGER_ADDRESS, gp_key),
        b256(&g.gaspricemanager.packed_default_word),
    );

    // setGasPriceConfig: packed storage word + lastChangedAt + event, all
    // byte-exact; one upfront gas charge (no Durango branch).
    let out = gpm
        .run(
            &hex_bytes(&g.gaspricemanager.set_gas_price_config_calldata),
            GAS,
            &ctx(caller),
            &mut ops,
        )
        .expect("setGasPriceConfig");
    assert_eq!(out.result, InstructionResult::Return);
    assert_eq!(
        out.gas.total_gas_spent(),
        ava_evm::precompile::gaspricemanager::SET_GAS_PRICE_CONFIG_GAS
    );
    assert_eq!(
        ops.storage[&(GAS_PRICE_MANAGER_ADDRESS, gp_key)],
        b256(&g.gaspricemanager.packed_config_word)
    );
    let lca_key = ava_evm::precompile::gaspricemanager::gas_price_config_last_changed_at_key();
    assert_eq!(
        ops.storage[&(GAS_PRICE_MANAGER_ADDRESS, lca_key)],
        b256(&g.gaspricemanager.get_last_changed_at_output_block7)
    );
    let (_, topics, data) = ops.logs.last().expect("GasPriceConfigUpdated log").clone();
    let want: Vec<B256> = g
        .gaspricemanager
        .event_topics
        .iter()
        .map(|t| b256(t))
        .collect();
    assert_eq!(topics, want);
    assert_eq!(data, hex_bytes(&g.gaspricemanager.event_data));

    // getGasPriceConfig: 5-word tuple output.
    let out = gpm
        .run(&[0x44, 0x58, 0x21, 0xe3], GAS, &ctx(caller), &mut ops)
        .expect("getGasPriceConfig");
    assert_eq!(
        out.output.as_ref(),
        hex_bytes(&g.gaspricemanager.get_gas_price_config_output)
    );

    // getGasPriceConfigLastChangedAt.
    let out = gpm
        .run(&[0xeb, 0x8d, 0xf3, 0x96], GAS, &ctx(caller), &mut ops)
        .expect("getGasPriceConfigLastChangedAt");
    assert_eq!(
        out.output.as_ref(),
        hex_bytes(&g.gaspricemanager.get_last_changed_at_output_block7)
    );

    // Role gate: stranger cannot set.
    let stranger = Address::from([0x99u8; 20]);
    let out = gpm
        .run(
            &hex_bytes(&g.gaspricemanager.set_gas_price_config_calldata),
            GAS,
            &ctx(stranger),
            &mut ops,
        )
        .expect("setGasPriceConfig stranger");
    assert_eq!(out.result, InstructionResult::PrecompileError);
}

#[test]
fn nativeminter_mint() {
    let caller = Address::from([0x11u8; 20]);
    let to = Address::from([0x22u8; 20]);
    let nm = NativeMinterPrecompile;
    let mut ops = MemStateOps::default();
    grant(&mut ops, NATIVE_MINTER_ADDRESS, caller, Role::Enabled);

    // mintNativeCoin(to, 5 wei).
    let mut input = vec![0x4f, 0x5a, 0xaa, 0xba];
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(to.as_slice());
    input.extend_from_slice(&w);
    let mut amt = [0u8; 32];
    amt[31] = 5;
    input.extend_from_slice(&amt);

    let out = nm
        .run(&input, GAS, &ctx(caller), &mut ops)
        .expect("mintNativeCoin");
    assert_eq!(out.result, InstructionResult::Return);
    assert_eq!(
        ops.balances[&to],
        U256::from(5u64),
        "minted balance credited"
    );
    let (log_addr, topics, _) = ops.logs.last().expect("NativeCoinMinted log").clone();
    assert_eq!(log_addr, NATIVE_MINTER_ADDRESS);
    assert_eq!(topics.len(), 3);

    // Role gate: stranger cannot mint.
    let stranger = Address::from([0x99u8; 20]);
    let out = nm
        .run(&input, GAS, &ctx(stranger), &mut ops)
        .expect("mint stranger");
    assert_eq!(out.result, InstructionResult::PrecompileError);
}
