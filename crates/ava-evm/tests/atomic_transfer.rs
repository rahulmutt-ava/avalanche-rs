// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `atomic_transfer` — the M6.15 TDD entry point for the atomic
//! `EVMStateTransfer` pre-hook (spec 10 §6.3/§17.4, G3).
//!
//! Drives an [`AtomicStateHook`] (an `ImportTx` crediting an EVM recipient + an
//! `ExportTx` debiting a funded EOA and bumping its nonce) through the SAME
//! [`ExternalConsensusExecutor::execute_batch`] path the EVM tx loop uses, then
//! converts the returned `BundleState` into a Firewood proposal and asserts the
//! credited/debited balances + the bumped nonce fold into that one proposal —
//! i.e. the atomic effects are part of the EVM post-state root, exactly as
//! coreth. No EVM txs are executed: this isolates the pre-hook state transfer.

use std::sync::Arc;

use ava_database::MemDb;
use ava_evm::atomic::hook::AtomicStateHook;
use ava_evm::atomic::tx::{
    AtomicTx, EvmInput, EvmOutput, UnsignedExportTx, UnsignedImportTx, X2C_RATE,
};
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::AvaEvmConfig;
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    AccountInfo, AccountReader, Address, BundleState, Chain, ExternalConsensusExecutor, Header,
    State, StateProviderDatabase, U256,
};
use ava_types::id::Id;

/// The EVM recipient credited by the import (0x11 × 20). Starts empty.
const IMPORT_TO: [u8; 20] = [0x11; 20];
/// The funded EOA debited by the export (0x22 × 20).
const EXPORT_FROM: [u8; 20] = [0x22; 20];
/// nAVAX credited on import.
const IMPORT_AMOUNT: u64 = 4_000_000;
/// nAVAX debited on export.
const EXPORT_AMOUNT: u64 = 1_000_000;
/// The export input's nonce (replay protection); the account nonce bumps to +1.
const EXPORT_NONCE: u64 = 7;
/// Genesis balance (wei) of the export EOA — enough to cover the debit.
const FROM_GENESIS_WEI: u128 = 10_000_000 * (X2C_RATE as u128);

fn far_future_upgrades() -> NetworkUpgrades {
    const FAR: u64 = u64::MAX;
    NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: FAR,
        apricot_phase_5: FAR,
        apricot_phase_pre_6: FAR,
        apricot_phase_6: FAR,
        apricot_phase_post_6: FAR,
        banff: FAR,
        cortina: FAR,
        durango: FAR,
        etna: FAR,
        fortuna: FAR,
        granite: FAR,
    }
}

#[test]
fn import_credits_export_debits_and_bumps_nonce() {
    // --- Materialize a genesis funding only the export EOA. ---
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");

    let from = Address::from(EXPORT_FROM);
    let to = Address::from(IMPORT_TO);

    let genesis_bundle = BundleState::builder(0..=0)
        .state_present_account_info(
            from,
            AccountInfo {
                balance: U256::from(FROM_GENESIS_WEI),
                nonce: 0,
                ..Default::default()
            },
        )
        .build();
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");

    // --- Build the atomic batch: one Import (credit `to`), one Export (debit
    // `from` + bump its nonce). ---
    let asset = Id::from([0xAA; 32]);
    let import = AtomicTx::Import(UnsignedImportTx {
        network_id: 1,
        blockchain_id: Id::from([0x01; 32]),
        source_chain: Id::from([0x02; 32]),
        imported_inputs: Vec::new(),
        outs: vec![EvmOutput {
            address: IMPORT_TO,
            amount: IMPORT_AMOUNT,
            asset_id: asset,
        }],
    });
    let export = AtomicTx::Export(UnsignedExportTx {
        network_id: 1,
        blockchain_id: Id::from([0x01; 32]),
        destination_chain: Id::from([0x03; 32]),
        ins: vec![EvmInput {
            address: EXPORT_FROM,
            amount: EXPORT_AMOUNT,
            asset_id: asset,
            nonce: EXPORT_NONCE,
        }],
        exported_outputs: Vec::new(),
    });
    let hook = AtomicStateHook::new(vec![import, export]);

    // Atomic gas wiring (M6.13 reuse): the batch charges non-zero gas, and the
    // fee at a base fee is gas*base_fee (checked).
    let gas = hook.batch_gas(&[100, 120]).expect("batch gas");
    assert!(gas > 0, "atomic batch must charge gas");
    let fee = hook
        .batch_fee(&[100, 120], U256::from(25_000_000_000u64))
        .expect("batch fee");
    assert_eq!(fee, U256::from(gas) * U256::from(25_000_000_000u64));

    // --- Run the hook through execute_batch with NO EVM txs. ---
    let chain_spec = AvaChainSpec::from_parts(far_future_upgrades(), Chain::from_id(43114), false);
    let config = AvaEvmConfig::new(chain_spec);

    let header = Header {
        number: 1,
        timestamp: 1,
        gas_limit: 8_000_000,
        base_fee_per_gas: Some(25_000_000_000),
        ..Default::default()
    };

    let view = provider
        .history_by_state_root(genesis_root)
        .expect("genesis view");
    let mut state: State<StateProviderDatabase<_>> = ava_evm_reth::StateBuilder::new()
        .with_database(StateProviderDatabase::new(view))
        .with_bundle_update()
        .build();

    let env = config.evm_env_for_header(&header);
    let outcome = config
        .execute_batch(env, &mut state, &hook, &[])
        .expect("execute_batch with atomic hook");

    // --- Fold the bundle into a Firewood proposal; read back the post-state. ---
    let post_root = provider
        .propose_from_bundle(&outcome.bundle)
        .expect("propose post-state");
    provider.commit(post_root).expect("commit post-state");

    let post_view = provider
        .history_by_state_root(post_root)
        .expect("post view");

    // Import credits `amount * X2C_RATE` wei to the recipient.
    let to_acct = post_view
        .basic_account(&to)
        .expect("read to")
        .expect("recipient credited");
    let expected_credit = U256::from(IMPORT_AMOUNT) * U256::from(X2C_RATE);
    assert_eq!(to_acct.balance, expected_credit, "import credit");
    assert_eq!(to_acct.nonce, 0, "import does not touch nonce");

    // Export debits `amount * X2C_RATE` wei and sets nonce = max(cur, nonce+1).
    let from_acct = post_view
        .basic_account(&from)
        .expect("read from")
        .expect("export EOA present");
    let expected_debit = U256::from(EXPORT_AMOUNT) * U256::from(X2C_RATE);
    assert_eq!(
        from_acct.balance,
        U256::from(FROM_GENESIS_WEI) - expected_debit,
        "export debit"
    );
    assert_eq!(
        from_acct.nonce,
        EXPORT_NONCE + 1,
        "export bumps nonce to nonce+1"
    );
}
