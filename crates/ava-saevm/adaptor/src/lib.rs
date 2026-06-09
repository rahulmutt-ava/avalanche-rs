// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-adaptor` — bridges the generic SAE chain VM onto the Snowman
//! [`ava_vm::ChainVm`] trait (specs/11 §5).
//!
//! # Design
//!
//! The SAE VM exposes a generic, block-property-based interface
//! ([`ChainVm<BP>`]) in which the VM owns all state transitions
//! (`verify_block`, `accept_block`, `reject_block`), and blocks are plain
//! property bags ([`BlockProperties`]). The Snowman consensus engine, however,
//! requires [`ava_vm::ChainVm`] where **blocks carry their own
//! `verify`/`accept`/`reject`** methods.
//!
//! [`convert`] bridges the gap: it wraps a `Arc<Mutex<V>>` in an [`Adaptor`]
//! that implements the consensus [`ava_vm::ChainVm`], and each block returned
//! by the adaptor's `build_block`/`get_block`/`parse_block` is an
//! [`AdaptorBlock`] that holds the block's property snapshot and an `Arc`
//! handle back to the VM. The block's `verify`/`accept`/`reject` forward to
//! the VM — the VM does **not** know about the block (the forwarding direction
//! is block → VM, matching Go `vms/saevm/adaptor/`).
//!
//! # Crate-name disambiguation
//!
//! Two `ChainVm` traits exist:
//! - **`crate::ChainVm<BP>`** — the SAE-specific generic trait defined here.
//! - **`ava_vm::ChainVm`** — the consensus Snowman VM trait (referred to by
//!   full path throughout this file).
//!
//! # Error mapping
//!
//! [`ChainVm<BP>`] methods return [`ava_vm::Result`]
//! (`Result<T, ava_vm::Error>`). The consensus [`ava_snow::Block`] trait (which
//! [`AdaptorBlock`] implements) returns [`ava_snow::error::Result`]
//! (`Result<T, ava_snow::Error>`). These are distinct error hierarchies with no
//! built-in conversion; the adaptor maps `ava_vm::Error` →
//! `ava_snow::Error::ParametersInvalid(e.to_string())` at the block boundary.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_vm::block::ChainVm as ConsensusChainVm;
use ava_vm::block::{Block as VmBlock, WithVerifyContext};
use ava_vm::{
    AppError, AppHandler, BlockContext, BuildBlockWithContext, ChainContext, Connector,
    EngineState, Fx, HealthCheck, HttpHandler, Result as VmResult, Vm, VmEvent,
};

use ava_database::DynDatabase;
use ava_types::node_id::NodeId;
use ava_version::application::Application;
use ava_vm::AppSender;

// ---- error mapping helpers ------------------------------------------------

/// Maps a [`ava_vm::Error`] to an [`ava_snow::error::Error`] for use at the
/// `ava_snow::Block` (verify/accept/reject) boundary.
///
/// `ava_snow::Error` has no generic "other" variant; we carry the message via
/// `ParametersInvalid` to preserve at least the human-readable form.
fn vm_err_to_snow(e: &ava_vm::Error) -> ava_snow::error::Error {
    ava_snow::error::Error::ParametersInvalid(e.to_string())
}

// ---- BlockProperties trait -----------------------------------------------

/// The property bag that describes a block.
///
/// Implemented by the concrete block type produced by a [`ChainVm<BP>`] VM.
/// Separating block identity/content from consensus life-cycle methods is the
/// core SAE invariant: the VM owns `verify`/`accept`/`reject`, the block is a
/// plain value (see Go `vms/saevm/adaptor/`).
pub trait BlockProperties: Clone + Send + Sync + 'static {
    /// The unique block identifier.
    fn id(&self) -> Id;

    /// The parent block's identifier.
    fn parent(&self) -> Id;

    /// The canonical serialized bytes of this block.
    fn bytes(&self) -> &[u8];

    /// The block's height in the chain.
    fn height(&self) -> u64;

    /// The block's timestamp.
    fn timestamp(&self) -> SystemTime;
}

// ---- SAE ChainVm<BP> trait -----------------------------------------------

