// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 proposal-initiation (task 2) — the **per-chain proposal forwarder**
//! (production `NotificationForwarder`, Go `snow/engine/common/notifier.go`).
//!
//! This is the load-bearing proof that a pending EVM tx *by itself* — with **no
//! manual `vm_tx.send(PendingTxs)`** — drives the running Snowman engine to
//! `build_block`, issue it, and (via the solo node's self-loopback poll) accept a
//! real height-1 C-Chain block. The trigger is the forwarder `create_snowman_chain`
//! spawns off the Task-1 `PendingWorkWaiter`: it awaits buildable EVM work WITHOUT
//! holding the consensus-shared `Arc<Mutex<dyn Vm>>`, then sends `PendingTxs` into
//! the existing VM→engine channel.
//!
//! Contrast with `engine_issuance.rs` / `in_process_chain.rs`'s
//! `engine_accepts_self_built_block_via_loopback`, which drive the *same* engine
//! path but by **manually** sending `PendingTxs`. Here the send is the forwarder's
//! job — remove the forwarder and this test times out (RED), because the admitted
//! tx sits in the pool forever and the engine is never woken.
//!
//! Harness choice: the real `create_snowman_chain` forwarder-spawn is only
//! reachable with the self-loopback installed, which lives in `avalanchers`'
//! `boot_chain_with_loopback`. So the test boots a **real `EvmVm`**
//! (`EvmVm::from_genesis` over the committed local C-Chain genesis, ewoq funded)
//! through that path. It boots with `network_id = 1` (mainnet) so the proposervm
//! wrapper is pre-fork pass-through at the in-process boot clock (UNIX_EPOCH) and
//! `build_block` reaches the inner `EvmVm` directly — exactly as `engine_issuance.rs`
//! relies on. The EVM chain rules stay `local` (from `from_genesis(LOCAL_ID)`); the
//! two layers are independent and `EvmVm::initialize` ignores both the genesis
//! bytes and the boot network id (it only records the chain context).

use std::sync::Arc;
use std::time::{Duration, Instant};

use ava_crypto::secp256k1::PrivateKey;
use ava_database::{DynDatabase, MemDb};
use ava_evm::mempool::{AdmissionRules, SenderAccount};
use ava_evm::vm::EvmVm;
use ava_evm_reth::{
    Address, EvmSignature, SignableTransaction, SignerRecoverable, TransactionSigned, TxKind,
    TxLegacy, U256,
};
use ava_snow::EngineState;
use ava_types::constants::{LOCAL_ID, PRIMARY_NETWORK_ID};
use ava_types::id::Id;
use avalanchers::wiring::chains::{PChainBootHandle, boot_chain_with_loopback};

/// The well-known "ewoq" pre-funded private key on `local` networks (matches
/// `ava_evm`'s `tx_pipeline.rs` / `pending_work_waiter.rs`).
const EWOQ_KEY_HEX: &str = "56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027";

/// The local C-Chain id (the `local.json` genesis config `chainId`).
const CHAIN_ID: u64 = 43112;

/// A gas price comfortably above the AP3 genesis base fee (225 gwei).
const GAS_PRICE_WEI: u128 = 300_000_000_000;

/// The committed C-Chain local genesis JSON — the sole `alloc` entry funds ewoq.
fn local_genesis_json() -> &'static str {
    include_str!("vectors/cchain/genesis/local.json")
}

/// The ewoq signing key.
fn ewoq_key() -> PrivateKey {
    PrivateKey::from_bytes(&hex::decode(EWOQ_KEY_HEX).expect("ewoq key hex")).expect("ewoq key")
}

/// The ewoq genesis balance.
fn ewoq_balance() -> U256 {
    U256::from_str_radix("295BE96E64066972000000", 16).expect("ewoq genesis balance")
}

/// A funded ewoq self-transfer at `nonce`, signed EIP-155 over `CHAIN_ID`.
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

/// Poll the shared `ConsensusContext` until the solo engine reaches `NormalOp`
/// (empty beacons short-circuit Bootstrapping → NormalOp). Bounded by a wall-clock
/// deadline so a stuck boot fails rather than hangs.
async fn await_normalop(handle: &PChainBootHandle) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(15))
        .expect("deadline in range");
    while Instant::now() < deadline {
        if matches!(**handle.ctx.state.load(), EngineState::NormalOp) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "the solo C-Chain engine reached NormalOp (last state: {:?})",
        **handle.ctx.state.load()
    );
}

