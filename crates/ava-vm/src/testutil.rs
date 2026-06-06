// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! In-memory test [`ChainVm`] + the [`vm_conformance!`] macro (specs 07 §10).
//!
//! Gated behind the `testutil` feature so the conformance battery can run
//! against any `ChainVm` (here, `TestVm`) without pulling test-only code into a
//! production build. The macro is the generic VM-conformance battery that every
//! concrete VM (`08`–`11`) and the rpcchainvm host/guest pair are expected to
//! pass.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
// `tokio` is only used by the exported `vm_conformance!` macro (it expands in the
// downstream crate, so the lib never references it directly).
use tokio as _;
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_snow::{ChainContext, EngineState, Result as SnowResult};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;

use crate::app::{AppError, AppHandler};
use crate::app_sender::{AppSender, SendConfig};
use crate::block::{Block, ChainVm};
use crate::connector::Connector;
use crate::error::{Error, Result};
use crate::health::HealthCheck;
use crate::vm::{Fx, HttpHandler, Vm, VmEvent};

/// The shared, mutable state behind a [`TestVm`] and its [`TestBlock`]s.
///
/// A `TestBlock::accept` reaches back into this state to advance
/// `last_accepted` + the height index, exactly as a real VM's block would write
/// back to the VM's storage on accept.
#[derive(Debug, Default)]
struct Inner {
    /// All known (built or parsed) blocks, by id.
    blocks: HashMap<Id, Arc<TestBlock>>,
    /// The accepted height index (`height -> id`). `BTreeMap`, not `HashMap`, so
    /// iteration is deterministic (specs 00 §6.1).
    accepted_at_height: BTreeMap<u64, Id>,
    /// The id of the last accepted block.
    last_accepted: Id,
    /// The currently preferred (leaf) block.
    preference: Id,
}

/// An in-memory Snowman block used by the conformance battery.
///
/// `bytes` is a trivial canonical encoding (`parent ++ be64(height) ++ payload`)
/// so `parse_block` round-trips. `accept`/`reject` write back through the shared
/// [`Inner`] so the VM's `last_accepted` + height index stay correct.
#[derive(Debug)]
pub struct TestBlock {
    id: Id,
    parent: Id,
    height: u64,
    timestamp: SystemTime,
    bytes: Vec<u8>,
    inner: Arc<Mutex<Inner>>,
}

impl TestBlock {
    /// The canonical byte encoding of a block: `parent (32) ++ be64(height) ++
    /// payload`.
    fn encode(parent: Id, height: u64, payload: &[u8]) -> Vec<u8> {
        let cap = 40usize.saturating_add(payload.len());
        let mut bytes = Vec::with_capacity(cap);
        bytes.extend_from_slice(&parent.to_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    /// Decodes `(parent, height, payload)` from canonical block bytes.
    fn decode(bytes: &[u8]) -> Result<(Id, u64, Vec<u8>)> {
        let parent_slice = bytes.get(..32).ok_or(Error::NotFound)?;
        let height_slice = bytes.get(32..40).ok_or(Error::NotFound)?;
        let payload = bytes.get(40..).ok_or(Error::NotFound)?;

        let mut parent_bytes = [0u8; 32];
        parent_bytes.copy_from_slice(parent_slice);
        let mut height_bytes = [0u8; 8];
        height_bytes.copy_from_slice(height_slice);
        Ok((
            Id::from(parent_bytes),
            u64::from_be_bytes(height_bytes),
            payload.to_vec(),
        ))
    }

    /// Derives a block id deterministically from its canonical bytes
    /// (`sha256(bytes)`).
    fn derive_id(bytes: &[u8]) -> Id {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest: [u8; 32] = hasher.finalize().into();
        Id::from(digest)
    }
}

#[async_trait]
impl Block for TestBlock {
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

    async fn verify(&self, _token: &CancellationToken) -> SnowResult<()> {
        Ok(())
    }

    async fn accept(&self, _token: &CancellationToken) -> SnowResult<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.last_accepted = self.id;
        inner.accepted_at_height.insert(self.height, self.id);
        Ok(())
    }

    async fn reject(&self, _token: &CancellationToken) -> SnowResult<()> {
        Ok(())
    }
}

/// A minimal in-memory [`ChainVm`] for the conformance battery.
///
/// `initialize` seeds a genesis block (height 0) as the last accepted block.
/// `build_block` appends a child of the current preference. The VM tracks an
/// engine phase so the battery can assert `set_state` transitions and that
/// `shutdown` is idempotent.
#[derive(Debug)]
pub struct TestVm {
    inner: Arc<Mutex<Inner>>,
    /// The genesis block id, returned by `last_accepted` before anything else is
    /// accepted.
    genesis_id: Id,
    /// The current engine phase, or `None` before `initialize`.
    state: Option<EngineState>,
    /// Whether `shutdown` has run (so a second call is a no-op).
    shutdown: bool,
    /// Monotonic payload counter so successive built blocks differ.
    next_payload: u64,
}

impl Default for TestVm {
    fn default() -> Self {
        Self::new()
    }
}

impl TestVm {
    /// Builds an uninitialized [`TestVm`]. Call [`Vm::initialize`] before use.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            genesis_id: Id::EMPTY,
            state: None,
            shutdown: false,
            next_payload: 0,
        }
    }

    fn register(&self, parent: Id, height: u64, payload: &[u8]) -> Arc<TestBlock> {
        let bytes = TestBlock::encode(parent, height, payload);
        let id = TestBlock::derive_id(&bytes);
        let block = Arc::new(TestBlock {
            id,
            parent,
            height,
            timestamp: SystemTime::UNIX_EPOCH,
            bytes,
            inner: Arc::clone(&self.inner),
        });
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.blocks.entry(id).or_insert_with(|| Arc::clone(&block));
        Arc::clone(&block)
    }
}

