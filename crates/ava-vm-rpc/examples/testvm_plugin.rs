// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Minimal Rust test-VM **plugin binary** for the rpcchainvm v45 guest protocol
//! (specs/07 §5.1/§5.3; plan/M9 task M9.3).
//!
//! This is the binary an external `rpcchainvm` host (a Go `avalanchego` node, or
//! a Rust [`ava_vm_rpc::host::RpcChainVm`]) spawns as a custom VM. On launch it
//! reads the host runtime address from `AVALANCHE_VM_RUNTIME_ENGINE_ADDR`
//! ([`ava_vm_rpc::ENGINE_ADDRESS_KEY`]), dials back, calls
//! `Runtime.Initialize(RPC_CHAIN_VM_PROTOCOL = 45, vm_addr)`, then serves the
//! `proto/vm` `VM` service + `grpc.health` on `vm_addr` until the host shuts it
//! down — exactly the guest half of the reverse-dial handshake.
//!
//! The VM itself ([`FixedGenesisVm`]) is intentionally trivial: it seeds a single
//! fixed genesis block at height 0 as the last-accepted block and can
//! build/parse/get linear child blocks. It carries no real state — the point of
//! this binary is to prove a Rust plugin completes the v45 handshake and serves
//! the `VM` service under a foreign host, not to execute chain logic. It mirrors
//! the in-process `DbProbeVm`/`ProbeBlock` pattern from
//! `tests/vm_initialize.rs`, minus the proxied-db round-trip.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_snow::{ChainContext, EngineState, Result as SnowResult};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;

use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{Block, ChainVm};
use ava_vm::connector::Connector;
use ava_vm::error::{Error as VmErr, Result as VmResult};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{HttpHandler, Vm, VmEvent};

use ava_vm_rpc::guest;

/// A minimal in-memory block (`parent ++ be64(height) ++ payload`).
#[derive(Debug)]
struct FixedBlock {
    id: Id,
    parent: Id,
    height: u64,
    bytes: Vec<u8>,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    blocks: HashMap<Id, Arc<FixedBlock>>,
    last_accepted: Id,
    preference: Id,
    height_index: BTreeMap<u64, Id>,
}

impl FixedBlock {
    fn encode(parent: Id, height: u64, payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::with_capacity(40usize.saturating_add(payload.len()));
        b.extend_from_slice(&parent.to_bytes());
        b.extend_from_slice(&height.to_be_bytes());
        b.extend_from_slice(payload);
        b
    }

    fn decode(bytes: &[u8]) -> Option<(Id, u64)> {
        if bytes.len() < 40 {
            return None;
        }
        let mut parent = [0u8; 32];
        parent.copy_from_slice(&bytes[..32]);
        let mut h = [0u8; 8];
        h.copy_from_slice(&bytes[32..40]);
        Some((Id::from(parent), u64::from_be_bytes(h)))
    }

    fn derive_id(bytes: &[u8]) -> Id {
        Id::from(ava_crypto::hashing::sha256(bytes))
    }
}

