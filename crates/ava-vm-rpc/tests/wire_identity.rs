// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.13 — the four-way wire-identity matrix, **Rust⇄Rust offline arm**
//! (specs/07 §10 four-way matrix; specs/02 §6 golden, §11.3).
//!
//! Drives a FIXED `build→verify→accept→parse` sequence through the in-process
//! Rust host ([`RpcChainVm`]) ⇄ Rust guest ([`guest::serve_with_addr`]) pairing,
//! captures the **prost-encoded `proto/vm` request message bytes** the host puts
//! on the wire for each RPC, and asserts them byte-identical to committed
//! goldens under `tests/vectors/rpcchainvm/`. It also asserts the resulting block
//! bytes / IDs / last-accepted are the expected deterministic values.
//!
//! The point is to **lock the wire format deterministically**: the same goldens
//! are the cross-language oracle the three Go-involving pairings of the four-way
//! matrix (Rust-host⇄Go-guest, Go-host⇄Rust-guest, Go⇄Go) diff against in the
//! gated live arm (`tests/differential/tests/plugin_wire_matrix.rs`).
//!
//! ## How the wire bytes are captured (direct prost-encode)
//! tonic 0.12 frames each unary request body as `prost::Message::encode`d bytes
//! (no envelope beyond the 5-byte gRPC length-prefix, which we strip by encoding
//! the message itself). A tonic *interceptor* only sees request metadata, not the
//! message body, so to capture the body deterministically we reconstruct the
//! exact request struct the host code in `src/host/{mod,block}.rs` sends and
//! `encode_to_vec()` it — the same `prost::Message` impl tonic's codec uses. The
//! end-to-end host⇄guest drive in the same test proves the host *actually* sends
//! these requests (the block bytes/ids/last-accepted it returns are derived from
//! that real round-trip); the encoded structs lock their wire shape.
//!
//! Regenerate the goldens (after an intentional wire change) with:
//! ```text
//! REGEN_WIRE_GOLDENS=1 cargo nextest run -p ava-vm-rpc -E 'test(wire_identity)'
//! ```
//! then `git add crates/ava-vm-rpc/tests/vectors/rpcchainvm/` and review the diff.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use parking_lot::Mutex;
use prost::Message as _;
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
use ava_vm_rpc::pb::vm::{self};
use ava_vm_rpc::pb::vm::{
    BlockAcceptRequest, BlockVerifyRequest, BuildBlockRequest, ParseBlockRequest,
    SetPreferenceRequest, SetStateRequest,
};
use ava_vm_rpc::{DEFAULT_HANDSHAKE_TIMEOUT, guest};

// Pulled in transitively; referenced so `unused_crate_dependencies` stays quiet.
use {anyhow as _, criterion as _, serde_json as _, thiserror as _, tokio_stream as _, tonic as _};

// ---------------------------------------------------------------------------
// Deterministic test VM (a trimmed copy of `vm_initialize.rs`'s ProbeBlock/Vm,
// minus the db round-trip — the wire-identity sequence does not exercise the db).
// ---------------------------------------------------------------------------

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
        let mut b = Vec::with_capacity(40usize.saturating_add(payload.len()));
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

struct ProbeVm {
    inner: Arc<Mutex<Inner>>,
    next_payload: AtomicU64,
}