/// The SAE-generic chain VM interface.
///
/// `V: ChainVm<BP>` is the trait a concrete SAE VM implements. [`convert`]
/// wraps it into the consensus [`ava_vm::ChainVm`]. Note that this trait's
/// methods take `&self` because SAE VMs use interior mutability; the consensus
/// wrapper takes `&mut self` for the mutating operations and acquires the
/// `Mutex` guard internally.
///
/// This is **distinct** from the consensus [`ava_vm::ChainVm`] (referred to by
/// its full path). The supertrait `ava_vm::Vm` is included so the adaptor can
/// delegate all base VM operations to the inner `V`.
#[async_trait]
pub trait ChainVm<BP: BlockProperties>: Vm {
    /// Retrieve an existing block by its id.
    ///
    /// Returns `Err(ava_vm::Error::NotFound)` when the block is unknown.
    async fn get_block(&self, id: Id) -> VmResult<BP>;

    /// Parse a block from its canonical bytes.
    async fn parse_block(&self, bytes: &[u8]) -> VmResult<BP>;

    /// Build a new block on top of the current preference.
    ///
    /// `ctx` is the proposervm [`BlockContext`] when available; `None` on the
    /// plain (non-proposervm) path.
    async fn build_block(&self, ctx: Option<&BlockContext>) -> VmResult<BP>;

    /// Verify that the block is valid.
    ///
    /// `ctx` carries the P-Chain height when proposervm is active.
    async fn verify_block(&self, ctx: Option<&BlockContext>, b: &BP) -> VmResult<()>;

    /// Accept the block, committing it to the chain.
    async fn accept_block(&self, b: &BP) -> VmResult<()>;

    /// Reject the block, discarding it.
    async fn reject_block(&self, b: &BP) -> VmResult<()>;

    /// Set the engine's preferred (leaf) block.
    ///
    /// `ctx` is the proposervm context when available.
    async fn set_preference(&self, id: Id, ctx: Option<&BlockContext>) -> VmResult<()>;

    /// The id of the last accepted block (genesis if nothing has been accepted).
    async fn last_accepted(&self) -> VmResult<Id>;

    /// The accepted block id at `height`.
    ///
    /// Returns `Err(ava_vm::Error::NotFound)` when the height is not indexed.
    async fn get_block_id_at_height(&self, h: u64) -> VmResult<Id>;
}

// ---- AdaptorBlock --------------------------------------------------------

/// An [`ava_vm::Block`] wrapper that holds a block-property snapshot and an
/// `Arc`-reference back to the owning VM.
///
/// `verify`/`accept`/`reject` forward to the VM; the VM does **not** hold a
/// reference to this block (block → VM, never VM → block).
pub struct AdaptorBlock<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    bp: BP,
    vm: Arc<Mutex<V>>,
}

impl<BP, V> AdaptorBlock<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    fn new(bp: BP, vm: Arc<Mutex<V>>) -> Self {
        Self { bp, vm }
    }
}

#[async_trait]
impl<BP, V> VmBlock for AdaptorBlock<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    fn id(&self) -> Id {
        self.bp.id()
    }

    fn parent(&self) -> Id {
        self.bp.parent()
    }

    fn height(&self) -> u64 {
        self.bp.height()
    }

    fn timestamp(&self) -> SystemTime {
        self.bp.timestamp()
    }

    fn bytes(&self) -> &[u8] {
        self.bp.bytes()
    }

    async fn verify(&self, token: &CancellationToken) -> ava_snow::error::Result<()> {
        let _ = token;
        let guard = self.vm.lock().await;
        guard
            .verify_block(None, &self.bp)
            .await
            .map_err(|e| vm_err_to_snow(&e))
    }

    async fn accept(&self, token: &CancellationToken) -> ava_snow::error::Result<()> {
        let _ = token;
        let guard = self.vm.lock().await;
        guard
            .accept_block(&self.bp)
            .await
            .map_err(|e| vm_err_to_snow(&e))
    }

    async fn reject(&self, token: &CancellationToken) -> ava_snow::error::Result<()> {
        let _ = token;
        let guard = self.vm.lock().await;
        guard
            .reject_block(&self.bp)
            .await
            .map_err(|e| vm_err_to_snow(&e))
    }
}

