// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `rpc_eth` — golden request→response tests for the `eth_*` RPC handlers over
//! Firewood + the `feerules` fee/accepted-tag overrides (G8, spec 10 §9.1/§17.9,
//! M6.23).
//!
//! The handlers are plain Rust functions over `Arc<FirewoodStateProvider>` (state
//! reads), `CanonicalStore` (accepted-block tag mapping), `feerules`
//! (gasPrice/feeHistory/maxPriorityFeePerGas) and the facade revm executor
//! (`eth_call`/`eth_estimateGas`) — NOT reth's `EthApi` stack (see the module +
//! report scoping note). Each method returns a `serde_json::Value` that matches
//! the Ethereum JSON-RPC `0x`-quantity / data conventions coreth's `eth/` server
//! emits. Golden request/response vectors + provenance live under
//! `tests/vectors/cchain/rpc/`.

use std::str::FromStr;
use std::sync::Arc;

use ava_database::MemDb;
use ava_evm::chainspec::AvaChainSpec;
use ava_evm::evmconfig::AvaEvmConfig;
use ava_evm::rpc::eth::{BlockTag, CallRequest, EthRpc, FeeHistoryArgs};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{Address, B256, BundleState, Bytes, Chain, KECCAK_EMPTY, U256, keccak256};
use serde_json::{Value, json};

// ─── Fixtures ────────────────────────────────────────────────────────────────

/// A funded EOA used across the read tests: 1 ether, nonce 7.
fn alice() -> Address {
    Address::from_str("0x1111111111111111111111111111111111111111").expect("addr")
}

/// A simple contract whose runtime code returns the 32-byte word 0x2a (= 42):
/// `PUSH1 0x2a PUSH1 0x00 MSTORE PUSH1 0x20 PUSH1 0x00 RETURN`.
const RETURN_42_RUNTIME: &[u8] = &[0x60, 0x2a, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3];

fn contract() -> Address {
    Address::from_str("0x2222222222222222222222222222222222222222").expect("addr")
}

/// Builds a Firewood provider seeded with: alice (1 ether, nonce 7) and a
/// `return 42` contract, then advances a [`CanonicalStore`] to height 5.
fn setup() -> (tempfile::TempDir, EthRpc) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let provider = FirewoodStateProvider::open(dir.path(), Arc::clone(&bytecode), block_hashes)
        .expect("open firewood");

    // Seed the contract bytecode into the side store (code_hash -> bytecode).
    let code_hash = keccak256(RETURN_42_RUNTIME);
    bytecode
        .put(code_hash.as_slice(), RETURN_42_RUNTIME)
        .expect("put code");

    // Commit alice + the contract account through propose -> stash -> commit.
    let mut builder = BundleState::builder(0..=0);
    builder = builder.state_present_account_info(
        alice(),
        ava_evm_reth::AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000_u128), // 1 ether
            nonce: 7,
            ..Default::default()
        },
    );
    builder = builder.state_present_account_info(
        contract(),
        ava_evm_reth::AccountInfo {
            balance: U256::ZERO,
            nonce: 1,
            code_hash,
            code: Some(ava_evm_reth::Bytecode::new_raw(
                RETURN_42_RUNTIME.to_vec().into(),
            )),
            ..Default::default()
        },
    );
    let bundle = builder.build();
    let root = provider.propose_from_bundle(&bundle).expect("propose");
    provider.commit(root).expect("commit");

    // A canonical store advanced to height 5 (the "last accepted" head).
    let canon_db: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let canonical = Arc::new(ava_evm::canonical::CanonicalStore::new(canon_db));
    for n in 1..=5u64 {
        canonical
            .append_canonical(
                n,
                B256::repeat_byte(n as u8),
                B256::repeat_byte(0xa0 + n as u8),
                &[],
                &[],
            )
            .expect("append");
    }

    // C-Chain mainnet spec (chain id 43114), Etna-era timestamps so the fee
    // regime is the AP3 rolling window (deterministic golden fees).
    let spec = AvaChainSpec::c_chain(1, Chain::from_id(43114));
    let config = AvaEvmConfig::new(spec);

    let rpc = EthRpc::new(provider, canonical, config, 43114);
    (dir, rpc)
}