#[async_trait]
impl Block for FixedBlock {
    fn id(&self) -> Id {
        self.id
    }
    fn parent(&self) -> Id {
        self.parent
    }
    fn height(&self) -> u64 {
        self.height
    }
    fn timestamp(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    async fn verify(&self, _token: &CancellationToken) -> SnowResult<()> {
        Ok(())
    }
    async fn accept(&self, _token: &CancellationToken) -> SnowResult<()> {
        let mut inner = self.inner.lock();
        inner.last_accepted = self.id;
        inner.height_index.insert(self.height, self.id);
        Ok(())
    }
    async fn reject(&self, _token: &CancellationToken) -> SnowResult<()> {
        Ok(())
    }
}

/// A trivial test VM: at `initialize` it seeds a fixed genesis block (height 0)
/// as the last-accepted block; it can then build/parse/get linear child blocks.
#[derive(Debug, Default)]
struct FixedGenesisVm {
    inner: Arc<Mutex<Inner>>,
    next_payload: AtomicU64,
}

impl FixedGenesisVm {
    fn register(&self, parent: Id, height: u64, payload: &[u8]) -> Arc<FixedBlock> {
        let bytes = FixedBlock::encode(parent, height, payload);
        let id = FixedBlock::derive_id(&bytes);
        let blk = Arc::new(FixedBlock {
            id,
            parent,
            height,
            bytes,
            inner: Arc::clone(&self.inner),
        });
        self.inner
            .lock()
            .blocks
            .entry(id)
            .or_insert_with(|| Arc::clone(&blk));
        Arc::clone(&blk)
    }
}

#[async_trait]
impl AppHandler for FixedGenesisVm {
    async fn app_request(
        &mut self,
        _t: &CancellationToken,
        _n: NodeId,
        _r: u32,
        _d: Instant,
        _req: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }
    async fn app_request_failed(
        &mut self,
        _t: &CancellationToken,
        _n: NodeId,
        _r: u32,
        _e: AppError,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn app_response(
        &mut self,
        _t: &CancellationToken,
        _n: NodeId,
        _r: u32,
        _resp: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }
    async fn app_gossip(&mut self, _t: &CancellationToken, _n: NodeId, _m: &[u8]) -> VmResult<()> {
        Ok(())
    }
}

#[async_trait]
impl HealthCheck for FixedGenesisVm {
    async fn health_check(&self, _t: &CancellationToken) -> VmResult<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl Connector for FixedGenesisVm {
    async fn connected(
        &mut self,
        _t: &CancellationToken,
        _n: NodeId,
        _v: Application,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn disconnected(&mut self, _t: &CancellationToken, _n: NodeId) -> VmResult<()> {
        Ok(())
    }
}

#[async_trait]
impl Vm for FixedGenesisVm {
    async fn initialize(
        &mut self,
        _token: &CancellationToken,
        _chain_ctx: Arc<ChainContext>,
        _db: Arc<dyn DynDatabase>,
        _genesis_bytes: &[u8],
        _upgrade_bytes: &[u8],
        _config_bytes: &[u8],
        _fxs: Vec<Fx>,
        _app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        // Seed a fixed genesis block (height 0) as the last accepted block.
        let genesis = self.register(Id::EMPTY, 0, b"genesis");
        let mut inner = self.inner.lock();
        inner.last_accepted = genesis.id();
        inner.preference = genesis.id();
        inner.height_index.insert(0, genesis.id());
        Ok(())
    }

    async fn set_state(&mut self, _t: &CancellationToken, _s: EngineState) -> VmResult<()> {
        Ok(())
    }
    async fn shutdown(&mut self, _t: &CancellationToken) -> VmResult<()> {
        Ok(())
    }
    async fn version(&self, _t: &CancellationToken) -> VmResult<String> {
        Ok("testvm_plugin/0.0.0".to_string())
    }
    async fn create_handlers(
        &mut self,
        _t: &CancellationToken,
    ) -> VmResult<HashMap<String, HttpHandler>> {
        Ok(HashMap::new())
    }
    async fn new_http_handler(&mut self, _t: &CancellationToken) -> VmResult<Option<HttpHandler>> {
        Ok(None)
    }
    async fn wait_for_event(&self, _t: &CancellationToken) -> VmResult<VmEvent> {
        Ok(VmEvent::PendingTxs)
    }
}

#[async_trait]
impl ChainVm for FixedGenesisVm {
    async fn build_block(&mut self, _t: &CancellationToken) -> VmResult<Arc<dyn Block>> {
        let (parent, height) = {
            let inner = self.inner.lock();
            let parent = inner.preference;
            let ph = inner.blocks.get(&parent).map_or(0, |b| b.height());
            (parent, ph.saturating_add(1))
        };
        let payload = self
            .next_payload
            .fetch_add(1, Ordering::SeqCst)
            .to_be_bytes();
        let blk = self.register(parent, height, &payload);
        Ok(blk as Arc<dyn Block>)
    }
    async fn get_block(&self, _t: &CancellationToken, id: Id) -> VmResult<Arc<dyn Block>> {
        self.inner
            .lock()
            .blocks
            .get(&id)
            .map(|b| Arc::clone(b) as Arc<dyn Block>)
            .ok_or(VmErr::NotFound)
    }
    async fn parse_block(&self, _t: &CancellationToken, bytes: &[u8]) -> VmResult<Arc<dyn Block>> {
        let (parent, height) = FixedBlock::decode(bytes).ok_or(VmErr::InvalidComponent(
            "testvm_plugin: block bytes too short",
        ))?;
        let id = FixedBlock::derive_id(bytes);
        let blk = Arc::new(FixedBlock {
            id,
            parent,
            height,
            bytes: bytes.to_vec(),
            inner: Arc::clone(&self.inner),
        });
        self.inner
            .lock()
            .blocks
            .entry(id)
            .or_insert_with(|| Arc::clone(&blk));
        Ok(blk as Arc<dyn Block>)
    }
    async fn set_preference(&mut self, _t: &CancellationToken, id: Id) -> VmResult<()> {
        self.inner.lock().preference = id;
        Ok(())
    }
    async fn last_accepted(&self, _t: &CancellationToken) -> VmResult<Id> {
        Ok(self.inner.lock().last_accepted)
    }
    async fn get_block_id_at_height(&self, _t: &CancellationToken, height: u64) -> VmResult<Id> {
        self.inner
            .lock()
            .height_index
            .get(&height)
            .copied()
            .ok_or(VmErr::NotFound)
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // The host cancels us via the graceful-shutdown path inside `serve`; this
    // token is the plugin-local kill switch (unused on the normal path).
    let token = CancellationToken::new();
    let vm = FixedGenesisVm::default();

    // `serve` reads ENGINE_ADDRESS_KEY, dials the host Runtime, reports the v45
    // handshake, and serves VM + health until the host shuts us down. On any
    // failure (missing env / dial failure) exit non-zero so the host sees the
    // plugin fail fast.
    if let Err(err) = guest::serve(vm, &token).await {
        eprintln!("testvm_plugin: serve failed: {err}");
        std::process::exit(1);
    }
}