/// [`WithVerifyContext`] implementation for [`AdaptorBlock`].
///
/// SAE blocks always support proposervm context verification, so
/// `should_verify_with_context` returns `true` unconditionally and
/// `verify_with_context` passes the context to [`ChainVm::verify_block`].
#[async_trait]
impl<BP, V> WithVerifyContext for AdaptorBlock<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    /// Returns `true` — SAE blocks always require a context when proposervm is
    /// active.
    async fn should_verify_with_context(&self, _token: &CancellationToken) -> ava_vm::Result<bool> {
        Ok(true)
    }

    /// Verify the block against the supplied P-Chain-height context.
    async fn verify_with_context(
        &self,
        _token: &CancellationToken,
        ctx: &BlockContext,
    ) -> ava_vm::Result<()> {
        let guard = self.vm.lock().await;
        guard.verify_block(Some(ctx), &self.bp).await
    }
}

// ---- Adaptor (implements ava_vm::ChainVm) ---------------------------------

/// The adaptor produced by [`convert`].
///
/// Wraps a `Arc<Mutex<V>>` where `V: ChainVm<BP>`. Implements
/// [`ava_vm::ChainVm`] (the consensus trait) by delegating base-VM operations
/// to the inner `V` and wrapping block results in [`AdaptorBlock`].
///
/// The adaptor also implements [`BuildBlockWithContext`] so the proposervm
/// path receives a P-Chain-height context.
///
/// # Concurrency — known limitation (M7.10)
///
/// Every consensus call funnels through the single `Arc<Mutex<V>>`, which exists
/// only because the base [`ava_vm::Vm`] supertrait methods (`initialize`,
/// `set_state`, `app_*`, …) take `&mut self`; the [`ChainVm`] block operations
/// are all `&self`. A consequence is that [`Vm::wait_for_event`] holds the mutex
/// for its **entire** (potentially long-blocking) duration, so a real VM whose
/// `wait_for_event` parks until a pending tx arrives would block every concurrent
/// `verify`/`accept`/`build_block` on the same lock — a deadlock. This is benign
/// for the current conformance fakes (their `wait_for_event` returns promptly),
/// but **must be resolved before wiring the real SAE VM** (M7.18): give `V`
/// interior mutability so the adaptor can hold `Arc<V>` (no outer mutex), or move
/// event notification onto a channel established at `initialize`. Tracked in
/// `plan/M7-saevm.md` (M7.10 as-built) and the M7 progress memory.
pub struct Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    vm: Arc<Mutex<V>>,
    // `BP` is a type parameter; zero-sized phantom data so the compiler knows
    // we logically own `BP`.
    _marker: std::marker::PhantomData<BP>,
}

impl<BP, V> Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    /// Wraps the inner VM.
    fn new(vm: Arc<Mutex<V>>) -> Self {
        Self {
            vm,
            _marker: std::marker::PhantomData,
        }
    }

    /// Wrap a `BP` into an `Arc<dyn VmBlock>`.
    fn wrap(&self, bp: BP) -> Arc<dyn VmBlock> {
        Arc::new(AdaptorBlock::new(bp, Arc::clone(&self.vm)))
    }
}

// --- Delegate all ava_vm::Vm supertraits to the inner V ---

#[async_trait]
impl<BP, V> AppHandler for Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: std::time::Instant,
        request: &[u8],
    ) -> VmResult<()> {
        self.vm
            .lock()
            .await
            .app_request(token, node, request_id, deadline, request)
            .await
    }

    async fn app_request_failed(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> VmResult<()> {
        self.vm
            .lock()
            .await
            .app_request_failed(token, node, request_id, err)
            .await
    }

    async fn app_response(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> VmResult<()> {
        self.vm
            .lock()
            .await
            .app_response(token, node, request_id, response)
            .await
    }

    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> VmResult<()> {
        self.vm.lock().await.app_gossip(token, node, msg).await
    }
}

#[async_trait]
impl<BP, V> HealthCheck for Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    async fn health_check(&self, token: &CancellationToken) -> VmResult<serde_json::Value> {
        self.vm.lock().await.health_check(token).await
    }
}

#[async_trait]
impl<BP, V> Connector for Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    async fn connected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> VmResult<()> {
        self.vm.lock().await.connected(token, node, version).await
    }

    async fn disconnected(&mut self, token: &CancellationToken, node: NodeId) -> VmResult<()> {
        self.vm.lock().await.disconnected(token, node).await
    }
}

