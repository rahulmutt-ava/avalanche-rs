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

use ava_crypto::secp256k1::PrivateKey;
use ava_database::MemDb;
use ava_evm::chainspec::AvaChainSpec;
use ava_evm::evmconfig::AvaEvmConfig;
use ava_evm::mempool::EvmMempool;
use ava_evm::receipts::{AcceptedTxIndex, TxReceiptRecord};
use ava_evm::rpc::eth::{BlockTag, CallRequest, EthRpc, FeeHistoryArgs};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, B256, BundleState, Bytes, Chain, Decodable2718, Encodable2718, EvmSignature,
    KECCAK_EMPTY, Log, SignableTransaction, TransactionSigned, TxKind, TxLegacy, U256, keccak256,
};
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

/// The chain id [`setup`]'s [`EthRpc`] is configured with — also the EIP-155
/// chain id [`sign_legacy`] signs against, so a signed tx passes
/// [`EthRpc::send_raw_transaction`]'s chain-id admission check.
const CHAIN_ID: u64 = 43114;

/// The `eth_sendRawTransaction` test signer (repeat-don't-import convention:
/// the same private-key-from-repeated-byte pattern `evm_factory.rs`'s
/// `funded_key`/`funded_address` use), funded by [`setup`].
fn funded_key() -> PrivateKey {
    PrivateKey::from_bytes(&[0x11u8; 32]).expect("PrivateKey::from_bytes")
}

/// The funded signer's EVM address (`PublicKey::eth_address`).
fn funded_address() -> Address {
    Address::from(funded_key().public_key().eth_address())
}

/// Signs `tx` with [`funded_key`] as a legacy EIP-155 transaction and returns
/// its EIP-2718-encoded raw bytes (the `eth_sendRawTransaction` wire form).
fn sign_legacy(tx: TxLegacy) -> Bytes {
    let sig_hash = tx.signature_hash();
    let rsv = funded_key().sign_hash(&sig_hash.0).expect("sign_hash");
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    let signed = TransactionSigned::Legacy(tx.into_signed(sig));
    Bytes::from(signed.encoded_2718())
}

/// A funded-signer legacy tx: `nonce`, a 2 gwei gas price (well above the
/// pool's 1-wei tip floor), 21000 gas (a plain transfer), 1 wei value, and
/// `CHAIN_ID` (so it passes the EIP-155 chain-id check).
fn funded_legacy_tx(nonce: u64) -> Bytes {
    sign_legacy(TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce,
        gas_price: 2_000_000_000,
        gas_limit: 21_000,
        to: TxKind::Call(alice()),
        value: U256::from(1u64),
        input: Bytes::new(),
    })
}

/// Builds a Firewood provider seeded with: alice (1 ether, nonce 7), the
/// `eth_sendRawTransaction` funded signer (1 ether, nonce 0), and a `return
/// 42` contract, then advances a [`CanonicalStore`] to height 5. Returns the
/// [`EthRpc`] plus the same mempool/tx-index handles it was built over, so
/// tests can inspect/seed them directly.
fn setup() -> (
    tempfile::TempDir,
    EthRpc,
    Arc<parking_lot::Mutex<EvmMempool>>,
    Arc<AcceptedTxIndex>,
) {
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

    // Commit alice + the funded signer + the contract account through
    // propose -> stash -> commit.
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
        funded_address(),
        ava_evm_reth::AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000_u128), // 1 ether
            nonce: 0,
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
    let spec = AvaChainSpec::c_chain(1, Chain::from_id(CHAIN_ID));
    let config = AvaEvmConfig::new(spec);

    let mempool = Arc::new(parking_lot::Mutex::new(EvmMempool::new(16)));
    let tx_index = Arc::new(AcceptedTxIndex::new());
    let rpc = EthRpc::new(
        provider,
        canonical,
        config,
        CHAIN_ID,
        Arc::clone(&mempool),
        Arc::clone(&tx_index),
    );
    (dir, rpc, mempool, tx_index)
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
    let (_d, rpc, _mempool, _tx_index) = setup();
    let want = vector("eth_chainId");
    assert_eq!(rpc.chain_id(), want["result"]);
}

#[test]
fn eth_block_number_is_last_accepted() {
    let (_d, rpc, _mempool, _tx_index) = setup();
    let want = vector("eth_blockNumber");
    assert_eq!(rpc.block_number().expect("block_number"), want["result"]);
}

