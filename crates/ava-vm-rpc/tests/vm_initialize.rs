// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! End-to-end `VM.Initialize` over the v45 reverse-dial proxy callbacks
//! (specs 07 §5.1–§5.4; plan M9.10 + M9.11).
//!
//! `rust_host_initializes_rust_guest` drives the full host-driven Initialize
//! flow in-process: the host [`RpcChainVm`] stands up the `proto/rpcdb`
//! `Database` server (`db_server_addr`) over a host `MemDb` plus the callback
//! bundle server (`server_addr`: appsender) over a recording `AppSender`,
//! encodes the [`ChainContext`] into `InitializeRequest`, and sends
//! `VM.Initialize`. The guest reads the two addrs, dials them back into the
//! six-proxy bundle (here `RpcDatabase` + `RpcAppSender`), and calls the inner
//! VM's `initialize` with the proxied handles. The inner `DbProbeVm` does a
//! `put`/`get` round-trip over the **proxied** db at `initialize`, so a passing
//! test proves the db callback server was reached across the wire. The test
//! then drives one build→verify→accept→last_accepted cycle to prove the
//! post-init path still works.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_database::{DynDatabase, MemDb};
use ava_snow::{ChainContext, EngineState, Result as SnowResult};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;

use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::block::{Block, ChainVm};
use ava_vm::connector::Connector;
use ava_vm::error::{Error as VmErr, Result as VmResult};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{HttpHandler, Vm, VmEvent};

use ava_vm_rpc::host::RpcChainVm;
use ava_vm_rpc::{DEFAULT_HANDSHAKE_TIMEOUT, guest};

// Pulled in by `tonic-build`/`tonic` transitively; referenced so the test
// binary's `unused_crate_dependencies` lint stays quiet.
use {tokio_stream as _, tonic as _};

/// A minimal in-memory block (`parent ++ be64(height) ++ payload`).
#[derive(Debug)]
struct ProbeBlock {
    id: Id,
    parent: Id,
    height: u64,
    bytes: Vec<u8>,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    blocks: HashMap<Id, Arc<ProbeBlock>>,
    last_accepted: Id,
    preference: Id,
    height_index: std::collections::BTreeMap<u64, Id>,
}

impl ProbeBlock {
    fn encode(parent: Id, height: u64, payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::with_capacity(40 + payload.len());
        b.extend_from_slice(&parent.to_bytes());
        b.extend_from_slice(&height.to_be_bytes());
        b.extend_from_slice(payload);
        b
    }

    fn decode(bytes: &[u8]) -> (Id, u64) {
        let mut parent = [0u8; 32];
        parent.copy_from_slice(&bytes[..32]);
        let mut h = [0u8; 8];
        h.copy_from_slice(&bytes[32..40]);
        (Id::from(parent), u64::from_be_bytes(h))
    }

    fn derive_id(bytes: &[u8]) -> Id {
        Id::from(ava_crypto::hashing::sha256(bytes))
    }
}

#[async_trait]
impl Block for ProbeBlock {
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

/// A test VM that, at `initialize`, does a `put`/`get` round-trip over the
/// **proxied** db (proving the db callback server is reached), then seeds a
/// genesis block. The synchronous `DynDatabase` blocks on its own runtime, so
/// the db work runs inside `spawn_blocking` (off the async runtime context),
/// mirroring how a real VM's blocking storage work runs at the call site.
#[derive(Debug)]
struct DbProbeVm {
    inner: Arc<Mutex<Inner>>,
    next_payload: AtomicU64,
    /// Set to the value read back from the proxied db at `initialize`.
    db_readback: Arc<Mutex<Option<Vec<u8>>>>,
}

impl DbProbeVm {
    fn new(db_readback: Arc<Mutex<Option<Vec<u8>>>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            next_payload: AtomicU64::new(0),
            db_readback,
        }
    }

