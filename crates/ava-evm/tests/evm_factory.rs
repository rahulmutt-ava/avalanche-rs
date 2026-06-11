// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The M6.31 named test: a block executed through the LIVE
//! `ConfigureEvm`/`EvmFactory` path (`AvaEvmConfig::execute_batch_with_ctx` →
//! `AvaEvmFactory` → `AvaEvm` + `AvaHandler`), asserting that
//!
//! 1. the Avalanche stateful precompiles are installed in the produced EVM:
//!    `sendWarpMessage` dispatches end-to-end (receipt log + `take_logs` →
//!    `handle_precompile_accept` → `WarpBackend`), and `getVerifiedWarpMessage`
//!    reads THIS tx's predicate results (tx-index-keyed gas discriminates a
//!    broken index from a working one);
//! 2. the base fee is credited to the coinbase (`AvaHandler` — coreth
//!    `state_transition.go` `fee := gasUsed * msg.GasPrice`), NOT burned
//!    (revm's London default), and the sender pays the full effective price.

use std::str::FromStr;
use std::sync::Arc;

use ava_crypto::secp256k1::PrivateKey;
use ava_database::MemDb;
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, AvaExecCtx, NoopPreHook};
use ava_evm::precompile::registry::{PrecompileModule, PrecompileRegistry, PredicateResults};
use ava_evm::precompile::warp::{
    PRE_GRANITE_GAS_CONFIG, WARP_PRECOMPILE_ADDRESS, WarpBackend, WarpPrecompile,
    handle_precompile_accept, predicate_to_chunks,
};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, B256, BundleState, Bytes, EvmSignature, Header, SignableTransaction,
    SignerRecoverable, State, StateProviderDatabase, TransactionSigned, TxKind, TxLegacy, U256,
};
use ava_types::id::Id;
use ava_warp::payload::{AddressedCall, WarpPayload};
use ava_warp::{BitSetSignature, Message, Signature, UnsignedMessage};

const CHAIN_ID: u64 = 43_112;
const NETWORK_ID: u32 = 12_345;
const BASE_FEE: u128 = 25_000_000_000;
const FUNDED: u128 = 1_000_000_000_000_000_000;

fn this_chain_id() -> Id {
    Id::from([0x0Cu8; 32])
}

fn funded_key() -> PrivateKey {
    PrivateKey::from_bytes(&[0x11u8; 32]).expect("key")
}

fn funded_address() -> Address {
    Address::from(funded_key().public_key().eth_address())
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

/// `abi.encode(bytes payload)` for the single-`bytes` selector args.
fn abi_encode_bytes(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut off = [0u8; 32];
    off[31] = 0x20;
    out.extend_from_slice(&off);
    let mut len = [0u8; 32];
    len[24..].copy_from_slice(&(payload.len() as u64).to_be_bytes());
    out.extend_from_slice(&len);
    out.extend_from_slice(payload);
    let rem = payload.len() % 32;
    if rem != 0 {
        out.extend(std::iter::repeat_n(0u8, 32 - rem));
    }
    out
}

/// A structurally-valid signed warp message over an `AddressedCall` (the BLS
/// aggregate is junk — the predicate RESULT is injected as pre-verified, the
/// precompile only re-parses the bytes).
fn warp_predicate_bytes() -> Vec<u8> {
    let call = AddressedCall {
        source_address: vec![0xAB; 20],
        payload: b"m631-predicate".to_vec(),
    };
    let unsigned = UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: Id::from([0x5Au8; 32]),
        payload: WarpPayload::AddressedCall(call).marshal_payload().expect("payload"),
    };
    let msg = Message {
        unsigned_message: unsigned,
        signature: Signature::BitSet(BitSetSignature {
            signers: vec![0x01],
            signature: [0u8; 96],
        }),
    };
    msg.marshal().expect("marshal")
}

/// London-era calldata gas: 16 per nonzero byte, 4 per zero byte.
fn calldata_gas(data: &[u8]) -> u64 {
    data.iter()
        .map(|&b| if b == 0 { 4u64 } else { 16 })
        .sum()
}

