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
    AccessList, AccessListItem, Address, B256, BundleState, Bytes, Chain, Decodable2718,
    Encodable2718, EvmSignature, KECCAK_EMPTY, Log, SignableTransaction, TransactionSigned,
    TxEip1559, TxKind, TxLegacy, U256, keccak256,
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

/// Signs `tx` with [`funded_key`] as an EIP-1559 transaction (the same
/// `signature_hash` -> `sign_hash` -> `into_signed` recipe [`sign_legacy`]
/// uses, generic over [`SignableTransaction`]) and returns its EIP-2718
/// raw bytes.
fn sign_1559(tx: TxEip1559) -> Bytes {
    let sig_hash = tx.signature_hash();
    let rsv = funded_key().sign_hash(&sig_hash.0).expect("sign_hash");
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    let signed = TransactionSigned::Eip1559(tx.into_signed(sig));
    Bytes::from(signed.encoded_2718())
}

/// The one-entry access list [`funded_1559_tx`] signs into its tx (a single
/// `contract()` address / one storage key), returned alongside the raw tx so
/// tests can assert the RPC's `accessList` shape against a value they derive
/// independently of the handler, not a re-statement of it.
fn funded_1559_access_list() -> AccessList {
    AccessList(vec![AccessListItem {
        address: contract(),
        storage_keys: vec![B256::repeat_byte(0x07)],
    }])
}

