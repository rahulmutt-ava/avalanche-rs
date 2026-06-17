// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! State-sync bridge — the second generic adaptor, parallel to the block-VM
//! bridge in [`crate`] (specs/11 §5 upstream-delta, Go `adaptor/sync.go`).
//!
//! # Design
//!
//! Mirrors the block bridge's "the value doesn't know about the VM; forwarding
//! is value → VM" inversion, applied to state sync. A SAE-friendly
//! [`SyncableVm<SP>`] returns plain property bags ([`SummaryProperties`]: an
//! `{id, bytes, height}` triple with no `accept`) from its methods, and
//! [`convert_state_sync`] wraps each `SP` in an [`AdaptorSummary`] whose
//! [`ava_vm::StateSummary::accept`] forwards back to the VM's
//! [`SyncableVm::accept_summary`].
//!
//! # Crate-name disambiguation
//!
//! - **[`SyncableVm<SP>`] / [`SummaryProperties`]** — the SAE-friendly traits
//!   defined here.
//! - **[`ava_vm::StateSyncableVm`] / [`ava_vm::StateSummary`]** — the consensus
//!   Snowman traits the adaptor bridges *to* (referred to by full path).
//!
//! # Error model
//!
//! Unlike the block bridge (which maps `ava_vm::Error` → `ava_snow::Error`),
//! the consensus [`ava_vm::StateSummary`] / [`ava_vm::StateSyncableVm`] traits
//! return [`ava_vm::Result`] directly — the same error hierarchy
//! [`SyncableVm`] uses — so errors forward as-is with no mapping.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_vm::Result as VmResult;
use ava_vm::block::{
    StateSummary as VmStateSummary, StateSyncMode, StateSyncableVm as ConsensusStateSyncableVm,
};

// ---- SummaryProperties trait ----------------------------------------------

/// The property bag that describes a state summary.
///
/// Implemented by the concrete summary type produced by a [`SyncableVm<SP>`]
/// VM. As with [`crate::BlockProperties`], this separates summary
/// identity/content from the consensus life-cycle method: the VM owns
/// `accept_summary`, the summary is a plain value (see Go `adaptor/sync.go`).
pub trait SummaryProperties: Clone + Send + Sync + 'static {
    /// The unique summary identifier.
    fn id(&self) -> Id;

    /// The canonical serialized bytes of this summary.
    fn bytes(&self) -> &[u8];

    /// The height the summary describes.
    fn height(&self) -> u64;
}

// ---- SAE SyncableVm<SP> trait ---------------------------------------------

/// The SAE-generic state-syncable VM interface.
///
/// `V: SyncableVm<SP>` is the trait a concrete SAE VM implements.
/// [`convert_state_sync`] wraps it into the consensus
/// [`ava_vm::StateSyncableVm`]. The getters return plain [`SummaryProperties`]
/// values (`SP`); the bridge wraps them in [`AdaptorSummary`].
///
/// This is **distinct** from the consensus [`ava_vm::StateSyncableVm`]
/// (referred to by its full path). Methods take `&self`; the consensus wrapper
/// acquires the `Mutex` guard internally.
#[async_trait]
pub trait SyncableVm<SP: SummaryProperties>: Send + Sync {
    /// Whether state sync is enabled for this VM.
    async fn state_sync_enabled(&self, token: &CancellationToken) -> VmResult<bool>;

    /// The summary of an in-progress sync.
    ///
    /// Returns `Err(ava_vm::Error::NotFound)` when none is ongoing.
    async fn get_ongoing_sync_state_summary(&self, token: &CancellationToken) -> VmResult<SP>;

    /// The most recent available summary.
    async fn get_last_state_summary(&self, token: &CancellationToken) -> VmResult<SP>;

    /// Parse a summary from its canonical bytes.
    async fn parse_state_summary(&self, token: &CancellationToken, bytes: &[u8]) -> VmResult<SP>;

    /// The summary at the given height.
    ///
    /// Returns `Err(ava_vm::Error::NotFound)` when the height is unknown.
    async fn get_state_summary(&self, token: &CancellationToken, height: u64) -> VmResult<SP>;

    /// Accept the summary, syncing the VM to it and returning the
    /// [`StateSyncMode`] it adopted.
    async fn accept_summary(
        &self,
        token: &CancellationToken,
        summary: &SP,
    ) -> VmResult<StateSyncMode>;
}

// ---- AdaptorSummary -------------------------------------------------------

/// An [`ava_vm::StateSummary`] wrapper that holds a summary-property snapshot
/// and an `Arc`-reference back to the owning VM.
///
/// `accept` forwards to the VM's [`SyncableVm::accept_summary`]; the VM does
/// **not** hold a reference to this summary (summary → VM, never VM →
/// summary). Mirrors [`crate::AdaptorBlock`].
pub struct AdaptorSummary<SP, V>
where
    SP: SummaryProperties,
    V: SyncableVm<SP> + 'static,
{
    sp: SP,
    vm: Arc<Mutex<V>>,
}

