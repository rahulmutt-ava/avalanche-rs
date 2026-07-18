// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! PREVRANDAO exec-env parity (coreth `core/evm.go:86-95`): at Durango+
//! (Shanghai) the EVM must run with Random = header difficulty and
//! difficulty = 0. A contract executing opcode 0x44 on a Go-shaped block
//! (difficulty == 1) must therefore read 1, not the mix_hash (0).

use std::str::FromStr;
use std::sync::Arc;

use ava_crypto::secp256k1::PrivateKey;
use ava_database::MemDb;
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, NoopPreHook};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    AccountInfo, Address, B256, BundleState, Bytecode, Bytes, Chain, EvmSignature,
    ExternalConsensusExecutor, Header, SignableTransaction, SignerRecoverable, State, StateBuilder,
    StateProviderDatabase, StorageKeyMap, TransactionSigned, TxKind, TxLegacy, U256, keccak256,
};

// Contract runtime code: 0x44 (PREVRANDAO) 0x5F (PUSH0) 0x55 (SSTORE) 0x00 (STOP)
const PREVRANDAO_PROBE_CODE: &[u8] = &[0x44, 0x5f, 0x55, 0x00];

const CHAIN_ID: u64 = 43_112;
const BASE_FEE: u128 = 1_000_000_000;
const FUNDED: u128 = 1_000_000_000_000_000_000;

fn funded_key() -> PrivateKey {
    PrivateKey::from_bytes(&[0x99u8; 32]).expect("key")
}

fn funded_address() -> Address {
    Address::from(funded_key().public_key().eth_address())
}

fn probe_contract() -> Address {
    Address::from_str("0x3333333333333333333333333333333333333333").expect("addr")
}

/// Signs a legacy tx with the funded key (EIP-155 over `CHAIN_ID`).
fn sign_legacy(tx: TxLegacy) -> TransactionSigned {
    let sig_hash = tx.signature_hash();
    let rsv = funded_key().sign_hash(&sig_hash.0).expect("sign");
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    TransactionSigned::Legacy(tx.into_signed(sig))
}

#[test]
fn prevrandao_reads_difficulty_at_durango() {
    // ---- Genesis: the probe contract + a funded EOA in a fresh
    // Firewood-ethhash db (same scaffolding as `cchain_state_root.rs` /
    // `rpc_eth.rs`). --------------------------------------------------------
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let provider = FirewoodStateProvider::open(dir.path(), Arc::clone(&bytecode), block_hashes)
        .expect("open firewood");

    let code_hash = keccak256(PREVRANDAO_PROBE_CODE);
    bytecode
        .put(code_hash.as_slice(), PREVRANDAO_PROBE_CODE)
        .expect("put code");

    let genesis_bundle = BundleState::builder(0..=0)
        .state_present_account_info(
            funded_address(),
            AccountInfo {
                balance: U256::from(FUNDED),
                nonce: 0,
                ..Default::default()
            },
        )
        .state_present_account_info(
            probe_contract(),
            AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash,
                code: Some(Bytecode::new_raw(PREVRANDAO_PROBE_CODE.to_vec().into())),
                ..Default::default()
            },
        )
        // Seed slot 0 with a sentinel unequal to both the buggy (0) and the
        // correct (1) PREVRANDAO value, so the probe tx's SSTORE always
        // produces an observable storage diff in the post-exec bundle
        // (an SSTORE that writes back the pre-existing default `0` would
        // otherwise vanish from the bundle as a no-op change).
        .state_storage(probe_contract(), {
            let mut storage: StorageKeyMap<(U256, U256)> = StorageKeyMap::default();
            storage.insert(U256::ZERO, (U256::from(0xdead_u64), U256::from(0xdead_u64)));
            storage
        })
        .build();
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");

    // ---- Chain spec: AP1-3 + Durango active from genesis (the block header
    // below is timestamped well after 0, i.e. Durango+ for this spec). ------
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
        durango: 0,
        etna: FAR_FUTURE,
        fortuna: FAR_FUTURE,
        granite: FAR_FUTURE,
        helicon: u64::MAX,
    };
    let chain_spec = AvaChainSpec::from_parts(upgrades, Chain::from_id(CHAIN_ID), false);
    let config = AvaEvmConfig::new(chain_spec);

    // ---- Block 1: a Go-shaped header (difficulty == 1, mix_hash == 0) at a
    // Durango+ timestamp, with one tx calling the probe contract. -----------
    let tx = sign_legacy(TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce: 0,
        gas_price: BASE_FEE,
        gas_limit: 100_000,
        to: TxKind::Call(probe_contract()),
        value: U256::ZERO,
        input: Bytes::new(),
    });
    let recovered = tx.try_into_recovered().expect("recover sender");

    let header = Header {
        number: 1,
        timestamp: 100,
        // coreth's Go header: every block carries difficulty 1, mixDigest 0
        // (`core/evm.go:86-95` reads Random FROM this difficulty).
        difficulty: U256::from(1),
        mix_hash: B256::ZERO,
        gas_limit: 8_000_000,
        base_fee_per_gas: Some(BASE_FEE as u64),
        beneficiary: Address::ZERO,
        extra_data: Bytes::new(),
        ..Default::default()
    };

    let view = provider
        .history_by_state_root(genesis_root)
        .expect("genesis view");
    let mut state: State<StateProviderDatabase<_>> = StateBuilder::new()
        .with_database(StateProviderDatabase::new(view))
        .with_bundle_update()
        .build();

    // Drive `evm_env_for_header` + `execute_batch` exactly as `EvmBlock::verify`
    // does (`block.rs:743-746`).
    let env = config.evm_env_for_header(&header);
    let outcome = config
        .execute_batch(env, &mut state, &NoopPreHook, &[recovered])
        .expect("execute_batch");

    assert!(
        outcome.result.receipts[0].success,
        "prevrandao probe tx must succeed"
    );

    // ---- Assert slot 0 of the probe contract == 1 (the header difficulty),
    // NOT 0 (the mix_hash) — coreth `core/evm.go:86-95` Random=difficulty
    // rule at Shanghai (== Durango). ------------------------------------------
    let account = outcome
        .bundle
        .account(&probe_contract())
        .expect("contract touched");
    let slot0 = account
        .storage
        .get(&U256::ZERO)
        .expect("slot 0 written")
        .present_value;
    assert_eq!(
        slot0,
        U256::from(1),
        "PREVRANDAO must read the header difficulty (coreth Random rule), not mix_hash"
    );
}