/// A funded-signer EIP-1559 tx: `nonce`, a 2 gwei fee cap / 1 gwei tip (both
/// above the pool's 1-wei tip floor), a one-entry access list
/// ([`funded_1559_access_list`]) so the `accessList` JSON encoding is
/// actually exercised, and `CHAIN_ID`.
fn funded_1559_tx(nonce: u64) -> Bytes {
    sign_1559(TxEip1559 {
        chain_id: CHAIN_ID,
        nonce,
        gas_limit: 100_000,
        max_fee_per_gas: 2_000_000_000,
        max_priority_fee_per_gas: 1_000_000_000,
        to: TxKind::Call(alice()),
        value: U256::from(1u64),
        access_list: funded_1559_access_list(),
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
    // contract-creation success, 2 logs, preceded in the (simulated) block by
    // 3 logs from earlier txs) and assert the full JSON shape — including
    // that `logIndex` is block-wide (go-ethereum `DeriveFields` semantics:
    // `first_log_index` + this log's position within the tx), not reset to 0
    // per tx.
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
        first_log_index: 3,
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
    assert_eq!(
        logs[0]["logIndex"], "0x3",
        "block-wide logIndex = first_log_index (3) + local position (0)"
    );
    assert_eq!(logs[0]["removed"], false);
    assert_eq!(
        logs[1]["logIndex"], "0x4",
        "block-wide logIndex = first_log_index (3) + local position (1)"
    );
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

// ─── eth_getTransactionByHash (pool-pending + mined) ─────────────────────────
// (cchain-tx-gossip task 13, over Task 1's EvmMempool + Task 3's
// AcceptedTxIndex — the same seams `eth_sendRawTransaction`/
// `eth_getTransactionReceipt` above already exercise.)

#[test]
fn get_transaction_by_hash_unknown_is_null() {
    let (_d, rpc, _mempool, _tx_index) = setup();
    let unknown = B256::repeat_byte(0x66);
    assert_eq!(
        rpc.get_transaction_by_hash(unknown).expect("lookup"),
        Value::Null,
        "an unknown hash must return a null result, not an error (coreth: nil, nil)"
    );
}

#[test]
fn get_transaction_by_hash_pending_has_null_block_hash() {
    let (_d, rpc, _mempool, _tx_index) = setup();
    let raw = funded_legacy_tx(0);

    // The tx's own hash + signature, decoded independently of the RPC path
    // (the same "decode as a test oracle" convention
    // `send_raw_transaction_admits_and_returns_hash` uses above), so this
    // test verifies the handler's `v`/`r`/`s` against a value it derives
    // itself, not just against whatever the handler happens to emit.
    let decoded =
        TransactionSigned::decode_2718(&mut raw.as_ref()).expect("decode_2718 (test oracle)");
    let want_hash = *decoded.tx_hash();
    let sig = decoded.signature();
    let y_parity = u64::from(sig.v());
    let want_v = U256::from(CHAIN_ID) * U256::from(2u64) + U256::from(35u64) + U256::from(y_parity);

    rpc.send_raw_transaction(&raw)
        .expect("send_raw_transaction");

    let got = rpc
        .get_transaction_by_hash(want_hash)
        .expect("get_transaction_by_hash");
    assert_eq!(
        got["blockHash"],
        Value::Null,
        "a pooled (un-mined) tx has no block yet — coreth pending shape"
    );
    assert_eq!(got["blockNumber"], Value::Null);
    assert_eq!(got["transactionIndex"], Value::Null);
    assert_eq!(got["hash"], data_hex(want_hash.as_slice()));
    assert_eq!(got["nonce"], "0x0");
    assert_eq!(got["from"], data_hex(funded_address().as_slice()));
    assert_eq!(got["to"], data_hex(alice().as_slice()));
    assert_eq!(got["value"], "0x1");
    assert_eq!(got["gas"], "0x5208", "21000 gas (funded_legacy_tx)");
    assert_eq!(
        got["gasPrice"], "0x77359400",
        "2 gwei gas price (funded_legacy_tx)"
    );
    assert_eq!(got["input"], "0x");
    assert_eq!(got["type"], "0x0", "legacy tx type");
    assert_eq!(
        got["chainId"],
        Value::String(format!("0x{CHAIN_ID:x}")),
        "protected (EIP-155) legacy tx must report chainId"
    );
    assert_eq!(got["r"], Value::String(format!("0x{:x}", sig.r())));
    assert_eq!(got["s"], Value::String(format!("0x{:x}", sig.s())));
    assert_eq!(
        got["v"],
        Value::String(format!("0x{want_v:x}")),
        "EIP-155 chain-id-encoded v"
    );
    assert!(
        got.get("yParity").is_none(),
        "a legacy tx must not carry yParity at all (coreth: no LegacyTxType \
         switch arm sets it, and the struct tag is `omitempty`), got: {got:?}"
    );
    assert!(
        got.get("accessList").is_none(),
        "a legacy tx must not carry accessList at all (coreth: no \
         LegacyTxType switch arm sets it, and the struct tag is \
         `omitempty`), got: {got:?}"
    );
}

#[test]
fn get_transaction_by_hash_pending_1559_shape() {
    let (_d, rpc, _mempool, _tx_index) = setup();
    let raw = funded_1559_tx(0);
    let access_list = funded_1559_access_list();

    // Independent test oracle (same convention as the legacy pending test
    // above): decode the envelope ourselves rather than trust the handler.
    let decoded =
        TransactionSigned::decode_2718(&mut raw.as_ref()).expect("decode_2718 (test oracle)");
    let want_hash = *decoded.tx_hash();
    let sig = decoded.signature();
    let y_parity = u64::from(sig.v());

    rpc.send_raw_transaction(&raw)
        .expect("send_raw_transaction");

    let got = rpc
        .get_transaction_by_hash(want_hash)
        .expect("get_transaction_by_hash");

    assert_eq!(
        got["blockHash"],
        Value::Null,
        "a pooled (un-mined) tx has no block yet — coreth pending shape"
    );
    assert_eq!(got["blockNumber"], Value::Null);
    assert_eq!(got["transactionIndex"], Value::Null);
    assert_eq!(got["hash"], data_hex(want_hash.as_slice()));
    assert_eq!(got["type"], "0x2", "EIP-1559 tx type");
    assert_eq!(
        got["maxFeePerGas"], "0x77359400",
        "2 gwei fee cap (funded_1559_tx)"
    );
    assert_eq!(
        got["maxPriorityFeePerGas"], "0x3b9aca00",
        "1 gwei tip (funded_1559_tx)"
    );
    assert_eq!(
        got["gasPrice"], got["maxFeePerGas"],
        "pending 1559: gasPrice reports the fee cap since no base fee is \
         known yet for an un-mined tx (coreth `else {{ result.GasPrice = \
         tx.GasFeeCap() }}`, internal/ethapi/api.go ~:1429)"
    );
    assert_eq!(
        got["chainId"],
        Value::String(format!("0x{CHAIN_ID:x}")),
        "a typed tx always reports chainId (coreth api.go ~:1424/:1429)"
    );
    assert_eq!(
        got["v"],
        Value::String(format!("0x{y_parity:x}")),
        "1559 v is the bare y-parity, not EIP-155-encoded (coreth \
         RawSignatureValues for any typed tx)"
    );
    assert_eq!(
        got["yParity"],
        Value::String(format!("0x{y_parity:x}")),
        "1559 must carry yParity alongside v (coreth \
         internal/ethapi/api.go ~:1429, DynamicFeeTxType arm)"
    );

    let got_access_list = got["accessList"]
        .as_array()
        .expect("accessList must be an array");
    assert_eq!(
        got_access_list.len(),
        access_list.0.len(),
        "accessList must carry the tx's one entry"
    );
    assert_eq!(
        got_access_list[0]["address"],
        data_hex(access_list.0[0].address.as_slice())
    );
    let got_keys = got_access_list[0]["storageKeys"]
        .as_array()
        .expect("storageKeys must be an array");
    assert_eq!(got_keys.len(), access_list.0[0].storage_keys.len());
    assert_eq!(
        got_keys[0],
        data_hex(access_list.0[0].storage_keys[0].as_slice()),
        "storageKeys entries must be hex-encoded (geth AccessTuple shape)"
    );
}

#[test]
fn get_transaction_by_hash_mined_reports_available_fields_only() {
    // The mined path resolves through the SAME AcceptedTxIndex seam
    // `get_transaction_receipt_null_when_unknown_then_served_after_accept`
    // above seeds directly (no live block-builder harness exists in this
    // test module to actually mine a block; see that test's precedent).
    let (_d, rpc, _mempool, tx_index) = setup();

    let tx_hash = B256::repeat_byte(0x43);
    let block_hash = B256::repeat_byte(0x05);
    let to = Address::repeat_byte(0xEE);
    let from = funded_address();
    tx_index.record(vec![TxReceiptRecord {
        tx_hash,
        block_hash,
        block_number: 5,
        tx_index: 2,
        from,
        to: Some(to),
        contract_address: None,
        gas_used: 21_000,
        cumulative_gas_used: 21_000,
        effective_gas_price: 2_000_000_000,
        success: true,
        logs: Vec::new(),
        tx_type: 0,
        first_log_index: 0,
    }]);

    let got = rpc
        .get_transaction_by_hash(tx_hash)
        .expect("get_transaction_by_hash");

    // Available from the TxReceiptRecord: real, not fabricated.
    assert_eq!(got["blockHash"], data_hex(block_hash.as_slice()));
    assert_eq!(got["blockNumber"], "0x5");
    assert_eq!(got["transactionIndex"], "0x2");
    assert_eq!(got["hash"], data_hex(tx_hash.as_slice()));
    assert_eq!(got["from"], data_hex(from.as_slice()));
    assert_eq!(got["to"], data_hex(to.as_slice()));
    assert_eq!(got["type"], "0x0");

    // NOT reconstructable today (CanonicalStore does not persist the block's
    // Txs RLP list — see EthRpc::get_transaction_by_hash's doc comment for
    // the honest gap): reported as null, never fabricated.
    for field in [
        "nonce",
        "value",
        "gas",
        "gasPrice",
        "input",
        "v",
        "r",
        "s",
        "chainId",
        "yParity",
        "accessList",
    ] {
        assert_eq!(
            got[field],
            Value::Null,
            "mined tx-body field {field:?} must be null (unreachable without \
             block-body storage), not a fabricated value"
        );
    }
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
