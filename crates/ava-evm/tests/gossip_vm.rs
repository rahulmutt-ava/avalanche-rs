// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `EvmVm` p2p tx-gossip wiring (cchain-tx-gossip task 12): `initialize`
//! builds a `P2pNetwork` over the supplied `AppSender`, registers the
//! `TX_GOSSIP_HANDLER_ID` `GossipHandler`, and spawns the push/pull cadence
//! loops (coreth `vm.go:780-833` ordering). These are black-box tests: they
//! hand-build the exact varint-prefixed wire frames a peer would send and
//! drive them through `EvmVm`'s `AppHandler` impl, and (for the outbound
//! side) inspect what a recording `AppSender` observed.
//!
//! Setup mirrors `pending_work_waiter.rs`'s convention: boot
//! `EvmVm::from_genesis` over the committed local C-Chain genesis so the
//! pre-funded "ewoq" EOA can sign admittable transactions.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use ava_crypto::secp256k1::PrivateKey;
use ava_database::{DynDatabase, MemDb};
use ava_evm::mempool::{AdmissionRules, SenderAccount};
use ava_evm::vm::EvmVm;
use ava_evm_reth::{
    Address, Encodable2718, EvmSignature, RecoveredTx, SignableTransaction, SignerRecoverable,
    TransactionSigned, TxKind, TxLegacy, U256,
};
use ava_p2p::gossip::bloom::BloomSet;
use ava_p2p::handler::TX_GOSSIP_HANDLER_ID;
use ava_p2p::network::protocol_prefix;
use ava_p2p::pb::sdk;
use ava_snow::ChainContext;
use ava_types::constants::LOCAL_ID;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::app::AppHandler;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::vm::Vm;
use bytes::Bytes;
use prost::Message;
use tokio_util::sync::CancellationToken;

/// The well-known "ewoq" pre-funded private key on `local` networks (matches
/// `pending_work_waiter.rs::EWOQ_KEY_HEX`).
const EWOQ_KEY_HEX: &str = "56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027";

/// The local C-Chain id (the `local.json` genesis config `chainId`).
const CHAIN_ID: u64 = 43112;

/// A gas price comfortably above the AP3 genesis base fee (225 gwei) so the
/// tx is never dropped as underpriced.
const GAS_PRICE_WEI: u128 = 300_000_000_000;

/// The committed local C-Chain genesis JSON — the sole `alloc` entry funds ewoq.
fn local_genesis_json() -> &'static str {
    include_str!("vectors/cchain/genesis/local.json")
}

fn ewoq_key() -> PrivateKey {
    PrivateKey::from_bytes(&hex::decode(EWOQ_KEY_HEX).expect("ewoq key hex")).expect("ewoq key")
}

/// The ewoq genesis balance (`pending_work_waiter.rs::ewoq_balance`).
fn ewoq_balance() -> U256 {
    U256::from_str_radix("295BE96E64066972000000", 16).expect("ewoq genesis balance")
}