#[async_trait]
impl AppHandler for TestVm {
    async fn app_request(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _deadline: Instant,
        _request: &[u8],
    ) -> Result<()> {
        Ok(())
    }

    async fn app_request_failed(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _err: AppError,
    ) -> Result<()> {
        Ok(())
    }

    async fn app_response(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _response: &[u8],
    ) -> Result<()> {
        Ok(())
    }

    async fn app_gossip(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _msg: &[u8],
    ) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl HealthCheck for TestVm {
    async fn health_check(&self, _token: &CancellationToken) -> Result<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl Connector for TestVm {
    async fn connected(
        &mut self,
        _token: &CancellationToken,
        _node: NodeId,
        _version: Application,
    ) -> Result<()> {
        Ok(())
    }

    async fn disconnected(&mut self, _token: &CancellationToken, _node: NodeId) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Vm for TestVm {
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
    ) -> Result<()> {
        // Seed the genesis block (height 0) as the last accepted block.
        let genesis = self.register(Id::EMPTY, 0, b"genesis");
        self.genesis_id = genesis.id();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.last_accepted = genesis.id();
            inner.preference = genesis.id();
            inner.accepted_at_height.insert(0, genesis.id());
        }
        self.state = Some(EngineState::Initializing);
        Ok(())
    }

    async fn set_state(&mut self, _token: &CancellationToken, state: EngineState) -> Result<()> {
        self.state = Some(state);
        Ok(())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> Result<()> {
        // Idempotent: a second call is a no-op.
        self.shutdown = true;
        Ok(())
    }

    async fn version(&self, _token: &CancellationToken) -> Result<String> {
        Ok("testvm/0.0.0".to_string())
    }

    async fn create_handlers(
        &mut self,
        _token: &CancellationToken,
    ) -> Result<HashMap<String, HttpHandler>> {
        Ok(HashMap::new())
    }

    async fn new_http_handler(
        &mut self,
        _token: &CancellationToken,
    ) -> Result<Option<HttpHandler>> {
        Ok(None)
    }

    async fn wait_for_event(&self, _token: &CancellationToken) -> Result<VmEvent> {
        Ok(VmEvent::PendingTxs)
    }
}

#[async_trait]
impl ChainVm for TestVm {
    async fn build_block(&mut self, _token: &CancellationToken) -> Result<Arc<dyn Block>> {
        let (parent, height) = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let parent = inner.preference;
            let parent_height = inner.blocks.get(&parent).map_or(0, |b| b.height());
            (parent, parent_height.saturating_add(1))
        };
        let payload = self.next_payload.to_be_bytes();
        self.next_payload = self.next_payload.saturating_add(1);
        let block = self.register(parent, height, &payload);
        Ok(block as Arc<dyn Block>)
    }

    async fn get_block(&self, _token: &CancellationToken, id: Id) -> Result<Arc<dyn Block>> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .blocks
            .get(&id)
            .map(|b| Arc::clone(b) as Arc<dyn Block>)
            .ok_or(Error::NotFound)
    }