impl ProbeVm {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            next_payload: AtomicU64::new(0),
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
impl AppHandler for ProbeVm {
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
impl HealthCheck for ProbeVm {
    async fn health_check(&self, _t: &CancellationToken) -> VmResult<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl Connector for ProbeVm {
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
impl Vm for ProbeVm {
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
        // The guest-side `RpcDatabase` (proxying the host db over the wire) owns an
        // internal runtime that must NOT be dropped from within an async context,
        // so we move it onto a blocking thread to drop — exactly as a real VM's
        // storage handle is torn down off the async runtime.
        tokio::task::spawn_blocking(move || drop(db));
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
        Ok("probevm/0.0.0".to_string())
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
impl ChainVm for ProbeVm {
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

#[derive(Default)]
struct NoopAppSender;

#[async_trait]
impl AppSender for NoopAppSender {
    async fn send_app_request(
        &self,
        _t: &CancellationToken,
        _nodes: &HashSet<NodeId>,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
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

// ---------------------------------------------------------------------------
// Golden helpers.
// ---------------------------------------------------------------------------

/// The canonical golden directory: `crates/ava-vm-rpc/tests/vectors/rpcchainvm/`.
/// This is the single source of truth for the four-way matrix; the differential
/// gated live arm reads the same files via a relative path (it must not depend on
/// `ava-vm-rpc`). Each file is the raw prost-encoded `proto/vm` request message.
fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join("rpcchainvm")
}

/// Compare `actual` against the committed golden `name`, or (re)write it when
/// `REGEN_WIRE_GOLDENS=1`. A missing golden without the regen flag fails loudly.
fn assert_golden(name: &str, actual: &[u8]) {
    let path = golden_dir().join(name);
    if std::env::var("REGEN_WIRE_GOLDENS").as_deref() == Ok("1") {
        std::fs::create_dir_all(golden_dir()).expect("create golden dir");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "golden {} missing ({e}); regenerate with REGEN_WIRE_GOLDENS=1",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected.as_slice(),
        "proto/vm request wire bytes for {name} drifted from the committed golden \
         (regenerate with REGEN_WIRE_GOLDENS=1 if intentional)"
    );
}

fn id_bytes(id: Id) -> bytes::Bytes {
    bytes::Bytes::copy_from_slice(&id.to_bytes())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_rust_wire_identity_matrix() {
    let token = CancellationToken::new();

    // 1. Stand up the in-process Rust guest wrapping an uninitialized ProbeVm; the
    //    host drives the v45 handshake + VM.Initialize over the wire.
    let host = RpcChainVm::start(&token, DEFAULT_HANDSHAKE_TIMEOUT, move |engine_addr| {
        let engine_addr = engine_addr.to_string();
        let token = CancellationToken::new();
        tokio::spawn(async move {
            let vm = ProbeVm::new();
            guest::serve_with_addr(vm, &engine_addr, &token)
                .await
                .expect("guest serve");
        });
    })
    .await
    .expect("handshake + dial VM");
    let mut host = host;

    let host_db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let app_sender: Arc<dyn AppSender> = Arc::new(NoopAppSender);
    let chain_ctx = ava_vm::testutil::test_chain_context();

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

    // 2. The genesis the guest seeded, observed host-side. Deterministic:
    //    sha256(EMPTY ++ be64(0) ++ "genesis").
    let genesis = host.last_accepted(&token).await.expect("last_accepted");
    let genesis_bytes = ProbeBlock::encode(Id::EMPTY, 0, b"genesis");
    let genesis_id = ProbeBlock::derive_id(&genesis_bytes);
    assert_eq!(genesis, genesis_id, "genesis id is deterministic");

    // 3. Drive the FIXED sequence: set_preference -> build -> verify -> accept ->
    //    parse, and observe the resulting block identity.
    host.set_preference(&token, genesis)
        .await
        .expect("set_preference");

    let blk = host.build_block(&token).await.expect("build_block");
    // Deterministic block-1: parent=genesis, height=1, payload=be64(0).
    let blk1_bytes = ProbeBlock::encode(genesis, 1, &0u64.to_be_bytes());
    let blk1_id = ProbeBlock::derive_id(&blk1_bytes);
    assert_eq!(blk.id(), blk1_id, "built block id is deterministic");
    assert_eq!(blk.parent(), genesis, "built on genesis");
    assert_eq!(blk.height(), 1, "child of genesis at height 1");
    assert_eq!(
        blk.bytes(),
        blk1_bytes.as_slice(),
        "built block bytes stable"
    );

    blk.verify(&token).await.expect("verify");
    blk.accept(&token).await.expect("accept");
    let last = host.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(last, blk1_id, "accept advances last_accepted over the wire");

    let parsed = host
        .parse_block(&token, &blk1_bytes)
        .await
        .expect("parse_block");
    assert_eq!(parsed.id(), blk1_id, "parse round-trips the same id");
    assert_eq!(parsed.parent(), genesis, "parsed parent");
    assert_eq!(parsed.height(), 1, "parsed height");

    // 4. Lock the `proto/vm` request wire bytes. We reconstruct the exact request
    //    struct each host method sends (see `src/host/{mod,block}.rs`) and encode
    //    it with the same `prost::Message` impl tonic's unary codec uses. These
    //    are the bytes on the wire (the gRPC length-prefix is framing, stripped
    //    here so the golden is the pure message body). InitializeRequest is
    //    deliberately NOT goldened: it carries ephemeral callback addresses.

    // SetState(Unspecified): the post-init last-accepted probe + the seed probe
    // share one shape.
    assert_golden(
        "set_state_unspecified.bin",
        &SetStateRequest {
            state: vm::State::Unspecified as i32,
        }
        .encode_to_vec(),
    );

    // SetPreference(genesis).
    assert_golden(
        "set_preference.bin",
        &SetPreferenceRequest {
            id: id_bytes(genesis),
        }
        .encode_to_vec(),
    );

    // BuildBlock: no p-chain height on the plain Snowman path.
    assert_golden(
        "build_block.bin",
        &BuildBlockRequest {
            p_chain_height: None,
        }
        .encode_to_vec(),
    );

    // BlockVerify(block-1 bytes): no p-chain height (plain Snowman).
    assert_golden(
        "block_verify.bin",
        &BlockVerifyRequest {
            bytes: bytes::Bytes::copy_from_slice(&blk1_bytes),
            p_chain_height: None,
        }
        .encode_to_vec(),
    );

    // BlockAccept(block-1 id).
    assert_golden(
        "block_accept.bin",
        &BlockAcceptRequest {
            id: id_bytes(blk1_id),
        }
        .encode_to_vec(),
    );

    // ParseBlock(block-1 bytes).
    assert_golden(
        "parse_block.bin",
        &ParseBlockRequest {
            bytes: bytes::Bytes::copy_from_slice(&blk1_bytes),
        }
        .encode_to_vec(),
    );

    // 5. Block-identity goldens: lock the derived block bytes + ids the matrix
    //    asserts identical across all four pairings.
    assert_golden("genesis_id.bin", &genesis_id.to_bytes());
    assert_golden("block1_bytes.bin", &blk1_bytes);
    assert_golden("block1_id.bin", &blk1_id.to_bytes());
}