#[async_trait]
impl<BP, V> Vm for Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    #[allow(clippy::too_many_arguments)]
    async fn initialize(
        &mut self,
        token: &CancellationToken,
        chain_ctx: Arc<ChainContext>,
        db: Arc<dyn DynDatabase>,
        genesis_bytes: &[u8],
        upgrade_bytes: &[u8],
        config_bytes: &[u8],
        fxs: Vec<Fx>,
        app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        self.vm
            .lock()
            .await
            .initialize(
                token,
                chain_ctx,
                db,
                genesis_bytes,
                upgrade_bytes,
                config_bytes,
                fxs,
                app_sender,
            )
            .await
    }

    async fn set_state(&mut self, token: &CancellationToken, state: EngineState) -> VmResult<()> {
        self.vm.lock().await.set_state(token, state).await
    }

    async fn shutdown(&mut self, token: &CancellationToken) -> VmResult<()> {
        self.vm.lock().await.shutdown(token).await
    }

    async fn version(&self, token: &CancellationToken) -> VmResult<String> {
        self.vm.lock().await.version(token).await
    }

    async fn create_handlers(
        &mut self,
        token: &CancellationToken,
    ) -> VmResult<std::collections::HashMap<String, HttpHandler>> {
        self.vm.lock().await.create_handlers(token).await
    }

    async fn new_http_handler(
        &mut self,
        token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        self.vm.lock().await.new_http_handler(token).await
    }

    async fn wait_for_event(&self, token: &CancellationToken) -> VmResult<VmEvent> {
        self.vm.lock().await.wait_for_event(token).await
    }
}

// --- Implement the consensus ava_vm::ChainVm for Adaptor ---

#[async_trait]
impl<BP, V> ConsensusChainVm for Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    async fn build_block(&mut self, _token: &CancellationToken) -> VmResult<Arc<dyn VmBlock>> {
        let bp = self.vm.lock().await.build_block(None).await?;
        Ok(self.wrap(bp))
    }

    async fn get_block(&self, _token: &CancellationToken, id: Id) -> VmResult<Arc<dyn VmBlock>> {
        let bp = self.vm.lock().await.get_block(id).await?;
        Ok(self.wrap(bp))
    }

    async fn parse_block(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn VmBlock>> {
        let bp = self.vm.lock().await.parse_block(bytes).await?;
        Ok(self.wrap(bp))
    }

    async fn set_preference(&mut self, _token: &CancellationToken, id: Id) -> VmResult<()> {
        self.vm.lock().await.set_preference(id, None).await
    }

    async fn last_accepted(&self, _token: &CancellationToken) -> VmResult<Id> {
        self.vm.lock().await.last_accepted().await
    }

    async fn get_block_id_at_height(
        &self,
        _token: &CancellationToken,
        height: u64,
    ) -> VmResult<Id> {
        self.vm.lock().await.get_block_id_at_height(height).await
    }

    /// Returns `Some(self)` — the adaptor always supports the proposervm
    /// [`BuildBlockWithContext`] capability.
    fn as_build_with_context(&self) -> Option<&dyn BuildBlockWithContext> {
        Some(self)
    }
}

// --- BuildBlockWithContext for Adaptor ---

/// Implements [`ava_vm::BuildBlockWithContext`] for [`Adaptor`].
///
/// Passes the proposervm [`BlockContext`] through to
/// [`ChainVm::build_block`].
#[async_trait]
impl<BP, V> BuildBlockWithContext for Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    async fn build_block_with_context(
        &self,
        _token: &CancellationToken,
        ctx: &BlockContext,
    ) -> VmResult<Arc<dyn VmBlock>> {
        let bp = self.vm.lock().await.build_block(Some(ctx)).await?;
        Ok(self.wrap(bp))
    }
}

// ---- Public constructor ---------------------------------------------------

/// Wraps a generic SAE VM into a consensus [`ava_vm::ChainVm`].
///
/// `vm` is a `Arc<Mutex<V>>` rather than a bare `V` so the caller can
/// retain a handle for testing or for implementing cross-cutting concerns.
/// Returns an [`Adaptor`] that forwards all consensus operations to `vm`.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use tokio::sync::Mutex;
/// use ava_saevm_adaptor::convert;
///
/// let my_vm = Arc::new(Mutex::new(MySaeVm::new()));
/// let adaptor = convert(Arc::clone(&my_vm));
/// // `adaptor` now implements `ava_vm::ChainVm`.
/// ```
pub fn convert<BP, V>(vm: Arc<Mutex<V>>) -> Adaptor<BP, V>
where
    BP: BlockProperties,
    V: ChainVm<BP> + 'static,
{
    Adaptor::new(vm)
}