/// A funded ewoq self-transfer at `nonce`, signed EIP-155 over `CHAIN_ID`.
/// Returns the recovered tx and its hash.
fn signed_transfer(nonce: u64) -> (RecoveredTx, ava_evm_reth::B256) {
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

fn test_node(byte: u8) -> NodeId {
    NodeId::from([byte; 20])
}

fn chain_ctx() -> Arc<ChainContext> {
    Arc::new(ChainContext {
        network_id: LOCAL_ID,
        subnet_id: Id::EMPTY,
        chain_id: Id::EMPTY,
        node_id: test_node(0),
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(LOCAL_ID),
        x_chain_id: Id::EMPTY,
        c_chain_id: Id::EMPTY,
        avax_asset_id: Id::EMPTY,
        chain_data_dir: std::path::PathBuf::new(),
    })
}

/// One recorded `send_app_response` call's arguments.
type RecordedResponse = (NodeId, u32, Vec<u8>);
/// One recorded `send_app_gossip` call's arguments.
type RecordedGossip = (SendConfig, Vec<u8>);

/// Records every `send_app_response`/`send_app_gossip` call it receives;
/// every method otherwise succeeds trivially (mirrors `ava-p2p`'s
/// `RecordingSender` test-double convention, duplicated here per that
/// crate's established per-module-recorder pattern).
#[derive(Default)]
struct RecordingSender {
    responses: StdMutex<Vec<RecordedResponse>>,
    gossips: StdMutex<Vec<RecordedGossip>>,
}

#[async_trait::async_trait]
impl AppSender for RecordingSender {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        _nodes: &std::collections::HashSet<NodeId>,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
        Ok(())
    }

    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
        self.responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((node, request_id, bytes));
        Ok(())
    }

    async fn send_app_error(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _code: i32,
        _message: &str,
    ) -> ava_vm::error::Result<()> {
        Ok(())
    }

    async fn send_app_gossip(
        &self,
        _token: &CancellationToken,
        config: SendConfig,
        bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
        self.gossips
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((config, bytes));
        Ok(())
    }
}

/// Builds a fresh `EvmVm` on the local committed genesis (ewoq funded, nonce
/// 0) and drives it through `Vm::initialize` over `sender` — the seam that
/// builds the tx-gossip `P2pNetwork` (Task 12). Returns the backing
/// `TempDir` alongside the VM — the Firewood state provider persists into
/// it, so callers must keep the directory alive for as long as the VM is
/// used.
async fn build_initialized_vm(sender: Arc<dyn AppSender>) -> (EvmVm, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut vm, _genesis_id) =
        EvmVm::from_genesis(LOCAL_ID, dir.path(), local_genesis_json().as_bytes())
            .expect("EvmVm::from_genesis over the committed local genesis");

    let token = CancellationToken::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    vm.initialize(
        &token,
        chain_ctx(),
        db,
        local_genesis_json().as_bytes(),
        b"",
        b"",
        Vec::new(),
        sender,
    )
    .await
    .expect("EvmVm::initialize wires the tx-gossip system");

    (vm, dir)
}

/// A wire-format `PushGossip` frame carrying `tx`'s EIP-2718 bytes, prefixed
/// with the tx-gossip handler id (what a peer's `PushGossiper::send` would
/// actually put on the wire, `ava-p2p`'s `gossip/push.rs::send`).
fn push_gossip_frame(tx: &RecoveredTx) -> Vec<u8> {
    let mut frame = protocol_prefix(TX_GOSSIP_HANDLER_ID);
    let msg = sdk::PushGossip {
        gossip: vec![Bytes::from(tx.encoded_2718())],
    };
    frame.extend_from_slice(&msg.encode_to_vec());
    frame
}

// ---------------------------------------------------------------------------
// Step 1: inbound push gossip lands in the mempool.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbound_push_gossip_lands_in_mempool() {
    let sender = Arc::new(RecordingSender::default());
    let (mut vm, _dir) = build_initialized_vm(sender).await;

    let (tx, tx_hash) = signed_transfer(0);
    let frame = push_gossip_frame(&tx);

    let token = CancellationToken::new();
    let node = test_node(1);
    vm.app_gossip(&token, node, &frame)
        .await
        .expect("app_gossip must route to the registered tx-gossip handler");

    assert!(
        vm.evm_mempool_handle().lock().contains(&tx_hash),
        "an inbound PushGossip frame for a valid, admittable tx must land in the EVM mempool"
    );
}

