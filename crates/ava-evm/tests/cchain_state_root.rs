// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `differential::cchain_state_root` — the M6.6 TDD entry point (spec 10
//! §3.2/§17.1/§17.4, 02 §10.5/§11.1).
//!
//! Recorded-oracle / reexecute mode: load the committed `genesis_to_1`
//! blockexport fixture (Go-EXECUTED against coreth — see
//! `tests/vectors/cchain/reexecute/genesis_to_1/manifest.json`), materialize the
//! genesis alloc into a fresh Firewood-ethhash db, verify the genesis state root
//! matches Go, decode block 1's EVM txs, drive `execute_batch`, convert the
//! returned `BundleState` into a Firewood proposal, and assert the post-state
//! root equals the Go-recorded value. One block proves the reth `BlockExecutor`
//! + Firewood-ethhash wiring (the cheapest differential oracle).

use std::str::FromStr;
use std::sync::Arc;

use ava_database::MemDb;
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::AvaEvmConfig;
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, B256, BundleState, Bytes, Decodable2718, ExternalConsensusExecutor, Header,
    SignerRecoverable, State, StateProviderDatabase, TransactionSigned, U256,
};

#[derive(serde::Deserialize)]
struct AllocEntry {
    address: String,
    balance: String,
}

#[derive(serde::Deserialize)]
struct Fixture {
    chain_id: u64,
    alloc: Vec<AllocEntry>,
    genesis_state_root: String,
    /// EIP-2718 typed-envelope encodings of block 1's txs. (We decode these
    /// directly rather than the full coreth block, whose header carries
    /// coreth-specific extra fields that alloy's standard `Header` decoder
    /// rejects — block-wire decode is M6.7 scope.)
    block1_txs: Vec<String>,
    block1_timestamp: u64,
    block1_base_fee: String,
    block1_gas_limit: u64,
    block1_coinbase: String,
    block1_parent_hash: String,
    block1_number: u64,
    #[serde(rename = "expected_post_state_root")]
    expected_post_state_root: String,
}

fn b256(s: &str) -> B256 {
    B256::from_str(s).expect("b256")
}

#[test]
fn cchain_state_root() {
    let raw = include_str!("vectors/cchain/reexecute/genesis_to_1/genesis_to_1.json");
    let fx: Fixture = serde_json::from_str(raw).expect("parse fixture");

    // --- Materialize the genesis alloc into a fresh Firewood-ethhash db. ---
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");

    // Build a genesis bundle of funded EOAs and commit it through the provider's
    // propose -> stash -> commit lifecycle (the same path accept() uses).
    let mut builder = BundleState::builder(0..=0);
    for entry in &fx.alloc {
        let addr = Address::from_str(&entry.address).expect("alloc addr");
        let balance = U256::from_str_radix(&entry.balance, 10).expect("alloc balance");
        builder = builder.state_present_account_info(
            addr,
            ava_evm_reth::AccountInfo {
                balance,
                nonce: 0,
                ..Default::default()
            },
        );
    }
    let genesis_bundle = builder.build();
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");

    // Verify the genesis state root matches the Go-recorded value.
    assert_eq!(
        provider.root(),
        b256(&fx.genesis_state_root),
        "genesis state root parity vs coreth"
    );

    // --- Decode block 1's txs (EIP-2718) + recover senders. ---
    let txs: Vec<_> = fx
        .block1_txs
        .iter()
        .map(|hex_tx| {
            let bytes = hex::decode(hex_tx.trim_start_matches("0x")).expect("tx hex");
            let signed =
                TransactionSigned::decode_2718(&mut bytes.as_slice()).expect("decode tx 2718");
            signed.try_into_recovered().expect("recover sender")
        })
        .collect();
    assert_eq!(txs.len(), 1, "fixture is a single-transfer block");

    // The env header: built from the recorded block-1 header fields (the
    // Go-computed basefee / gas_limit / timestamp / coinbase). On the reexecute
    // path the env is taken straight from the header (fee derivation is M6.13).
    let header = Header {
        parent_hash: b256(&fx.block1_parent_hash),
        number: fx.block1_number,
        timestamp: fx.block1_timestamp,
        gas_limit: fx.block1_gas_limit,
        base_fee_per_gas: Some(fx.block1_base_fee.parse().expect("base fee")),
        beneficiary: Address::from_str(&fx.block1_coinbase).expect("coinbase"),
        extra_data: Bytes::new(),
        ..Default::default()
    };

    // --- Drive execute_batch over a State<FirewoodStateView> at genesis. ---
    // The fixture was produced by coreth's `TestApricotPhase3Config` (AP1..AP3
    // active from genesis, AP4+ inactive => London-era / revm LONDON). Mirror
    // that schedule so `EvmEnv::for_eth_block` resolves SpecId::LONDON at the
    // block-1 timestamp (later forks far in the future).
    const FAR_FUTURE: u64 = u64::MAX;
    let upgrades = NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: FAR_FUTURE,
        apricot_phase_5: FAR_FUTURE,
        apricot_phase_pre_6: FAR_FUTURE,
        apricot_phase_6: FAR_FUTURE,
        apricot_phase_post_6: FAR_FUTURE,
        banff: FAR_FUTURE,
        cortina: FAR_FUTURE,
        durango: FAR_FUTURE,
        etna: FAR_FUTURE,
        fortuna: FAR_FUTURE,
        granite: FAR_FUTURE,
    };
    let chain_spec =
        AvaChainSpec::from_parts(upgrades, ava_evm_reth::Chain::from_id(fx.chain_id), false);
    let config = AvaEvmConfig::new(chain_spec);

    let view = provider
        .history_by_state_root(genesis_root)
        .expect("genesis view");
    let mut state: State<StateProviderDatabase<_>> = ava_evm_reth::StateBuilder::new()
        .with_database(StateProviderDatabase::new(view))
        .with_bundle_update()
        .build();

    let env = config.evm_env_for_header(&header);
    let outcome = config
        .execute_batch(env, &mut state, &ava_evm::evmconfig::NoopPreHook, &txs)
        .expect("execute_batch");

    // --- Convert the bundle to a Firewood proposal and assert post-root. ---
    let post_root = provider
        .propose_from_bundle(&outcome.bundle)
        .expect("propose post-state");
    assert_eq!(
        post_root,
        b256(&fx.expected_post_state_root),
        "post-block-1 state root parity vs coreth"
    );
}
