// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PendingWorkWaiter` seam (M9.15 proposal-initiation, task 1): a
//! forwarder needs to await buildable EVM work WITHOUT holding the
//! consensus-shared `Arc<Mutex<dyn Vm>>` (the M7.18 lock-parking hazard —
//! parking `wait_for_event` inside that lock wedges verify/get/build).
//! `EvmVm::pending_work_waiter` hands out a waiter that holds only the two
//! mempool `Arc`s, never the outer VM mutex — mirroring the subscribe-both-
//! pools-then-select shape `EvmVm::wait_for_event` already uses.
//!
//! Setup is repeated (not imported) from `tx_pipeline.rs` per the test-file
//! convention in this crate: boot `EvmVm::from_genesis` over the committed
//! local C-Chain genesis so the pre-funded "ewoq" EOA can sign admittable
//! transfers, and admit through `evm_mempool_handle()` (the same `Arc`
//! `create_handlers` hands to `EthRpc::new`).

use std::sync::Arc;
use std::time::Duration;

use ava_crypto::secp256k1::PrivateKey;
use ava_evm::mempool::{AdmissionRules, SenderAccount};
use ava_evm::vm::EvmVm;
use ava_evm_reth::{
    Address, EvmSignature, SignableTransaction, SignerRecoverable, TransactionSigned, TxKind,
    TxLegacy, U256,
};
use ava_types::constants::LOCAL_ID;
use ava_vm::vm::Vm;

/// The well-known "ewoq" pre-funded private key on `local` networks (matches
/// `proposer_candidates.rs::EWOQ_KEY_HEX` / `tx_pipeline.rs::EWOQ_KEY_HEX`).
const EWOQ_KEY_HEX: &str = "56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027";

/// The local C-Chain id (the `local.json` genesis config `chainId`).
const CHAIN_ID: u64 = 43112;

/// A gas price comfortably above the AP3 genesis base fee (225 gwei) so the tx
/// is never dropped as underpriced (`tx_pipeline.rs::GAS_PRICE_WEI`).
const GAS_PRICE_WEI: u128 = 300_000_000_000;

/// The committed C-Chain local genesis JSON — the sole `alloc` entry funds ewoq.
fn local_genesis_json() -> &'static str {
    include_str!("vectors/cchain/genesis/local.json")
}

/// The ewoq signing key.
fn ewoq_key() -> PrivateKey {
    PrivateKey::from_bytes(&hex::decode(EWOQ_KEY_HEX).expect("ewoq key hex")).expect("ewoq key")
}

/// The ewoq genesis balance (`tx_pipeline.rs::submitted_tx_flows_through_build_accept_receipt`).
fn ewoq_balance() -> U256 {
    U256::from_str_radix("295BE96E64066972000000", 16).expect("ewoq genesis balance")
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
    let recovered = signed
        .try_into_recovered()
        .expect("recover transfer sender");
    let hash = *recovered.hash();
    (recovered, hash)
}

/// Builds a fresh `EvmVm` on the local committed genesis (ewoq funded, nonce 0).
/// Returns the backing `TempDir` alongside the VM — the Firewood state
/// provider persists into it, so callers must keep the directory alive for as
/// long as the VM is used (mirroring `tx_pipeline.rs`'s `let dir = ...`
/// binding staying in scope for the whole test body).
fn build_vm() -> (EvmVm, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vm, _genesis_id) =
        EvmVm::from_genesis(LOCAL_ID, dir.path(), local_genesis_json().as_bytes())
            .expect("EvmVm::from_genesis over the committed local genesis");
    (vm, dir)
}

#[tokio::test]
async fn waiter_fires_on_evm_pool_admission_without_vm_lock() {
    let (vm, _dir) = build_vm();
    let waiter = vm
        .pending_work_waiter()
        .expect("EvmVm exposes a PendingWorkWaiter");
    assert!(!waiter.has_pending(), "empty pools => nothing pending");

    // Park a wait() on another task; it must resolve when a tx is admitted.
    // The waiter is cloned into the spawned task and never touches the outer
    // `EvmVm` (no `Arc<Mutex<dyn Vm>>` is captured) — only the pool `Arc`s.
    let w2 = Arc::clone(&waiter);
    let parked = tokio::spawn(async move { w2.wait().await });

    // Give the parked task a moment to actually register its subscription
    // before we admit, so this exercises the "already parked" path (the
    // no-lost-wake path is exercised by the third test).
    tokio::task::yield_now().await;

    let (tx, tx_hash) = signed_transfer(0);
    let sender = SenderAccount {
        nonce: 0,
        balance: ewoq_balance(),
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

    tokio::time::timeout(Duration::from_secs(5), parked)
        .await
        .expect("wait() must resolve within 5s of admission")
        .expect("parked wait() task must not panic");
    assert!(waiter.has_pending(), "has_pending true after admission");
}

#[tokio::test]
async fn wait_returns_immediately_when_already_pending() {
    let (vm, _dir) = build_vm();
    let (tx, tx_hash) = signed_transfer(0);
    let sender = SenderAccount {
        nonce: 0,
        balance: ewoq_balance(),
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

    let waiter = vm
        .pending_work_waiter()
        .expect("EvmVm exposes a PendingWorkWaiter");
    assert!(waiter.has_pending(), "pool already has the admitted tx");
    tokio::time::timeout(Duration::from_secs(1), waiter.wait())
        .await
        .expect("wait() returns at once when work already present");
}

#[tokio::test]
async fn no_lost_wake_between_check_and_wait() {
    // Admit from a task spawned immediately before the `wait()` call, racing
    // the subscribe-then-check inside `wait()`. If the impl checked emptiness
    // BEFORE subscribing to the pool's `Notify`, an admission landing between
    // the check and the subscribe would be lost and this would hang past the
    // timeout. Subscribing first (as `wait_for_event` does) closes that
    // window: the `Notify` permit set by `notify_one` after subscription is
    // always observed.
    let (vm, _dir) = build_vm();
    let waiter = vm
        .pending_work_waiter()
        .expect("EvmVm exposes a PendingWorkWaiter");

    let (tx, tx_hash) = signed_transfer(0);
    let sender = SenderAccount {
        nonce: 0,
        balance: ewoq_balance(),
    };
    let rules = AdmissionRules {
        chain_id: CHAIN_ID,
        ..Default::default()
    };
    let handle = vm.evm_mempool_handle();
    let admit = tokio::spawn(async move {
        handle
            .lock()
            .add_local(tx, &sender, &rules)
            .expect("admit ewoq transfer")
    });

    tokio::time::timeout(Duration::from_secs(5), waiter.wait())
        .await
        .expect("no lost wake between subscribe and check");
    let admitted = admit.await.expect("admit task must not panic");
    assert_eq!(admitted, tx_hash, "add_local returns the tx hash");
    assert!(waiter.has_pending(), "has_pending true after admission");
}