// ---------------------------------------------------------------------------
// Step 2: a pull request is answered with the pool's contents.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pull_request_answered_with_pool_contents() {
    let sender = Arc::new(RecordingSender::default());
    let (mut vm, _dir) = build_initialized_vm(Arc::clone(&sender) as Arc<dyn AppSender>).await;

    let (tx, tx_hash) = signed_transfer(0);
    let account = SenderAccount {
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
        .add_local(tx.clone(), &account, &rules)
        .expect("seed the mempool via add_local");
    assert_eq!(admitted, tx_hash);

    // An "empty-ish" filter: a fresh bloom filter with nothing added, so the
    // handler reports every pooled tx back (mirrors `ava-p2p`'s
    // `gossip_loops.rs::handler_answers_pull_excluding_known` filter
    // construction).
    let empty_filter = BloomSet::new(64, 0.01, 0.05).expect("BloomSet::new");
    let (filter_bytes, salt_bytes) = empty_filter.marshal();

    let mut frame = protocol_prefix(TX_GOSSIP_HANDLER_ID);
    let request = sdk::PullGossipRequest {
        salt: Bytes::from(salt_bytes),
        filter: Bytes::from(filter_bytes),
    };
    frame.extend_from_slice(&request.encode_to_vec());

    let token = CancellationToken::new();
    let node = test_node(1);
    vm.app_request(
        &token,
        node,
        7,
        Instant::now() + Duration::from_secs(5),
        &frame,
    )
    .await
    .expect("app_request must route to the registered tx-gossip handler");

    let responses = sender.responses.lock().unwrap();
    assert_eq!(
        responses.len(),
        1,
        "the pull request must be answered with exactly one AppResponse"
    );
    let (resp_node, resp_request_id, resp_bytes) = responses.first().unwrap();
    assert_eq!(*resp_node, node);
    assert_eq!(*resp_request_id, 7);

    let decoded = sdk::PullGossipResponse::decode(resp_bytes.as_slice())
        .expect("response bytes decode as PullGossipResponse");
    assert_eq!(
        decoded.gossip,
        vec![Bytes::from(tx.encoded_2718())],
        "the response must carry the seeded tx's EIP-2718 bytes"
    );
}

// ---------------------------------------------------------------------------
// Step 3: a locally-submitted tx is pushed to the network on the next push
// cycle (paused time).
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn local_submission_pushes_gossip() {
    let sender = Arc::new(RecordingSender::default());
    let (vm, _dir) = build_initialized_vm(Arc::clone(&sender) as Arc<dyn AppSender>).await;

    let (tx, tx_hash) = signed_transfer(0);
    let account = SenderAccount {
        nonce: 0,
        balance: ewoq_balance(),
    };
    let rules = AdmissionRules {
        chain_id: CHAIN_ID,
        ..Default::default()
    };
    vm.evm_mempool_handle()
        .lock()
        .add_local(tx.clone(), &account, &rules)
        .expect("add_local");

    // Advance virtual time past the push cadence (`GossipParams::default()`'s
    // `push_period`, 100ms) a few times over so the spawned push loop's next
    // tick has definitely fired and drained the mempool's gossip outbox
    // (`EvmMempool::take_gossip_outbox`) into a `PushGossip` send — mirroring
    // `ava-p2p`'s `gossip_loops.rs::every_runs_cycle_on_period_and_stops_on_cancel`,
    // which sleeps in a loop under paused time rather than a single
    // `tokio::time::advance` so every parked task (the test's own sleep AND
    // the spawned loop's `interval.tick()`) gets a chance to run.
    let push_period = ava_p2p::gossip::GossipParams::default().push_period;
    for _ in 0..3 {
        tokio::time::sleep(push_period).await;
    }

    let gossips = sender.gossips.lock().unwrap();
    assert!(
        !gossips.is_empty(),
        "the push loop must have sent at least one PushGossip after push_period elapsed"
    );

    let expected_prefix = protocol_prefix(TX_GOSSIP_HANDLER_ID);
    let mut found = false;
    for (_cfg, bytes) in gossips.iter() {
        let Some(payload) = bytes.get(expected_prefix.len()..) else {
            continue;
        };
        let Ok(decoded) = sdk::PushGossip::decode(payload) else {
            continue;
        };
        if decoded
            .gossip
            .iter()
            .any(|g| g.as_ref() == tx.encoded_2718())
        {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "the locally-submitted tx (hash {tx_hash:?}) must appear in a push-gossip send"
    );
}
