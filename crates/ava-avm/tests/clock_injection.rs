// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `clock_injection::build_block_reads_injected_clock` (specs 24 hazard #5):
//! an [`AvmVm`] constructed via [`AvmVm::with_clock`] with a `MockClock` pinned
//! strictly past the genesis/parent time builds a height-1 block whose
//! timestamp equals the pinned clock time — proving `build_block` stamps the
//! INJECTED clock, NOT the wall clock and NOT the parent timestamp.
//!
//! The block time is `max(parent_time, now)` (specs 09 §7.1). The genesis ts is
//! `GENESIS_TS` (1_000_000); the clock is pinned at `PINNED` (2_000_000),
//! strictly past the parent, so the resolved block time is exactly the pinned
//! `now`. A wall-clock read (≈1.7e9) or a parent read (1_000_000) would both
//! fail the assertion.
//!
//! Mirrors `ava-platformvm`'s `vm::clock_injection::build_block_reads_injected_clock`
//! and reuses the funded single-UTXO build harness from `vm_conformance.rs` (the
//! avm builder verifies + executes each candidate, so the packed tx must be a
//! genuinely spendable `BaseTx`, not an input-free synthetic).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

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
use ava_utils::clock::MockClock;
use ava_vm::ChainVm;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::vm::Vm;

const NETWORK_ID: u32 = 10;
/// The genesis Unix timestamp encoded into the synthetic genesis bytes.
const GENESIS_TS: u64 = 1_000_000;
/// The pinned clock time — strictly past `GENESIS_TS` so `max(parent, now)` == now.
const PINNED: u64 = 2_000_000;

fn chain_id() -> Id {
    Id::from([0x05; 32])
}

fn owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![ShortId::from([0xab; 20])])
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

/// A `CreateAssetTx` seeding the asset the spendable UTXO belongs to.
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

fn seeded_tx_id(tx_byte: u8) -> Id {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    Id::from(tx_id)
}

fn utxo_bytes(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> (Id, Vec<u8>) {
    let utxo = Utxo {
        tx_id: seeded_tx_id(tx_byte),
        output_index: idx,
        asset_id,
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    };
    (utxo.input_id(), utxo.marshal().expect("marshal utxo"))
}

/// A signed `BaseTx` consuming the UTXO at (`in_tx_id`, index 0) holding `amt`
/// and producing a single output of the same amount (zero fee).
fn base_tx(in_tx_id: Id, asset_id: Id, amt: u64) -> Tx {
    let mut tx = Tx::new(UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![TransferableOutput {
            asset_id,
            out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
        }],
        ins: vec![TransferableInput {
            tx_id: in_tx_id,
            output_index: 0,
            asset_id,
            r#in: Input::SecpTransfer(TransferInput::new(amt, vec![0])),
        }],
        memo: Vec::new(),
    })));
    tx.creds = vec![FxCredential::new(
        Id::EMPTY,
        SecpCredential::new(vec![[0u8; 65]]),
    )];
    tx.initialize(Codec()).expect("initialize base tx");
    tx
}

/// The minimal synthetic genesis bytes: 32-byte stop-vertex id + 8-byte ts.
fn genesis_bytes() -> Vec<u8> {
    let mut out = vec![0x07; 32];
    out.extend_from_slice(&GENESIS_TS.to_be_bytes());
    out
}

#[tokio::test]
async fn build_block_reads_injected_clock() {
    let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(PINNED));

    let ca = create_asset_tx();
    let asset_id = ca.id();
    // A single spendable UTXO U0 (2000 of the asset), produced by synthetic tx 0xb1.
    let (utxo_id0, utxo0) = utxo_bytes(0xb1, 0, asset_id, 2000);

    // Inject the pinned clock via the determinism seam (specs 24 hazard #5).
    let mut vm = AvmVm::with_clock(Arc::new(clock));
    let token = CancellationToken::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    // A zero-fee config keeps the spend balanced without a separate fee UTXO.
    let config_bytes = serde_json::to_vec(&Config {
        tx_fee: 0,
        create_asset_tx_fee: 0,
    })
    .expect("config bytes");

    vm.initialize(
        &token,
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

    // Seed the genesis-state UTXO set + asset tx (genesis-asset alloc is M8).
    vm.seed_genesis_state(|s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id0, utxo0);
    })
    .expect("seed genesis state");

    // Admit a fully-funded spend so the builder verifies + packs it.
    vm.mempool_add(base_tx(seeded_tx_id(0xb1), asset_id, 2000))
        .expect("mempool add");

    // Build over genesis: a height-1 standard block stamping the chain time. The
    // built block's timestamp must equal the pinned clock — not the wall clock
    // and not the parent ts (GENESIS_TS).
    let built = vm.build_block(&token).await.expect("build block");
    assert_eq!(built.height(), 1, "build advances to height 1");

    let ts = built
        .timestamp()
        .duration_since(UNIX_EPOCH)
        .expect("post-epoch timestamp")
        .as_secs();
    assert_eq!(
        ts, PINNED,
        "build_block must stamp the INJECTED clock time, not the wall clock or parent ts (1_000_000)"
    );
}
