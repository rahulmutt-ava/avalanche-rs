// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! End-to-end C-Chain EVM tx pipeline (cchain-tx-pipeline task 5): a signed
//! transfer admitted to the [`EvmMempool`](ava_evm::mempool::EvmMempool) flows
//! through `wait_for_event` (notify-driven `PendingTxs`, NOT cancellation) →
//! `build_block` (which packs the pooled tx) → the full `Block::verify` path →
//! `Block::accept` (which runs pool maintenance + persists the receipt), and the
//! served receipt names the accepted block height.
//!
//! Boots the VM straight from the committed local C-Chain genesis
//! (`EvmVm::from_genesis`, the same seam `proposer_candidates.rs` drives) so the
//! pre-funded "ewoq" EOA is spendable and genesis is the preferred build parent.
//!
//! The tx is admitted directly through the VM's `evm_mempool_handle` (the same
//! `Arc` `create_handlers` hands to `EthRpc::new`); the RPC `eth_sendRawTransaction`
//! admission leg is covered by `rpc_eth.rs::send_raw_transaction_*`, so this test
//! keeps the focus on build → accept → receipt.

use std::time::Duration;

use ava_crypto::secp256k1::PrivateKey;
use ava_evm::block::decode_ava_evm_block;
use ava_evm::chainspec::AvaChainSpec;
use ava_evm::mempool::{AdmissionRules, SenderAccount};
use ava_evm::vm::EvmVm;
use ava_evm_reth::{
    Address, Chain, EvmSignature, SignableTransaction, SignerRecoverable, TransactionSigned,
    TxKind, TxLegacy, U256,
};
use ava_types::constants::LOCAL_ID;
use ava_vm::block::ChainVm;
use ava_vm::vm::{Vm, VmEvent};
use tokio_util::sync::CancellationToken;

/// The well-known "ewoq" pre-funded private key on `local` networks (matches
/// `proposer_candidates.rs::EWOQ_KEY_HEX`).
const EWOQ_KEY_HEX: &str = "56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027";

/// The local C-Chain id (the `local.json` genesis config `chainId`).
const CHAIN_ID: u64 = 43112;

/// A gas price comfortably above the AP3 genesis base fee (225 gwei) so the tx
/// is never dropped as underpriced while `build_on` packs against the real base
/// fee (`proposer_candidates.rs::HONEST_TX_GAS_PRICE`).
const GAS_PRICE_WEI: u128 = 300_000_000_000;

/// The committed C-Chain local genesis JSON — the sole `alloc` entry funds ewoq.
fn local_genesis_json() -> &'static str {
    include_str!("vectors/cchain/genesis/local.json")
}

/// The ewoq signing key.
fn ewoq_key() -> PrivateKey {
    PrivateKey::from_bytes(&hex::decode(EWOQ_KEY_HEX).expect("ewoq key hex")).expect("ewoq key")
}

/// A funded ewoq self-transfer at `nonce`, signed EIP-155 over `CHAIN_ID`.
/// Returns the recovered tx and its hash.
fn signed_transfer(nonce: u64) -> (ava_evm_reth::RecoveredTx, ava_evm_reth::B256) {
    let key = ewoq_key();
    let ewoq_addr = Address::from(key.public_key().eth_address());
    let tx = TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce,
        gas_price: GAS_PRICE_WEI,
        gas_limit: 21_000,
        to: TxKind::Call(ewoq_addr),
        value: U256::from(1u64),
        input: Default::default(),
    };
    let sig_hash = tx.signature_hash();
    let rsv = key.sign_hash(&sig_hash.0).expect("sign transfer");
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    let signed = TransactionSigned::Legacy(tx.into_signed(sig));
    let recovered = signed.try_into_recovered().expect("recover transfer sender");
    let hash = *recovered.hash();
    (recovered, hash)
}

#[tokio::test]
async fn submitted_tx_flows_through_build_accept_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut vm, _genesis_id) =
        EvmVm::from_genesis(LOCAL_ID, dir.path(), local_genesis_json().as_bytes())
            .expect("EvmVm::from_genesis over the committed local genesis");
    let token = CancellationToken::new();

    // 1. Admit a signed transfer via the VM's EVM mempool handle (the same Arc
    //    the RPC service admits into). ewoq is funded at nonce 0 in genesis.
    let (tx, tx_hash) = signed_transfer(0);
    let ewoq_balance =
        U256::from_str_radix("295BE96E64066972000000", 16).expect("ewoq genesis balance");
    let sender = SenderAccount {
        nonce: 0,
        balance: ewoq_balance,
    };
    let rules = AdmissionRules {
        chain_id: CHAIN_ID,
        ..Default::default()
    };
    let admitted = vm
        .evm_mempool_handle()
        .lock()
        .add_local(tx, &sender, &rules)
        .expect("admit ewoq transfer");
    assert_eq!(admitted, tx_hash, "add_local returns the tx hash");

    // 2. wait_for_event returns PendingTxs off the pool's admission notify —
    //    WITHOUT cancellation — bounded by a 5s timeout. A never-firing
    //    wait_for_event (the pre-task-5 cancellation-only park) times out here.
    let event = tokio::time::timeout(Duration::from_secs(5), vm.wait_for_event(&token))
        .await
        .expect("wait_for_event must return without cancellation (notify path)")
        .expect("wait_for_event");
    assert_eq!(
        event,
        VmEvent::PendingTxs,
        "a pooled EVM tx signals PendingTxs"
    );

    // 3. build_block packs the pooled tx into the built block.
    let built = vm.build_block(&token).await.expect("build_block");
    let built_height = built.height();
    assert_eq!(built_height, 1, "the block builds on genesis (height 0)");

    let spec = AvaChainSpec::c_chain(LOCAL_ID, Chain::from_id(CHAIN_ID));
    let decoded = decode_ava_evm_block(built.bytes(), &spec).expect("decode built block");
    let included: Vec<_> = decoded
        .transactions()
        .iter()
        .map(|tx| *tx.tx_hash())
        .collect();
    assert!(
        included.contains(&tx_hash),
        "the built block must carry the admitted tx (got {included:?})"
    );

    // 4. The built block passes the FULL verify path (the same entry the ChainVm
    //    adapter drives).
    built
        .verify(&token)
        .await
        .expect("the built block must pass full verify");

    // 5. accept runs pool maintenance (pool drains) and persists the receipt.
    built.accept(&token).await.expect("accept built block");
    assert!(
        vm.evm_mempool_handle().lock().is_empty(),
        "accept maintenance must drain the included tx from the pool"
    );

    // 6. The receipt is served, keyed to the accepted block height.
    let receipt = vm
        .accepted_tx_index()
        .get(&tx_hash)
        .expect("receipt served after accept");
    assert_eq!(
        receipt.block_number, built_height,
        "the served receipt names the accepted block height"
    );
}