#[test]
fn evm_factory_live_path() {
    // ---- Genesis: one funded EOA in a fresh Firewood-ethhash db. -----------
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");
    let genesis_bundle = BundleState::builder(0..=0)
        .state_present_account_info(
            funded_address(),
            ava_evm_reth::AccountInfo {
                balance: U256::from(FUNDED),
                nonce: 0,
                ..Default::default()
            },
        )
        .build();
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");

    // ---- The LIVE config: warp registered in the precompile registry. ------
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
        AvaChainSpec::from_parts(upgrades, ava_evm_reth::Chain::from_id(CHAIN_ID), false);

    let warp = Arc::new(WarpPrecompile::new(this_chain_id(), NETWORK_ID, false));
    let mut registry = PrecompileRegistry::new();
    registry.register(PrecompileModule {
        address: WARP_PRECOMPILE_ADDRESS,
        activation: 0,
        precompile: warp.clone(),
    });
    let config = AvaEvmConfig::new(chain_spec).with_precompiles(Arc::new(registry));

    // ---- Block: tx0 sendWarpMessage, tx1 getVerifiedWarpMessage(0). --------
    let coinbase = Address::from([0x44u8; 20]);
    let send_payload = b"m631-out".to_vec();
    let mut send_input = vec![0xee, 0x5b, 0x48, 0xeb];
    send_input.extend_from_slice(&abi_encode_bytes(&send_payload));
    let tx0 = sign_legacy(TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce: 0,
        gas_price: BASE_FEE,
        gas_limit: 1_000_000,
        to: TxKind::Call(WARP_PRECOMPILE_ADDRESS),
        value: U256::ZERO,
        input: Bytes::from(send_input),
    });

    let mut get_input = vec![0x6f, 0x82, 0x53, 0x50];
    get_input.extend_from_slice(&[0u8; 32]); // index 0
    let tx1 = sign_legacy(TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce: 1,
        gas_price: BASE_FEE,
        gas_limit: 1_000_000,
        to: TxKind::Call(WARP_PRECOMPILE_ADDRESS),
        value: U256::ZERO,
        input: Bytes::from(get_input.clone()),
    });

    let txs: Vec<_> = [tx0, tx1]
        .into_iter()
        .map(|t| t.try_into_recovered().expect("recover"))
        .collect();

    // tx index 1 carries one pre-verified warp predicate (the predicate pass
    // result, spec 20 §7.2) — the live factory must thread it into THAT tx's
    // precompile reads only.
    let pred_chunks = predicate_to_chunks(&warp_predicate_bytes());
    let mut predicates = PredicateResults::default();
    predicates.set_warp(1, vec![pred_chunks.clone()], vec![true]);
    let exec_ctx = AvaExecCtx {
        predicates: Arc::new(predicates),
        pchain_height: 42,
    };

    let header = Header {
        number: 1,
        timestamp: 10,
        gas_limit: 8_000_000,
        base_fee_per_gas: Some(BASE_FEE as u64),
        beneficiary: coinbase,
        extra_data: Bytes::new(),
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
        .execute_batch_with_ctx(env, &mut state, &NoopPreHook, &txs, &exec_ctx)
        .expect("execute_batch_with_ctx");

    // ---- (1a) warp dispatched end-to-end: both receipts succeed; the send
    // emitted the SendWarpMessage log into the RECEIPT (journal write-through).
    let receipts = &outcome.result.receipts;
    assert_eq!(receipts.len(), 2);
    assert!(receipts[0].success, "sendWarpMessage tx must succeed");
    assert!(receipts[1].success, "getVerifiedWarpMessage tx must succeed");
    assert_eq!(
        receipts[0].logs.len(),
        1,
        "SendWarpMessage log lands in the receipt via the live journal"
    );
    assert_eq!(receipts[0].logs[0].address, WARP_PRECOMPILE_ADDRESS);

    // ---- (1b) tx-index-keyed predicate read: tx1's gas includes the per-chunk
    // read charge for ITS predicate — a broken tx-index threading reads no
    // predicate and charges only the base cost.
    let gas_tx0 = receipts[0].cumulative_gas_used;
    let gas_tx1 = receipts[1].cumulative_gas_used - gas_tx0;
    let n_chunks = (pred_chunks.len() / 32) as u64;
    let expected_tx1_gas = 21_000
        + calldata_gas(&get_input)
        + PRE_GRANITE_GAS_CONFIG.get_verified_warp_message_base
        + PRE_GRANITE_GAS_CONFIG.per_warp_message_chunk * n_chunks;
    assert_eq!(
        gas_tx1, expected_tx1_gas,
        "getVerifiedWarpMessage gas = intrinsic + base + per-chunk over THIS tx's predicate"
    );

    // ---- (2) base-fee-to-coinbase (AvaHandler): coinbase += gas_used·price,
    // sender -= the same (no value transferred); nothing burned.
    let total_gas = outcome.result.gas_used;
    assert_eq!(
        receipts[1].cumulative_gas_used, total_gas,
        "receipt cumulative gas folds both txs"
    );
    let fee = U256::from(BASE_FEE) * U256::from(total_gas);
    let coinbase_acc = outcome.bundle.account(&coinbase).expect("coinbase touched");
    assert_eq!(
        coinbase_acc.info.as_ref().expect("coinbase info").balance,
        fee,
        "coinbase credited the FULL effective fee (base fee NOT burned)"
    );
    let sender_acc = outcome.bundle.account(&funded_address()).expect("sender");
    assert_eq!(
        sender_acc.info.as_ref().expect("sender info").balance,
        U256::from(FUNDED) - fee,
        "sender pays the full effective price"
    );

    // ---- (1c) accept routing: take_logs → handle_precompile_accept records
    // the unsigned message in the WarpBackend.
    let logs = warp.take_logs();
    assert_eq!(logs.len(), 1, "one SendWarpMessage buffered for accept");
    let backend = WarpBackend::new();
    handle_precompile_accept(&backend, &logs).expect("accept routing");
    assert_eq!(backend.len(), 1, "accepted message recorded for signing");

    // The post root differs from genesis and is proposable (sanity).
    let post_root = provider
        .propose_from_bundle(&outcome.bundle)
        .expect("propose post");
    assert_ne!(post_root, genesis_root);
    let _ = B256::from_str; // (used by sibling tests' helpers; keep imports tidy)
}