    fn register(&self, parent: Id, height: u64, payload: &[u8]) -> Arc<ProbeBlock> {
        let bytes = ProbeBlock::encode(parent, height, payload);
        let id = ProbeBlock::derive_id(&bytes);
        let blk = Arc::new(ProbeBlock {
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
impl AppHandler for DbProbeVm {
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
impl HealthCheck for DbProbeVm {
    async fn health_check(&self, _t: &CancellationToken) -> VmResult<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl Connector for DbProbeVm {
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
impl Vm for DbProbeVm {
    async fn initialize(
        &mut self,
        _token: &CancellationToken,
        _chain_ctx: Arc<ChainContext>,
        db: Arc<dyn DynDatabase>,
        _genesis_bytes: &[u8],
        _upgrade_bytes: &[u8],
        _config_bytes: &[u8],
        _fxs: Vec<Fx>,
        _app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        // Round-trip over the proxied db. The DynDatabase is synchronous (it
        // `block_on`s its own runtime), so run it on a blocking thread (off the
        // async runtime context) — exactly as a real VM's storage work runs.
        let readback = tokio::task::spawn_blocking(move || {
            db.put(b"probe-key", b"probe-val")
                .map_err(|_| VmErr::InvalidComponent("db put failed"))?;
            db.get(b"probe-key")
                .map_err(|_| VmErr::InvalidComponent("db get failed"))
        })
        .await
        .map_err(|_| VmErr::InvalidComponent("blocking db task join failed"))??;
        *self.db_readback.lock() = Some(readback);

        // Seed a genesis block (height 0) as the last accepted block.
        let genesis = self.register(Id::EMPTY, 0, b"genesis");
        {
            let mut inner = self.inner.lock();
            inner.last_accepted = genesis.id();
            inner.preference = genesis.id();
            inner.height_index.insert(0, genesis.id());
        }
        Ok(())
    }

    async fn set_state(&mut self, _t: &CancellationToken, _s: EngineState) -> VmResult<()> {
        Ok(())
    }
    async fn shutdown(&mut self, _t: &CancellationToken) -> VmResult<()> {
        Ok(())
    }
    async fn version(&self, _t: &CancellationToken) -> VmResult<String> {
        Ok("dbprobevm/0.0.0".to_string())
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
impl ChainVm for DbProbeVm {
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
        let (parent, height) = ProbeBlock::decode(bytes);
        let id = ProbeBlock::derive_id(bytes);
        let blk = Arc::new(ProbeBlock {
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

/// A recording host-side `AppSender` (served at `server_addr`) — proves the
/// callback bundle server is reachable, even though `DbProbeVm` does not call
/// it in this test.
#[derive(Default)]
struct RecordingAppSender {
    requests: Mutex<Vec<u32>>,
}

#[async_trait]
impl AppSender for RecordingAppSender {
    async fn send_app_request(
        &self,
        _t: &CancellationToken,
        _nodes: &HashSet<NodeId>,
        request_id: u32,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        self.requests.lock().push(request_id);
        Ok(())
    }
    async fn send_app_response(
        &self,
        _t: &CancellationToken,
        _n: NodeId,
        _r: u32,
        _b: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn send_app_error(
        &self,
        _t: &CancellationToken,
        _n: NodeId,
        _r: u32,
        _c: i32,
        _m: &str,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn send_app_gossip(
        &self,
        _t: &CancellationToken,
        _cfg: SendConfig,
        _b: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_host_initializes_rust_guest() {
    let token = CancellationToken::new();

    // The guest's db-readback slot, observed host-side after Initialize.
    let db_readback = Arc::new(Mutex::new(None));
    let db_readback_guest = Arc::clone(&db_readback);

    // The launcher spins up an in-process Rust guest wrapping a DbProbeVm that
    // is NOT yet initialized — the host drives VM.Initialize over the wire.
    let host = RpcChainVm::start(&token, DEFAULT_HANDSHAKE_TIMEOUT, move |engine_addr| {
        let engine_addr = engine_addr.to_string();
        let db_readback_guest = Arc::clone(&db_readback_guest);
        let token = CancellationToken::new();
        tokio::spawn(async move {
            let vm = DbProbeVm::new(db_readback_guest);
            guest::serve_with_addr(vm, &engine_addr, &token)
                .await
                .expect("guest serve");
        });
    })
    .await
    .expect("handshake + dial VM");

    let mut host = host;

    // The host-side db + app_sender that the host serves to the guest.
    let host_db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let app_sender: Arc<dyn AppSender> = Arc::new(RecordingAppSender::default());
    let chain_ctx = ava_vm::testutil::test_chain_context();

    // Drive VM.Initialize over the wire.
    host.initialize(
        &token,
        chain_ctx,
        Arc::clone(&host_db),
        b"genesis",
        b"",
        b"",
        Vec::new(),
        app_sender,
    )
    .await
    .expect("host VM.Initialize over the wire");

    // The guest's db round-trip went through the proxied db callback server,
    // which is backed by the host's MemDb — so the host can observe the write.
    let host_seen = tokio::task::spawn_blocking(move || host_db.get(b"probe-key").ok())
        .await
        .expect("blocking host db read");
    assert_eq!(
        host_seen,
        Some(b"probe-val".to_vec()),
        "guest's proxied db.put landed on the host-served MemDb"
    );
    assert_eq!(
        db_readback.lock().clone(),
        Some(b"probe-val".to_vec()),
        "guest read back its own write over the proxied db"
    );

    // After Initialize, last_accepted is the genesis the guest derived.
    let genesis = host.last_accepted(&token).await.expect("last_accepted");
    assert_ne!(genesis, Id::EMPTY, "genesis seeded by VM.Initialize");

    // Drive build -> verify -> accept -> last_accepted to prove the post-init
    // path still works.
    let blk = host.build_block(&token).await.expect("build_block");
    assert_eq!(blk.parent(), genesis, "built on the last accepted block");
    assert_eq!(blk.height(), 1, "child of genesis is at height 1");
    blk.verify(&token).await.expect("verify");
    blk.accept(&token).await.expect("accept");
    let last = host.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(
        last,
        blk.id(),
        "accept advances last_accepted over the wire"
    );
}