fn vector(name: &str) -> Value {
    let raw = std::fs::read_to_string(format!(
        "{}/tests/vectors/cchain/rpc/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("read vector");
    serde_json::from_str(&raw).expect("parse vector")
}

// ─── eth_chainId / eth_blockNumber + accepted-block tags ─────────────────────

#[test]
fn eth_chain_id_matches_golden() {
    let (_d, rpc) = setup();
    let want = vector("eth_chainId");
    assert_eq!(rpc.chain_id(), want["result"]);
}

#[test]
fn eth_block_number_is_last_accepted() {
    let (_d, rpc) = setup();
    let want = vector("eth_blockNumber");
    assert_eq!(rpc.block_number().expect("block_number"), want["result"]);
}

#[test]
fn accepted_tags_all_map_to_last_accepted_height() {
    // latest / safe / finalized all resolve to the last-accepted height (5);
    // Snowman has no pending/unsafe head (spec 10 §17.9, coreth rpc_accepted).
    let (_d, rpc) = setup();
    for tag in [BlockTag::Latest, BlockTag::Safe, BlockTag::Finalized] {
        assert_eq!(rpc.resolve_tag(tag).expect("resolve"), Some(5));
    }
    assert_eq!(
        rpc.resolve_tag(BlockTag::Earliest).expect("resolve"),
        Some(0)
    );
    assert_eq!(
        rpc.resolve_tag(BlockTag::Number(3)).expect("resolve"),
        Some(3)
    );
}

// ─── eth_getBalance / nonce / code ───────────────────────────────────────────

#[test]
fn eth_get_balance_matches_golden() {
    let (_d, rpc) = setup();
    let want = vector("eth_getBalance");
    let got = rpc.get_balance(alice(), BlockTag::Latest).expect("balance");
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_get_transaction_count_matches_golden() {
    let (_d, rpc) = setup();
    let want = vector("eth_getTransactionCount");
    let got = rpc
        .get_transaction_count(alice(), BlockTag::Latest)
        .expect("nonce");
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_get_code_matches_golden() {
    let (_d, rpc) = setup();
    let want = vector("eth_getCode");
    let got = rpc.get_code(contract(), BlockTag::Latest).expect("code");
    assert_eq!(got, want["result"]);
}

// ─── eth_call ────────────────────────────────────────────────────────────────

#[test]
fn eth_call_returns_contract_output() {
    let (_d, rpc) = setup();
    let want = vector("eth_call");
    let req = CallRequest {
        from: Some(alice()),
        to: Some(contract()),
        gas: Some(100_000),
        value: None,
        data: None,
    };
    let got = rpc.call(req, BlockTag::Latest).expect("call");
    // The contract returns the 32-byte word 0x2a (= 42).
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_estimate_gas_for_contract_call() {
    let (_d, rpc) = setup();
    let want = vector("eth_estimateGas");
    let req = CallRequest {
        from: Some(alice()),
        to: Some(contract()),
        gas: Some(1_000_000),
        value: None,
        data: None,
    };
    let got = rpc.estimate_gas(req, BlockTag::Latest).expect("estimate");
    assert_eq!(got, want["result"]);
}

// ─── eth_getProof (account fields today; proof array pending M6.25) ───────────

#[test]
fn eth_get_proof_account_fields_match_golden() {
    let (_d, rpc) = setup();
    let want = vector("eth_getProof");
    let got = rpc
        .get_proof(alice(), &[], BlockTag::Latest)
        .expect("proof");
    // Account fields come from direct Firewood reads (work today).
    assert_eq!(got["address"], want["result"]["address"]);
    assert_eq!(got["balance"], want["result"]["balance"]);
    assert_eq!(got["nonce"], want["result"]["nonce"]);
    assert_eq!(got["codeHash"], want["result"]["codeHash"]);
    assert_eq!(got["storageHash"], want["result"]["storageHash"]);
    // The merkle proof array is EMPTY until M6.25 wires Firewood proofs into
    // StateProofProvider::proof (documented in code + golden + report).
    assert_eq!(got["accountProof"], json!([]));
    assert_eq!(got["accountProof"], want["result"]["accountProof"]);
}

// ─── fee helpers via feerules ────────────────────────────────────────────────

#[test]
fn eth_gas_price_uses_feerules() {
    let (_d, rpc) = setup();
    let want = vector("eth_gasPrice");
    let got = rpc.gas_price().expect("gas_price");
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_max_priority_fee_per_gas_is_zero_tip() {
    let (_d, rpc) = setup();
    let want = vector("eth_maxPriorityFeePerGas");
    let got = rpc.max_priority_fee_per_gas().expect("tip");
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_fee_history_shape_matches_golden() {
    let (_d, rpc) = setup();
    let want = vector("eth_feeHistory");
    let args = FeeHistoryArgs {
        block_count: 2,
        newest_block: BlockTag::Latest,
        reward_percentiles: vec![],
    };
    let got = rpc.fee_history(args).expect("fee_history");
    assert_eq!(got["oldestBlock"], want["result"]["oldestBlock"]);
    assert_eq!(got["baseFeePerGas"], want["result"]["baseFeePerGas"]);
    assert_eq!(got["gasUsedRatio"], want["result"]["gasUsedRatio"]);
}

// ─── debug_traceTransaction (deferred) ───────────────────────────────────────

#[test]
fn debug_trace_transaction_is_deferred() {
    let (_d, rpc) = setup();
    // The prestate tracer needs a revm inspector that is not reachable behind the
    // facade without a heavy dep (M6.23 scoping note); the handler returns a
    // documented error until a follow-up wires it.
    let err = rpc
        .debug_trace_transaction(B256::repeat_byte(0xab))
        .unwrap_err();
    assert!(err.to_string().contains("debug_traceTransaction"));
}

// ─── self-consistency: empty-code / empty-storage sentinels ──────────────────

#[test]
fn empty_account_uses_canonical_sentinels() {
    let (_d, rpc) = setup();
    // An absent account reports zero balance/nonce and the empty sentinels.
    let absent = Address::repeat_byte(0x99);
    let proof = rpc.get_proof(absent, &[], BlockTag::Latest).expect("proof");
    assert_eq!(proof["balance"], "0x0");
    assert_eq!(proof["nonce"], "0x0");
    assert_eq!(
        proof["codeHash"],
        format!("0x{}", hex::encode(KECCAK_EMPTY.as_slice()))
    );
    // Code of an EOA is "0x".
    assert_eq!(
        rpc.get_code(absent, BlockTag::Latest).expect("code"),
        Value::String("0x".to_string())
    );
    let _ = Bytes::new();
}
