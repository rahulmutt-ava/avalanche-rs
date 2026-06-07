// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (AVM) VM-assembly conformance (M5.19, specs 09 §1/§7; 07 §10).
//!
//! Drives the generic [`ava_vm::vm_conformance!`] battery against a fully
//! initialized [`AvmVm`]: genesis == last-accepted == `get_block_id_at_height(0)`,
//! build → verify → accept advances last-accepted + the height index, parse
//! round-trips bytes, `get_block` for processing + accepted blocks, unknown
//! id/height → `Err(NotFound)`, `set_preference` re-parents the next built block,
//! capability probes default to `None`, the `set_state` phase cycle, and
//! idempotent shutdown.
//!
//! ## Seeding the battery (chained spends)
//!
//! The X-Chain has no "advance time" block: a `StandardBlock` only forms when
//! the builder packs at least one tx (else `Error::NoPendingBlocks`). The
//! conformance battery builds up to two blocks per VM (genesis-child at h1, then
//! a child of that *unaccepted* block at h2 — see `set_preference_ok`).
//!
//! The VM packs Go-parity **multi-tx** blocks (FIFO + size-capped) and removes
//! every packed tx from the mempool on build. So [`init_avm_vm`] seeds **one**
//! spendable secp UTXO (`U0`) of a pre-seeded asset and pre-loads the mempool
//! with a **chain** of two `BaseTx`es: `tx1` spends `U0` and produces `U1` (its
//! own output index 0), and `tx2` spends `U1`. Crucially `tx2` is enqueued
//! BEFORE `tx1`: building over the genesis diff in FIFO order the builder hits
//! `tx2` first, `U1` does not exist yet so `tx2` is dropped (left in the pool),
//! then `tx1` packs and produces `U1`. So h1 carries only `tx1` and the builder
//! removes only `tx1`. The h2-child build over h1's verified-but-unaccepted diff
//! then finds `U1` and packs `tx2`.
//!
//! (Were `tx1` enqueued first, the builder would pack BOTH into a single block —
//! the running diff already holds `tx1`'s freshly produced `U1` within the same
//! packing run — leaving the child with nothing (`NoPendingBlocks`). The FIFO
//! ordering is what forces the two-block shape while keeping genuine Go-parity
//! multi-tx packing.)

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_avm::config::Config;
use ava_avm::state::Chain;
use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput};
use ava_avm::txs::executor::semantic::Utxo;
use ava_avm::txs::{BaseTx, CreateAssetTx, FxCredential, InitialState, Tx, UnsignedTx};
use ava_avm::vm::AvmVm;
use ava_database::{DynDatabase, MemDb};
use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput};
use ava_snow::ChainContext;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::vm::Vm;

const NETWORK_ID: u32 = 10;
/// The blockchain id of this X-Chain (matches `chain_ctx().chain_id`).
fn chain_id() -> Id {
    Id::from([0x05; 32])
}

/// The (arbitrary) stop-vertex id the genesis block parents off (specs 09 §1).
const STOP_VERTEX: [u8; 32] = [0x07; 32];
/// The genesis Unix timestamp encoded into the synthetic genesis bytes.
const GENESIS_TS: u64 = 1_000_000;

fn addr() -> ShortId {
    ShortId::from([0xab; 20])
}

fn owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![addr()])
}

/// A no-op [`AppSender`] for the `initialize` call.
#[derive(Debug, Default)]
struct NoopAppSender;

#[async_trait]
impl AppSender for NoopAppSender {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        _nodes: &HashSet<NodeId>,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
        Ok(())
    }
    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
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
        _config: SendConfig,
        _bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
        Ok(())
    }
}

fn chain_ctx() -> Arc<ChainContext> {
    Arc::new(ChainContext {
        network_id: NETWORK_ID,
        subnet_id: Id::EMPTY,
        chain_id: chain_id(),
        node_id: NodeId::default(),
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(1),
        x_chain_id: chain_id(),
        c_chain_id: Id::EMPTY,
        avax_asset_id: Id::EMPTY,
        chain_data_dir: std::path::PathBuf::new(),
    })
}

/// A `CreateAssetTx` seeding the asset the conformance UTXOs belong to.
fn create_asset_tx() -> Tx {
    let ca = CreateAssetTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: Vec::new(),
            ins: Vec::new(),
            memo: Vec::new(),
        }),
        name: "Asset".to_string(),
        symbol: "MYA".to_string(),
        denomination: 8,
        states: vec![InitialState::new(
            0,
            vec![Output::SecpTransfer(TransferOutput::new(1, owners()))],
        )],
    };
    let mut tx = Tx::new(UnsignedTx::CreateAsset(ca));
    tx.initialize(Codec()).expect("initialize create-asset");
    tx
}

fn transfer_output(asset_id: Id, amt: u64) -> TransferableOutput {
    TransferableOutput {
        asset_id,
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    }
}