#[test]
fn accepted_tags_all_map_to_last_accepted_height() {
    // latest / safe / finalized all resolve to the last-accepted height (5);
    // Snowman has no pending/unsafe head (spec 10 §17.9, coreth rpc_accepted).
    let (_d, rpc, _mempool, _tx_index) = setup();
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
    let (_d, rpc, _mempool, _tx_index) = setup();
    let want = vector("eth_getBalance");
    let got = rpc.get_balance(alice(), BlockTag::Latest).expect("balance");
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_get_transaction_count_matches_golden() {
    let (_d, rpc, _mempool, _tx_index) = setup();
    let want = vector("eth_getTransactionCount");
    let got = rpc
        .get_transaction_count(alice(), BlockTag::Latest)
        .expect("nonce");
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_get_code_matches_golden() {
    let (_d, rpc, _mempool, _tx_index) = setup();
    let want = vector("eth_getCode");
    let got = rpc.get_code(contract(), BlockTag::Latest).expect("code");
    assert_eq!(got, want["result"]);
}

// ─── eth_call ────────────────────────────────────────────────────────────────

#[test]
fn eth_call_returns_contract_output() {
    let (_d, rpc, _mempool, _tx_index) = setup();
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
    let (_d, rpc, _mempool, _tx_index) = setup();
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
    let (_d, rpc, _mempool, _tx_index) = setup();
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
    let (_d, rpc, _mempool, _tx_index) = setup();
    let want = vector("eth_gasPrice");
    let got = rpc.gas_price().expect("gas_price");
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_max_priority_fee_per_gas_is_zero_tip() {
    let (_d, rpc, _mempool, _tx_index) = setup();
    let want = vector("eth_maxPriorityFeePerGas");
    let got = rpc.max_priority_fee_per_gas().expect("tip");
    assert_eq!(got, want["result"]);
}

#[test]
fn eth_fee_history_shape_matches_golden() {
    let (_d, rpc, _mempool, _tx_index) = setup();
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

// ─── eth_sendRawTransaction / eth_getTransactionReceipt ──────────────────────
// (cchain-tx-pipeline task 4, over Task 1's EvmMempool + Task 3's
// AcceptedTxIndex.)

#[test]
fn send_raw_transaction_admits_and_returns_hash() {
    let (_d, rpc, mempool, _tx_index) = setup();
    let raw = funded_legacy_tx(0);
    // The tx's own hash, independent of the RPC path, for the "result == tx
    // hash" assertion below (`TransactionSigned::decode_2718` + `.tx_hash()`
    // would need SignerRecoverable in scope here; simplest is decoding the
    // same envelope through the handler and comparing to a fresh decode).
    let decoded =
        TransactionSigned::decode_2718(&mut raw.as_ref()).expect("decode_2718 (test oracle)");
    let want_hash = *decoded.tx_hash();

    let got = rpc
        .send_raw_transaction(&raw)
        .expect("send_raw_transaction");
    assert_eq!(
        got,
        Value::String(format!("0x{}", hex::encode(want_hash.as_slice()))),
        "eth_sendRawTransaction returns the tx hash"
    );
    assert!(
        mempool.lock().contains(&want_hash),
        "the admitted tx must be pooled"
    );
}

#[test]
fn send_raw_transaction_maps_admission_errors() {
    let (_d, rpc, _mempool, _tx_index) = setup();

    // Admitting the SAME tx twice: the second reply is a JSON-RPC-mapped
    // error whose message contains coreth's "already known" sentinel
    // verbatim (EvmMempoolError::AlreadyKnown, surfaced through
    // Error::Mempool's #[from]/transparent Display).
    let raw = funded_legacy_tx(0);
    rpc.send_raw_transaction(&raw).expect("first admission");
    let err = rpc.send_raw_transaction(&raw).unwrap_err();
    assert!(err.to_string().contains("already known"), "got: {err}");

    // A nonce-5 tx from a nonce-0 account: this pool rejects gapped nonces
    // (documented divergence, EvmMempoolError::NonceGap) with a message
    // containing "nonce gap".
    let gapped = funded_legacy_tx(5);
    let err = rpc.send_raw_transaction(&gapped).unwrap_err();
    assert!(err.to_string().contains("nonce gap"), "got: {err}");
}

#[test]
fn get_transaction_receipt_null_when_unknown_then_served_after_accept() {
    let (_d, rpc, _mempool, tx_index) = setup();

    // Unknown hash -> null result (geth returns null, not an error).
    let unknown = B256::repeat_byte(0x77);
    assert_eq!(
        rpc.get_transaction_receipt(unknown).expect("receipt"),
        Value::Null,
        "an unknown tx hash must return a null result, not an error"
    );

    // Seed the AcceptedTxIndex directly with a full TxReceiptRecord (a
    // contract-creation success, 2 logs) and assert the full JSON shape.
    let tx_hash = B256::repeat_byte(0x42);
    let block_hash = B256::repeat_byte(0x05);
    let contract_address = Address::repeat_byte(0x09);
    let from = funded_address();
    let log0 = Log::new_unchecked(
        Address::repeat_byte(0x11),
        vec![B256::repeat_byte(0xaa)],
        Bytes::from_static(b"one"),
    );
    let log1 = Log::new_unchecked(
        Address::repeat_byte(0x22),
        vec![B256::repeat_byte(0xbb), B256::repeat_byte(0xcc)],
        Bytes::from_static(b"two"),
    );
    tx_index.record(vec![TxReceiptRecord {
        tx_hash,
        block_hash,
        block_number: 5,
        tx_index: 2,
        from,
        to: None,
        contract_address: Some(contract_address),
        gas_used: 100_000,
        cumulative_gas_used: 150_000,
        effective_gas_price: 2_000_000_000,
        success: true,
        logs: vec![log0.clone(), log1.clone()],
        tx_type: 0,
    }]);

    let got = rpc
        .get_transaction_receipt(tx_hash)
        .expect("receipt")
        .clone();
    assert_eq!(got["transactionHash"], data_hex(tx_hash.as_slice()));
    assert_eq!(got["blockHash"], data_hex(block_hash.as_slice()));
    assert_eq!(got["blockNumber"], "0x5");
    assert_eq!(got["transactionIndex"], "0x2");
    assert_eq!(got["from"], data_hex(from.as_slice()));
    assert_eq!(got["to"], Value::Null, "contract-creation tx has no `to`");
    assert_eq!(
        got["contractAddress"],
        data_hex(contract_address.as_slice())
    );
    assert_eq!(got["gasUsed"], "0x186a0");
    assert_eq!(got["cumulativeGasUsed"], "0x249f0");
    assert_eq!(got["effectiveGasPrice"], "0x77359400");
    assert_eq!(got["status"], "0x1", "success == status 1");
    assert_eq!(got["type"], "0x0", "legacy tx type");

    let logs = got["logs"].as_array().expect("logs array");
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0]["address"], data_hex(log0.address.as_slice()));
    assert_eq!(
        logs[0]["topics"],
        json!([data_hex(log0.topics()[0].as_slice())])
    );
    assert_eq!(logs[0]["data"], data_hex(b"one"));
    assert_eq!(logs[0]["blockHash"], data_hex(block_hash.as_slice()));
    assert_eq!(logs[0]["transactionHash"], data_hex(tx_hash.as_slice()));
    assert_eq!(logs[0]["transactionIndex"], "0x2");
    assert_eq!(logs[0]["blockNumber"], "0x5");
    assert_eq!(logs[0]["logIndex"], "0x0");
    assert_eq!(logs[0]["removed"], false);
    assert_eq!(logs[1]["logIndex"], "0x1");
    assert_eq!(
        logs[1]["topics"],
        json!([
            data_hex(log1.topics()[0].as_slice()),
            data_hex(log1.topics()[1].as_slice())
        ])
    );

    // logsBloom folds both logs (non-zero; a full re-derivation is out of
    // scope for this shape assertion).
    let bloom = got["logsBloom"].as_str().expect("logsBloom string");
    assert_ne!(bloom, "0x", "logsBloom must be non-trivial with 2 logs");
}

/// `0x`-hex full-width data encoding (mirrors `eth.rs`'s private `data()`
/// helper — this is an external integration test, so it re-derives the same
/// encoding rather than reaching into the crate's private helpers).
fn data_hex(bytes: &[u8]) -> Value {
    Value::String(format!("0x{}", hex::encode(bytes)))
}

// ─── debug_traceTransaction (deferred) ───────────────────────────────────────

#[test]
fn debug_trace_transaction_is_deferred() {
    let (_d, rpc, _mempool, _tx_index) = setup();
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
    let (_d, rpc, _mempool, _tx_index) = setup();
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
