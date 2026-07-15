// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::x_genesis_block` — the local-network X-Chain **genesis Snowman
//! block id** golden (M9.15 rung 4, genesis identity parity).
//!
//! On a fresh network whose `Upgrades.CortinaXChainStopVertexID` is empty
//! (local/custom networks), Go does NOT linearize off the empty id: the
//! avalanche bootstrapper builds a **stop vertex over the empty DAG edge**
//! (`snow/engine/avalanche/bootstrap/bootstrapper.go` "If a stop vertex isn't
//! well known, treat the current state as the final DAG state" →
//! `vertex.BuildStopVertex(chainID, height=0, parentIDs=[])`) and the height-0
//! X genesis `StandardBlock` uses **that vertex's id** as its parent. The block
//! id therefore depends only on the X blockchain id + `CortinaTime` — not on
//! the genesis assets.
//!
//! Golden provenance (Go oracle `avalanchego@96897293a2`):
//! * live run-7 (mixed_network, 2026-07-15): every Go validator's X-Chain
//!   `lastAcceptedID` is `2R2UY2pZMQr8nR9ywCdqn97Lp5a6hceqtLXkag6vH7KQSVvmst`
//!   — see go1/logs/X.log `starting bootstrapper {"lastAcceptedID":
//!   "2R2UY2pZ…", "lastAcceptedHeight": 0}`;
//! * re-confirmed against a solo `avalanchego --network-id=local` node
//!   (same binary) during this fix.
//!
//! The local X blockchain id `2eNy1mUFdmaxXNj1eQHUe7Np4gju9sJsEtWQ4MX3ToiNKuADed`
//! is itself pinned in `ava-genesis/tests/golden_genesis_block_id.rs`.

#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_avm::genesis::{Genesis, GenesisAsset};
use ava_avm::txs::codec::GenesisCodec;
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
use ava_vm::vm::Vm;

/// The local network id (12345) — its upgrade config has
/// `cortina_x_chain_stop_vertex_id == Id::EMPTY` (fresh-network branch).
const NETWORK_ID: u32 = 12345;

/// The local X blockchain id (`ava-genesis` golden table, specs 23 §7).
const LOCAL_X_CHAIN_ID: &str = "2eNy1mUFdmaxXNj1eQHUe7Np4gju9sJsEtWQ4MX3ToiNKuADed";

/// The Go-live local X genesis Snowman block id (provenance above).
const GO_X_GENESIS_BLOCK_ID: &str = "2R2UY2pZMQr8nR9ywCdqn97Lp5a6hceqtLXkag6vH7KQSVvmst";

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

fn x_chain_id() -> Id {
    LOCAL_X_CHAIN_ID.parse().expect("cb58 chain id")
}

/// A minimal valid genesis (one AVAX asset). The X genesis Snowman block id is
/// independent of the genesis assets (the block carries no transactions), so a
/// synthetic asset list exercises the identical production path.
fn genesis_bytes_and_asset_id() -> (Vec<u8>, Id) {
    let g = Genesis {
        txs: vec![GenesisAsset {
            alias: "AVAX".to_string(),
            tx: CreateAssetTx {
                base: BaseTx::new(AvaxBaseTx {
                    network_id: NETWORK_ID,
                    blockchain_id: x_chain_id(),
                    outs: Vec::new(),
                    ins: Vec::new(),
                    memo: Vec::new(),
                }),
                name: "Avalanche".to_string(),
                symbol: "AVAX".to_string(),
                denomination: 9,
                states: vec![InitialState::new(
                    0,
                    vec![Output::SecpTransfer(TransferOutput::new(
                        1_000_000,
                        OutputOwners::new(0, 1, vec![ShortId::from([0xab; 20])]),
                    ))],
                )],
            },
        }],
    };
    let bytes = g.marshal().expect("Genesis::marshal");
    let mut tx = Tx::new(UnsignedTx::CreateAsset(g.txs[0].tx.clone()));
    tx.initialize(GenesisCodec()).expect("initialize asset tx");
    (bytes, tx.id())
}

/// The production `AvmVm::initialize` path on the local network must root the
/// linearized chain at the Go stop vertex — its height-0 genesis Snowman block
/// id must equal the Go network's (`2R2UY2pZ…`), or a mixed Rust/Go local
/// network forks at genesis (M9.15 rung 4).
#[tokio::test]
async fn local_x_genesis_block_id_matches_go() {
    let token = CancellationToken::new();
    let (genesis_bytes, avax_asset_id) = genesis_bytes_and_asset_id();

    let ctx = Arc::new(ChainContext {
        network_id: NETWORK_ID,
        subnet_id: Id::EMPTY,
        chain_id: x_chain_id(),
        node_id: NodeId::default(),
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(NETWORK_ID),
        x_chain_id: x_chain_id(),
        c_chain_id: Id::EMPTY,
        avax_asset_id,
        chain_data_dir: std::path::PathBuf::new(),
    });

    let mut vm = AvmVm::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    vm.initialize(
        &token,
        ctx,
        db,
        &genesis_bytes,
        b"",
        b"",
        Vec::new(),
        Arc::new(NoopAppSender),
    )
    .await
    .expect("AvmVm::initialize");

    let last = ava_vm::block::ChainVm::last_accepted(&vm, &token)
        .await
        .expect("last_accepted");
    assert_eq!(
        last.to_string(),
        GO_X_GENESIS_BLOCK_ID,
        "local X genesis Snowman block id vs Go oracle (96897293a2)"
    );
}