fn transfer_input(tx_id: Id, idx: u32, asset_id: Id, amt: u64) -> TransferableInput {
    TransferableInput {
        tx_id,
        output_index: idx,
        asset_id,
        r#in: Input::SecpTransfer(TransferInput::new(amt, vec![0])),
    }
}

/// The id of a synthetic seeded UTXO's producing tx (first byte = `tx_byte`).
fn seeded_tx_id(tx_byte: u8) -> Id {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    Id::from(tx_id)
}

fn secp_credential() -> FxCredential {
    FxCredential::new(Id::EMPTY, SecpCredential::new(vec![[0u8; 65]]))
}

fn utxo_bytes(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> (Id, Vec<u8>) {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    let utxo = Utxo {
        tx_id: Id::from(tx_id),
        output_index: idx,
        asset_id,
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    };
    (utxo.input_id(), utxo.marshal().expect("marshal utxo"))
}

/// A signed `BaseTx` consuming the UTXO at (`in_tx_id`, index 0) holding `amt`
/// and producing a single output of the same amount (zero fee) at its own
/// index 0 — so its output UTXO chains into the next spend.
fn base_tx(in_tx_id: Id, asset_id: Id, amt: u64) -> Tx {
    let mut tx = Tx::new(UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![transfer_output(asset_id, amt)],
        ins: vec![transfer_input(in_tx_id, 0, asset_id, amt)],
        memo: Vec::new(),
    })));
    tx.creds = vec![secp_credential()];
    tx.initialize(Codec()).expect("initialize base tx");
    tx
}

/// Builds the minimal synthetic genesis bytes the VM's `initialize` parses:
/// the 32-byte stop-vertex id followed by the 8-byte big-endian Unix timestamp.
fn genesis_bytes() -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(&STOP_VERTEX);
    out.extend_from_slice(&GENESIS_TS.to_be_bytes());
    out
}

/// Initializes a fully-wired [`AvmVm`] from the synthetic genesis, seeds the
/// state with a `CreateAssetTx` + a single spendable UTXO (`U0`), and pre-loads
/// the mempool with a CHAIN of two `BaseTx`es (`tx1` spends `U0` → `U1`; `tx2`
/// spends `U1`). With Go-parity multi-tx packing, the genesis-diff build packs
/// only `tx1` (`U1` does not exist yet) and the h2-child build over h1's diff
/// then packs `tx2` — so every `build_block` the battery issues succeeds.
async fn init_avm_vm(token: &CancellationToken) -> AvmVm {
    let ca = create_asset_tx();
    let asset_id = ca.id();
    // The single seeded UTXO U0 (2000 of the asset), produced by synthetic tx 0xb1.
    let (utxo_id0, utxo0) = utxo_bytes(0xb1, 0, asset_id, 2000);

    let mut vm = AvmVm::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    // A zero-fee config keeps the battery txs balanced without a fee UTXO.
    let config_bytes = serde_json::to_vec(&Config {
        tx_fee: 0,
        create_asset_tx_fee: 0,
    })
    .expect("config bytes");

    vm.initialize(
        token,
        chain_ctx(),
        db,
        &genesis_bytes(),
        b"",
        &config_bytes,
        Vec::new(),
        Arc::new(NoopAppSender),
    )
    .await
    .expect("initialize");

    // Seed the genesis-state UTXO set + asset tx (the genesis-asset alloc is the
    // M8/ava-genesis follow-up — this test seeds them directly).
    vm.seed_genesis_state(|s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id0, utxo0);
    })
    .expect("seed genesis state");

    // Pre-load the mempool with a CHAIN of two spends: tx1 consumes U0 and
    // produces U1 (its own output index 0); tx2 consumes U1. tx2 is enqueued
    // BEFORE tx1 so that, building over the genesis diff (FIFO order), the
    // builder hits tx2 first — U1 does not exist yet, so tx2 is dropped (left in
    // the pool) — then packs tx1, producing U1. So h1 carries only tx1 and the
    // builder removes only tx1 from the pool. The h2-child build over h1's
    // verified-but-unaccepted diff now finds U1 and packs tx2.
    //
    // (Were tx1 enqueued first, the builder would pack BOTH into h1 in one run —
    // the running diff already holds tx1's freshly produced U1 — and the child
    // build would find nothing. The ordering is what forces the two-block shape
    // while keeping genuine Go-parity multi-tx packing.)
    let tx1 = base_tx(seeded_tx_id(0xb1), asset_id, 2000);
    let tx2 = base_tx(tx1.id(), asset_id, 2000);
    vm.mempool_add(tx2).expect("add tx2");
    vm.mempool_add(tx1).expect("add tx1");

    vm
}

// The macro expands inside its own `mod vm_conformance`, so the closure body
// reaches the crate-root helper through `super::`.
ava_vm::vm_conformance!(|token: ::tokio_util::sync::CancellationToken| async move {
    super::init_avm_vm(&token).await
});