impl<SP, V> AdaptorSummary<SP, V>
where
    SP: SummaryProperties,
    V: SyncableVm<SP> + 'static,
{
    fn new(sp: SP, vm: Arc<Mutex<V>>) -> Self {
        Self { sp, vm }
    }
}

#[async_trait]
impl<SP, V> VmStateSummary for AdaptorSummary<SP, V>
where
    SP: SummaryProperties,
    V: SyncableVm<SP> + 'static,
{
    fn id(&self) -> Id {
        self.sp.id()
    }

    fn height(&self) -> u64 {
        self.sp.height()
    }

    fn bytes(&self) -> &[u8] {
        self.sp.bytes()
    }

    async fn accept(&self, token: &CancellationToken) -> VmResult<StateSyncMode> {
        let guard = self.vm.lock().await;
        guard.accept_summary(token, &self.sp).await
    }
}

// ---- ConvertStateSync (implements ava_vm::StateSyncableVm) -----------------

/// The adaptor produced by [`convert_state_sync`].
///
/// Wraps a `Arc<Mutex<V>>` where `V: SyncableVm<SP>`. Implements
/// [`ava_vm::StateSyncableVm`] (the consensus trait) by delegating each getter
/// to the inner `V` and wrapping the returned `SP` in an [`AdaptorSummary`].
/// Mirrors [`crate::Adaptor`].
pub struct ConvertStateSync<SP, V>
where
    SP: SummaryProperties,
    V: SyncableVm<SP> + 'static,
{
    vm: Arc<Mutex<V>>,
    // `SP` is a type parameter; zero-sized phantom data so the compiler knows
    // we logically own `SP`.
    _marker: std::marker::PhantomData<SP>,
}

impl<SP, V> ConvertStateSync<SP, V>
where
    SP: SummaryProperties,
    V: SyncableVm<SP> + 'static,
{
    /// Wraps the inner VM.
    fn new(vm: Arc<Mutex<V>>) -> Self {
        Self {
            vm,
            _marker: std::marker::PhantomData,
        }
    }

    /// Wrap an `SP` into an `Arc<dyn VmStateSummary>`.
    fn wrap(&self, sp: SP) -> Arc<dyn VmStateSummary> {
        Arc::new(AdaptorSummary::new(sp, Arc::clone(&self.vm)))
    }
}

#[async_trait]
impl<SP, V> ConsensusStateSyncableVm for ConvertStateSync<SP, V>
where
    SP: SummaryProperties,
    V: SyncableVm<SP> + 'static,
{
    async fn state_sync_enabled(&self, token: &CancellationToken) -> VmResult<bool> {
        self.vm.lock().await.state_sync_enabled(token).await
    }

    async fn get_ongoing_sync_state_summary(
        &self,
        token: &CancellationToken,
    ) -> VmResult<Arc<dyn VmStateSummary>> {
        let sp = self
            .vm
            .lock()
            .await
            .get_ongoing_sync_state_summary(token)
            .await?;
        Ok(self.wrap(sp))
    }

    async fn get_last_state_summary(
        &self,
        token: &CancellationToken,
    ) -> VmResult<Arc<dyn VmStateSummary>> {
        let sp = self.vm.lock().await.get_last_state_summary(token).await?;
        Ok(self.wrap(sp))
    }

    async fn parse_state_summary(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn VmStateSummary>> {
        let sp = self
            .vm
            .lock()
            .await
            .parse_state_summary(token, bytes)
            .await?;
        Ok(self.wrap(sp))
    }

    async fn get_state_summary(
        &self,
        token: &CancellationToken,
        height: u64,
    ) -> VmResult<Arc<dyn VmStateSummary>> {
        let sp = self
            .vm
            .lock()
            .await
            .get_state_summary(token, height)
            .await?;
        Ok(self.wrap(sp))
    }
}

// ---- Public constructor ---------------------------------------------------

/// Wraps a generic SAE [`SyncableVm<SP>`] into a consensus
/// [`ava_vm::StateSyncableVm`].
///
/// `vm` is a `Arc<Mutex<V>>` rather than a bare `V` so the caller can retain a
/// handle for testing or for cross-cutting concerns. Returns a
/// [`ConvertStateSync`] that forwards all consensus operations to `vm`,
/// wrapping each returned summary in an [`AdaptorSummary`]. Mirrors
/// [`crate::convert`].
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use tokio::sync::Mutex;
/// use ava_saevm_adaptor::convert_state_sync;
///
/// let my_vm = Arc::new(Mutex::new(MySyncableVm::new()));
/// let adaptor = convert_state_sync(Arc::clone(&my_vm));
/// // `adaptor` now implements `ava_vm::StateSyncableVm`.
/// ```
#[must_use]
pub fn convert_state_sync<SP, V>(vm: Arc<Mutex<V>>) -> ConvertStateSync<SP, V>
where
    SP: SummaryProperties,
    V: SyncableVm<SP> + 'static,
{
    ConvertStateSync::new(vm)
}