    async fn parse_block(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> Result<Arc<dyn Block>> {
        let (parent, height, _payload) = TestBlock::decode(bytes)?;
        let id = TestBlock::derive_id(bytes);
        let block = Arc::new(TestBlock {
            id,
            parent,
            height,
            timestamp: SystemTime::UNIX_EPOCH,
            bytes: bytes.to_vec(),
            inner: Arc::clone(&self.inner),
        });
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.blocks.entry(id).or_insert_with(|| Arc::clone(&block));
        Ok(block as Arc<dyn Block>)
    }

    async fn set_preference(&mut self, _token: &CancellationToken, id: Id) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.preference = id;
        Ok(())
    }

    async fn last_accepted(&self, _token: &CancellationToken) -> Result<Id> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Ok(inner.last_accepted)
    }

    async fn get_block_id_at_height(&self, _token: &CancellationToken, height: u64) -> Result<Id> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .accepted_at_height
            .get(&height)
            .copied()
            .ok_or(Error::NotFound)
    }
}

/// A no-op [`AppSender`] for the conformance battery's `initialize` call.
#[derive(Debug, Default)]
pub struct NoopAppSender;

#[async_trait]
impl AppSender for NoopAppSender {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        _nodes: &HashSet<NodeId>,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> Result<()> {
        Ok(())
    }

    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> Result<()> {
        Ok(())
    }