/// The forwarder — spawned by `create_snowman_chain` off the EVM VM's
/// `PendingWorkWaiter` — must turn a pool admission into an engine build+accept
/// with **no manual `vm_tx.send`**. Boot a real `EvmVm` through the loopback
/// engine path, reach NormalOp, admit a signed ewoq transfer directly into the
/// EVM mempool, and assert an ACCEPTED height-1 block carrying that tx appears
/// (the served receipt names height 1). Bounded by a timeout so a missing
/// forwarder FAILS (RED) rather than hangs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forwarder_drives_submitted_tx_to_accepted_block() {
    // A real EVM VM over the committed local genesis (ewoq funded at nonce 0). The
    // Firewood state db lives in an owned scratch dir kept alive by the boot handle.
    let dir = tempfile::tempdir().expect("tempdir");
    let (vm, genesis_id) = EvmVm::from_genesis(LOCAL_ID, dir.path(), local_genesis_json().as_bytes())
        .expect("EvmVm::from_genesis over the committed local genesis");

    // Clone the two lock-free observation handles BEFORE the VM is moved into the
    // chain: the EVM mempool (to admit the tx) and the accepted-tx receipt index
    // (to observe acceptance). These are the same `Arc`s the forwarder's waiter and
    // the block accept-side writer hold — never the outer VM mutex.
    let pool = vm.evm_mempool_handle();
    let receipts = vm.accepted_tx_index();

    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());

    // Boot with network_id = 1 (mainnet) so the proposervm wrapper is pre-fork
    // pass-through at the in-process UNIX_EPOCH boot clock (build_block reaches the
    // inner EvmVm directly). A fixed C-Chain-ish blockchain id; solo node.
    let chain_id = Id::from([0xC5; 32]);
    let handle = boot_chain_with_loopback(
        1, // network_id (mainnet ⇒ proposervm pass-through at UNIX_EPOCH)
        chain_id,
        PRIMARY_NETWORK_ID,
        "C",
        Id::EMPTY, // avax_asset_id — the EVM genesis carries none (matches boot_cchain)
        genesis_id,
        vm,
        local_genesis_json().as_bytes().to_vec(),
        Arc::clone(&base),
    )
    .await
    .expect("boot a real EvmVm chain with the self-loopback installed");

    // Keep the Firewood scratch dir alive for the VM's lifetime.
    let _dir = dir;

    await_normalop(&handle).await;

    // The tip is genesis (nothing accepted yet).
    assert!(
        receipts.get(&signed_transfer(0).1).is_none(),
        "no receipt exists before the tx is admitted and built"
    );

    // Admit a signed transfer directly to the EVM mempool — NO manual vm_tx.send.
    // The forwarder's parked waiter observes the admission and wakes the engine.
    let (tx, tx_hash) = signed_transfer(0);
    let sender = SenderAccount {
        nonce: 0,
        balance: ewoq_balance(),
    };
    let rules = AdmissionRules {
        chain_id: CHAIN_ID,
        ..Default::default()
    };
    let admitted = pool
        .lock()
        .add_local(tx, &sender, &rules)
        .expect("admit ewoq transfer");
    assert_eq!(admitted, tx_hash, "add_local returns the tx hash");

    // Poll until the engine-accepted block persists the receipt (block_number == 1).
    // Bounded by a wall-clock deadline: WITHOUT the forwarder the tx sits in the
    // pool forever, the engine is never woken, no receipt appears, and this fails.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(20))
        .expect("deadline in range");
    let mut accepted_height = None;
    while Instant::now() < deadline {
        if let Some(receipt) = receipts.get(&tx_hash) {
            accepted_height = Some(receipt.block_number);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        accepted_height,
        Some(1),
        "the forwarder woke the engine, which built + accepted a height-1 block \
         carrying the admitted tx (NO manual vm_tx.send)"
    );

    // Acceptance drained the tx from the pool (the engine's accept maintenance).
    assert!(
        pool.lock().is_empty(),
        "the accepted block's pool maintenance drained the included tx"
    );

    // No leaked task: cancel and join cleanly.
    handle.token.cancel();
    handle.join.await.expect("handler task joined cleanly");
}
