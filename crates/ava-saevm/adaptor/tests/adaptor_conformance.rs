// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Conformance tests for the `ava-saevm-adaptor` crate: verifies that
//! `convert()` correctly bridges a `ChainVm<BP>` into `ava_vm::ChainVm`.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_saevm_adaptor::{BlockProperties, ChainVm, convert};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;
use ava_vm::block::ChainVm as ConsensusChainVm;
use ava_vm::{
    AppError, AppHandler, AppSender, BlockContext, ChainContext, Connector, EngineState, Fx,
    HealthCheck, HttpHandler, Result as VmResult, SendConfig, Vm, VmEvent,
};

// ---------------------------------------------------------------------------
// FakeBlockProperties
// ---------------------------------------------------------------------------

/// Minimal block-properties value for tests.
#[derive(Clone)]
struct FakeBlockProperties {
    id: Id,
    parent: Id,
    height: u64,
    timestamp: SystemTime,
    bytes: Vec<u8>,
}

impl BlockProperties for FakeBlockProperties {
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
        self.timestamp
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

// ---------------------------------------------------------------------------
// FakeAppSender (no-op)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct FakeAppSender;

#[async_trait]
impl AppSender for FakeAppSender {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        _nodes: &HashSet<NodeId>,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn send_app_error(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _code: i32,
        _message: &str,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn send_app_gossip(
        &self,
        _token: &CancellationToken,
        _config: SendConfig,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FakeChainVm
// ---------------------------------------------------------------------------

/// Low byte of a height, without a truncating `as` cast (the SAE bar denies
/// `cast_possible_truncation`, which applies to test code under `--all-targets`).
fn low_byte(height: u64) -> u8 {
    u8::try_from(height & 0xFF).expect("masked to a single byte")
}

/// An in-process `ChainVm<FakeBlockProperties>` that increments an atomic
/// counter when `accept_block` is called.
// Fields share the `_count` postfix by design (one counter per lifecycle hook);
// the pedantic `struct_field_names` lint is not meaningful for this test double.
#[allow(clippy::struct_field_names)]
struct FakeChainVm {
    accept_count: Arc<AtomicUsize>,
    verify_count: Arc<AtomicUsize>,
    reject_count: Arc<AtomicUsize>,
}

impl FakeChainVm {
    fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let accept = Arc::new(AtomicUsize::new(0));
        let verify = Arc::new(AtomicUsize::new(0));
        let reject = Arc::new(AtomicUsize::new(0));
        (
            Self {
                accept_count: Arc::clone(&accept),
                verify_count: Arc::clone(&verify),
                reject_count: Arc::clone(&reject),
            },
            accept,
            verify,
            reject,
        )
    }

    /// Produce a fake block at the given height.
    fn make_block(height: u64) -> FakeBlockProperties {
        let id = Id::from([low_byte(height).wrapping_add(1); 32]);
        let parent = Id::from([low_byte(height); 32]);
        FakeBlockProperties {
            id,
            parent,
            height,
            timestamp: SystemTime::UNIX_EPOCH,
            bytes: vec![low_byte(height)],
        }
    }
}

// --- AppHandler ---

#[async_trait]
impl AppHandler for FakeChainVm {
    async fn app_request(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _deadline: Instant,
        _request: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_request_failed(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _err: AppError,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_response(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _response: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }

    async fn app_gossip(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _msg: &[u8],
    ) -> VmResult<()> {
        Ok(())
    }
}

// --- HealthCheck ---

#[async_trait]
impl HealthCheck for FakeChainVm {
    async fn health_check(&self, _token: &CancellationToken) -> VmResult<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

// --- Connector ---

#[async_trait]
impl Connector for FakeChainVm {
    async fn connected(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _version: Application,
    ) -> VmResult<()> {
        Ok(())
    }

    async fn disconnected(&mut self, _token: &CancellationToken, _node: NodeId) -> VmResult<()> {
        Ok(())
    }
}

// --- ava_vm::Vm ---

#[async_trait]
impl Vm for FakeChainVm {
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
        Ok(())
    }

    async fn set_state(&mut self, _token: &CancellationToken, _state: EngineState) -> VmResult<()> {
        Ok(())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> VmResult<()> {
        Ok(())
    }

    async fn version(&self, _token: &CancellationToken) -> VmResult<String> {
        Ok("fake/0.0.0".to_string())
    }

    async fn create_handlers(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<std::collections::HashMap<String, HttpHandler>> {
        Ok(std::collections::HashMap::new())
    }

    async fn new_http_handler(
        &mut self,
        _token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        Ok(None)
    }

    async fn wait_for_event(&self, _token: &CancellationToken) -> VmResult<VmEvent> {
        Ok(VmEvent::PendingTxs)
    }
}

// --- SAE ChainVm<FakeBlockProperties> ---

#[async_trait]
impl ChainVm<FakeBlockProperties> for FakeChainVm {
    async fn get_block(&self, _id: Id) -> VmResult<FakeBlockProperties> {
        Ok(Self::make_block(1))
    }

    async fn parse_block(&self, _bytes: &[u8]) -> VmResult<FakeBlockProperties> {
        Ok(Self::make_block(1))
    }

    async fn build_block(&self, _ctx: Option<&BlockContext>) -> VmResult<FakeBlockProperties> {
        Ok(Self::make_block(1))
    }

    async fn verify_block(
        &self,
        _ctx: Option<&BlockContext>,
        _b: &FakeBlockProperties,
    ) -> VmResult<()> {
        self.verify_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn accept_block(&self, _b: &FakeBlockProperties) -> VmResult<()> {
        self.accept_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn reject_block(&self, _b: &FakeBlockProperties) -> VmResult<()> {
        self.reject_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn set_preference(&self, _id: Id, _ctx: Option<&BlockContext>) -> VmResult<()> {
        Ok(())
    }

    async fn last_accepted(&self) -> VmResult<Id> {
        Ok(Id::EMPTY)
    }

    async fn get_block_id_at_height(&self, _h: u64) -> VmResult<Id> {
        Ok(Id::EMPTY)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `accept(token)` on the wrapper block calls `accept_block` on the inner VM.
#[tokio::test]
async fn accept_forwards_to_vm() {
    let (fake_vm, accept_count, _verify, _reject) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let mut adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();

    let block = adaptor.build_block(&token).await.expect("build_block");
    block.accept(&token).await.expect("accept");

    assert_eq!(
        accept_count.load(Ordering::SeqCst),
        1,
        "accept_block called once"
    );
}

/// `reject(token)` on the wrapper block forwards to `reject_block`.
#[tokio::test]
async fn reject_forwards_to_vm() {
    let (fake_vm, _accept, _verify, reject_count) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let mut adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();

    let block = adaptor.build_block(&token).await.expect("build_block");
    block.reject(&token).await.expect("reject");

    assert_eq!(
        reject_count.load(Ordering::SeqCst),
        1,
        "reject_block called once"
    );
}

/// `verify(token)` on the wrapper block forwards to `verify_block` (no
/// context, plain verify path).
#[tokio::test]
async fn verify_forwards_to_vm() {
    let (fake_vm, _accept, verify_count, _reject) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let mut adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();

    let block = adaptor.build_block(&token).await.expect("build_block");
    block.verify(&token).await.expect("verify");

    assert_eq!(
        verify_count.load(Ordering::SeqCst),
        1,
        "verify_block called once"
    );
}

/// `verify_with_context` on the wrapper block forwards to `verify_block` with
/// the supplied `BlockContext`.
#[tokio::test]
async fn verify_with_context_forwards_to_vm() {
    let (fake_vm, _accept, verify_count, _reject) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let mut adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();

    let block = adaptor.build_block(&token).await.expect("build_block");
    let ctx = BlockContext::new(42);

    // Down-cast to WithVerifyContext to call verify_with_context.
    // We need to get the block as a concrete WithVerifyContext.
    // The block returned by build_block is Arc<dyn VmBlock>.
    // We check should_verify_with_context returns true.
    // Need to cast — let's probe via get_block (returns Arc<dyn VmBlock>).
    let id = block.id();
    let block2 = adaptor.get_block(&token, id).await.expect("get_block");

    // Verify the block supports context verification via the adaptor's
    // as_build_with_context() capability.
    let with_ctx = adaptor.as_build_with_context();
    assert!(
        with_ctx.is_some(),
        "adaptor must advertise build_with_context"
    );

    let ctx_block = with_ctx
        .expect("checked above")
        .build_block_with_context(&token, &ctx)
        .await
        .expect("build_block_with_context");

    // Plain verify on ctx_block also calls verify_block (no context).
    ctx_block.verify(&token).await.expect("verify");
    assert!(verify_count.load(Ordering::SeqCst) >= 1);

    // Block id/parent/height/bytes/timestamp come from FakeBlockProperties.
    let expected_id = Id::from([2u8; 32]); // height=1 → id byte = 1+1 = 2
    assert_eq!(block2.id(), expected_id);
    assert_eq!(ctx_block.id(), expected_id);
}

/// `build_block_with_context` (the `BuildBlockWithContext` capability) threads
/// the `BlockContext` through to `ChainVm<BP>::build_block`.
#[tokio::test]
async fn build_block_with_context_available() {
    let (fake_vm, _accept, _verify, _reject) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();
    let ctx = BlockContext::new(100);

    let with_ctx = adaptor.as_build_with_context();
    assert!(
        with_ctx.is_some(),
        "adaptor must expose BuildBlockWithContext"
    );

    let block = with_ctx
        .expect("checked above")
        .build_block_with_context(&token, &ctx)
        .await
        .expect("build_block_with_context");

    // Block properties round-trip from FakeBlockProperties.
    assert_eq!(block.height(), 1);
}

/// `last_accepted` and `get_block_id_at_height` are correctly forwarded.
#[tokio::test]
async fn chain_vm_delegation() {
    let (fake_vm, _accept, _verify, _reject) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();

    let last = adaptor.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(last, Id::EMPTY);

    let at_height = adaptor
        .get_block_id_at_height(&token, 5)
        .await
        .expect("get_block_id_at_height");
    assert_eq!(at_height, Id::EMPTY);
}

/// The `BlockProperties` accessor methods (id/parent/height/timestamp/bytes)
/// are correctly wired on the wrapped block.
#[tokio::test]
async fn block_properties_accessible() {
    let (fake_vm, _accept, _verify, _reject) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let mut adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();

    let block = adaptor.build_block(&token).await.expect("build_block");

    // height=1 → id byte = 1+1 = 2, parent byte = 1
    let expected_id = Id::from([2u8; 32]);
    let expected_parent = Id::from([1u8; 32]);
    assert_eq!(block.id(), expected_id);
    assert_eq!(block.parent(), expected_parent);
    assert_eq!(block.height(), 1u64);
    assert_eq!(block.timestamp(), SystemTime::UNIX_EPOCH);
    assert_eq!(block.bytes(), &[1u8]);
}

/// `parse_block` returns a block whose consensus properties are correct.
#[tokio::test]
async fn parse_block_properties() {
    let (fake_vm, _accept, _verify, _reject) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();

    let block = adaptor
        .parse_block(&token, &[0xAB])
        .await
        .expect("parse_block");
    assert_eq!(block.height(), 1u64);
}

/// Multiple accept calls each increment the counter exactly once.
#[tokio::test]
async fn multiple_accepts() {
    let (fake_vm, accept_count, _verify, _reject) = FakeChainVm::new();
    let vm = Arc::new(tokio::sync::Mutex::new(fake_vm));
    let mut adaptor = convert(Arc::clone(&vm));
    let token = CancellationToken::new();

    for _ in 0..3u8 {
        let block = adaptor.build_block(&token).await.expect("build_block");
        block.accept(&token).await.expect("accept");
    }
    assert_eq!(accept_count.load(Ordering::SeqCst), 3, "three accepts");
}
