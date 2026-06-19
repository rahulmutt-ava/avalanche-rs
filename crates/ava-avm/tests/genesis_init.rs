// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `AvmVm::initialize` genesis seeding (M5.f4, specs 09 §1).
//!
//! Ports Go `vms/avm/vm.go`'s `initGenesis` + `Linearize`: `initialize` decodes
//! the real Go-format genesis bytes (a `Genesis{Txs []*GenesisAsset}` list),
//! builds + initializes a `CreateAssetTx` per asset, records the alias → asset id,
//! and seeds the produced UTXOs + the asset tx into state. The genesis Snowman
//! block's stop-vertex id + timestamp come from the upgrade config
//! (`Upgrades.CortinaXChainStopVertexID` / `CortinaTime`), NOT the genesis bytes.
//!
//! This test builds real genesis bytes via `Genesis::marshal`, drives
//! `AvmVm::initialize`, and asserts:
//!   * the produced AVAX UTXO(s) are readable via the VM state seam,
//!   * the `"AVAX"` alias resolves to the index-0 tx id,
//!   * that id == `ctx.avax_asset_id`,
//!   * the genesis block timestamp == `cortina_time` for the local network.

#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::indexing_slicing)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_avm::block::Block;
use ava_avm::genesis::{Genesis, GenesisAsset};
use ava_avm::state::ReadOnlyChain;
use ava_avm::txs::codec::{Codec, GenesisCodec};
use ava_avm::txs::components::{AvaxBaseTx, Output};
use ava_avm::txs::{BaseTx, CreateAssetTx, InitialState, Tx, UnsignedTx};
use ava_avm::vm::AvmVm;
use ava_database::{DynDatabase, MemDb};
use ava_secp256k1fx::{OutputOwners, TransferOutput};
use ava_snow::ChainContext;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::block::ChainVm;
use ava_vm::vm::Vm;

/// The local/custom network id (the default upgrade-config branch).
const NETWORK_ID: u32 = 12345;

fn chain_id() -> Id {
    Id::from([0x05; 32])
}

fn addr() -> ShortId {
    ShortId::from([0xab; 20])
}

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

/// A genesis `CreateAssetTx`: an empty base (genesis assets MUST have empty base
/// outs — the value lives in `states`) + a single secp `TransferOutput` initial
/// state (so the asset seeds one spendable UTXO at output index 0).
fn genesis_create_asset(name: &str, symbol: &str) -> CreateAssetTx {
    CreateAssetTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: Vec::new(),
            ins: Vec::new(),
            memo: Vec::new(),
        }),
        name: name.to_string(),
        symbol: symbol.to_string(),
        denomination: 9,
        states: vec![InitialState::new(
            0,
            vec![Output::SecpTransfer(TransferOutput::new(
                1_000_000,
                OutputOwners::new(0, 1, vec![addr()]),
            ))],
        )],
    }
}

/// Real Go-format genesis bytes: a `Genesis` with two assets (AVAX at index 0).
fn genesis() -> Genesis {
    Genesis {
        txs: vec![
            GenesisAsset {
                alias: "AVAX".to_string(),
                tx: genesis_create_asset("Avalanche", "AVAX"),
            },
            GenesisAsset {
                alias: "OTHER".to_string(),
                tx: genesis_create_asset("Other Asset", "OTH"),
            },
        ],
    }
}

/// The genesis asset id (the tx id of the asset at `index`), computed the way the
/// node does: `Tx::new(UnsignedTx::CreateAsset(..)).initialize(GenesisCodec())`.
fn genesis_asset_id(g: &Genesis, index: usize) -> Id {
    let mut tx = Tx::new(UnsignedTx::CreateAsset(g.txs[index].tx.clone()));
    tx.initialize(GenesisCodec()).expect("initialize asset tx");
    tx.id()
}

fn chain_ctx(avax_asset_id: Id) -> Arc<ChainContext> {
    Arc::new(ChainContext {
        network_id: NETWORK_ID,
        subnet_id: Id::EMPTY,
        chain_id: chain_id(),
        node_id: NodeId::default(),
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(NETWORK_ID),
        x_chain_id: chain_id(),
        c_chain_id: Id::EMPTY,
        avax_asset_id,
        chain_data_dir: std::path::PathBuf::new(),
    })
}

#[tokio::test]
async fn initialize_seeds_genesis_assets_and_cortina_stop_vertex() {
    let token = CancellationToken::new();
    let g = genesis();
    let genesis_bytes = g.marshal().expect("Genesis::marshal");

    // The node derives ctx.avax_asset_id from the index-0 genesis asset.
    let avax_asset_id = genesis_asset_id(&g, 0);

    let mut vm = AvmVm::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    vm.initialize(
        &token,
        chain_ctx(avax_asset_id),
        db,
        &genesis_bytes,
        b"",
        b"",
        Vec::new(),
        Arc::new(NoopAppSender),
    )
    .await
    .expect("initialize");

    // Every produced UTXO of every genesis asset is readable from state.
    for asset in &g.txs {
        let mut tx = Tx::new(UnsignedTx::CreateAsset(asset.tx.clone()));
        tx.initialize(GenesisCodec()).expect("initialize asset tx");
        let tx_id = tx.id();
        let utxos = tx.unsigned.utxos(tx_id);
        assert!(
            !utxos.is_empty(),
            "genesis asset {} seeds a UTXO",
            asset.alias
        );
        for utxo in utxos {
            let id = utxo.input_id();
            let stored = vm
                .with_state(|s| s.get_utxo(id))
                .expect("with_state")
                .unwrap_or_else(|_| panic!("genesis UTXO {id} present"));
            assert_eq!(
                stored,
                utxo.marshal().expect("marshal utxo"),
                "stored UTXO bytes match the produced UTXO"
            );
        }
        // The asset tx itself is stored under its id.
        let stored_tx = vm
            .with_state(|s| s.get_tx(tx_id))
            .expect("with_state")
            .expect("genesis asset tx stored");
        assert_eq!(stored_tx, tx.bytes(), "stored asset tx bytes match");
    }

    // The "AVAX" alias resolves to the index-0 asset id, == ctx.avax_asset_id.
    let avax_id = genesis_asset_id(&g, 0);
    assert_eq!(
        vm.lookup_alias(avax_id),
        Some("AVAX"),
        "the AVAX alias resolves to the index-0 asset id"
    );
    assert_eq!(
        avax_id, avax_asset_id,
        "index-0 asset id == ctx.avax_asset_id"
    );

    // The genesis Snowman block timestamp == cortina_time for the local network.
    let cortina_time = ava_version::upgrade::get_config(NETWORK_ID).cortina_time;
    let cortina_secs = u64::try_from(cortina_time.timestamp()).unwrap_or(0);
    let last = vm.last_accepted(&token).await.expect("last_accepted");
    let blk_bytes = vm
        .with_state(|s| s.get_block(last))
        .expect("with_state")
        .expect("genesis block stored");
    let blk = Block::parse(Codec(), &blk_bytes).expect("parse genesis block");
    assert_eq!(blk.height(), 0, "genesis is height 0");
    assert_eq!(
        blk.timestamp(),
        cortina_secs,
        "genesis block timestamp == cortina_time"
    );
    // And the stored chain timestamp matches the same Unix-second value.
    let ts = vm
        .with_state(ReadOnlyChain::get_timestamp)
        .expect("with_state");
    assert_eq!(
        ts,
        UNIX_EPOCH + Duration::from_secs(cortina_secs),
        "chain timestamp == cortina_time"
    );
}