    async fn send_app_error(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _code: i32,
        _message: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn send_app_gossip(
        &self,
        _token: &CancellationToken,
        _config: SendConfig,
        _bytes: Vec<u8>,
    ) -> Result<()> {
        Ok(())
    }
}

/// Builds a minimal [`ChainContext`] suitable for driving a `ChainVm` in tests.
#[must_use]
pub fn test_chain_context() -> Arc<ChainContext> {
    Arc::new(ChainContext {
        network_id: 1,
        subnet_id: Id::EMPTY,
        chain_id: Id::EMPTY,
        node_id: NodeId::default(),
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(1),
        x_chain_id: Id::EMPTY,
        c_chain_id: Id::EMPTY,
        avax_asset_id: Id::EMPTY,
        chain_data_dir: std::path::PathBuf::new(),
    })
}

/// Initializes a freshly-built [`TestVm`] with a no-op DB / app sender, returning
/// the initialized VM. Convenience for the conformance battery.
///
/// # Errors
/// Propagates any error from [`Vm::initialize`].
pub async fn init_test_vm(token: &CancellationToken) -> Result<TestVm> {
    let mut vm = TestVm::new();
    let db: Arc<dyn DynDatabase> = Arc::new(ava_database::MemDb::new());
    vm.initialize(
        token,
        test_chain_context(),
        db,
        b"genesis",
        b"",
        b"",
        Vec::new(),
        Arc::new(NoopAppSender),
    )
    .await?;
    Ok(vm)
}

/// The generic VM-conformance battery (specs 07 §10).
///
/// `vm_conformance!(make_vm)` expands to a module of `#[tokio::test]`s that drive
/// the `ChainVm` returned by the async `make_vm` closure. The closure takes a
/// `CancellationToken` **by value** (cheap clone — sidesteps the higher-ranked
/// lifetime on the returned future) and must yield an already-`initialize`d VM
/// whose genesis is the last accepted block. The battery covers:
///
/// * init ⇒ genesis is `last_accepted` and is at height 0;
/// * `build_block` ⇒ `verify`/`accept` advances `last_accepted` + the height
///   index;
/// * `parse_block` round-trips a built block's `bytes`;
/// * `get_block` returns accepted and processing blocks;
/// * `Err(NotFound)` for an unknown id and an unknown height;
/// * `set_preference`;
/// * the optional-capability probes default to `None`;
/// * `set_state` phase transitions;
/// * `shutdown` is idempotent.
#[macro_export]
macro_rules! vm_conformance {
    ($make_vm:expr) => {
        mod vm_conformance {
            use std::sync::Arc;

            use tokio_util::sync::CancellationToken;

            use $crate::block::{Block, ChainVm};
            use $crate::error::Error;
            use $crate::vm::Vm;
            use $crate::{EngineState, Id};

            // Silence dead-code on the imports for VMs that exercise a subset.
            #[allow(unused_imports)]
            use $crate::block::{BatchedChainVm, StateSyncableVm};

            #[tokio::test]
            async fn init_genesis_is_last_accepted() {
                let token = CancellationToken::new();
                let vm = ($make_vm)(token.clone()).await;
                let last = vm.last_accepted(&token).await.expect("last_accepted");
                let genesis = vm
                    .get_block_id_at_height(&token, 0)
                    .await
                    .expect("genesis at height 0");
                assert_eq!(last, genesis, "genesis must be the last accepted block");
                let blk = vm.get_block(&token, last).await.expect("get genesis");
                assert_eq!(blk.height(), 0, "genesis is at height 0");
            }

            #[tokio::test]
            async fn build_verify_accept_advances() {
                let token = CancellationToken::new();
                let mut vm = ($make_vm)(token.clone()).await;
                let parent = vm.last_accepted(&token).await.expect("last_accepted");

                let blk = vm.build_block(&token).await.expect("build_block");
                assert_eq!(blk.parent(), parent, "built on the preferred block");
                assert_eq!(blk.height(), 1, "child of genesis is at height 1");

                blk.verify(&token).await.expect("verify");
                blk.accept(&token).await.expect("accept");

                let last = vm.last_accepted(&token).await.expect("last_accepted");
                assert_eq!(last, blk.id(), "accept advances last_accepted");
                let at_height = vm
                    .get_block_id_at_height(&token, 1)
                    .await
                    .expect("height index advanced");
                assert_eq!(at_height, blk.id(), "accept advances the height index");
            }

            #[tokio::test]
            async fn parse_round_trips_bytes() {
                let token = CancellationToken::new();
                let mut vm = ($make_vm)(token.clone()).await;
                let blk = vm.build_block(&token).await.expect("build_block");
                let bytes = blk.bytes().to_vec();
                let parsed = vm.parse_block(&token, &bytes).await.expect("parse_block");
                assert_eq!(parsed.id(), blk.id(), "parse round-trips the id");
                assert_eq!(parsed.bytes(), bytes.as_slice(), "parse round-trips bytes");
            }

            #[tokio::test]
            async fn get_block_accepted_and_processing() {
                let token = CancellationToken::new();
                let mut vm = ($make_vm)(token.clone()).await;
                // Processing: built but not yet accepted.
                let processing = vm.build_block(&token).await.expect("build_block");
                let got = vm
                    .get_block(&token, processing.id())
                    .await
                    .expect("get processing block");
                assert_eq!(got.id(), processing.id());

                // Accepted.
                processing.verify(&token).await.expect("verify");
                processing.accept(&token).await.expect("accept");
                let got = vm
                    .get_block(&token, processing.id())
                    .await
                    .expect("get accepted block");
                assert_eq!(got.id(), processing.id());
            }

            #[tokio::test]
            async fn unknown_id_and_height_not_found() {
                let token = CancellationToken::new();
                let vm = ($make_vm)(token.clone()).await;
                let unknown = Id::from([0xABu8; 32]);
                assert!(matches!(
                    vm.get_block(&token, unknown).await,
                    Err(Error::NotFound)
                ));
                assert!(matches!(
                    vm.get_block_id_at_height(&token, 99_999).await,
                    Err(Error::NotFound)
                ));
            }

            #[tokio::test]
            async fn set_preference_ok() {
                let token = CancellationToken::new();
                let mut vm = ($make_vm)(token.clone()).await;
                let blk = vm.build_block(&token).await.expect("build_block");
                vm.set_preference(&token, blk.id())
                    .await
                    .expect("set_preference");
                // Building again now extends the new preference.
                let child = vm.build_block(&token).await.expect("build child");
                assert_eq!(child.parent(), blk.id(), "build extends the preference");
            }

            #[tokio::test]
            async fn capability_probes_default_none() {
                let token = CancellationToken::new();
                let vm = ($make_vm)(token.clone()).await;
                let vm_ref: &dyn ChainVm = &vm;
                assert!(vm_ref.as_build_with_context().is_none());
                assert!(vm_ref.as_set_preference_with_context().is_none());
                assert!(vm_ref.as_batched().is_none());
                assert!(vm_ref.as_state_syncable().is_none());
            }

            #[tokio::test]
            async fn set_state_transitions() {
                let token = CancellationToken::new();
                let mut vm = ($make_vm)(token.clone()).await;
                for state in [
                    EngineState::StateSyncing,
                    EngineState::Bootstrapping,
                    EngineState::NormalOp,
                ] {
                    vm.set_state(&token, state).await.expect("set_state");
                }
            }

            #[tokio::test]
            async fn shutdown_idempotent() {
                let token = CancellationToken::new();
                let mut vm = ($make_vm)(token.clone()).await;
                vm.shutdown(&token).await.expect("first shutdown");
                vm.shutdown(&token)
                    .await
                    .expect("second shutdown is a no-op");
            }

            // Keep the `Arc`/`Block` imports used in all VM specializations.
            #[allow(dead_code)]
            fn _assert_object_safe(_: Arc<dyn Block>) {}
        }
    };
}
