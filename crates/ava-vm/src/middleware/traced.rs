// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `tracedvm.blockVM` — the tracing `ChainVm` decorator (specs 07 §6; Go
//! `vms/tracedvm/`).
//!
//! [`TracedVm`] wraps a [`ChainVm`] and *is* a [`ChainVm`], opening a `tracing`
//! span per method named `"<name>.<method>"` (matching Go's
//! `name + ".buildBlock"` span tags). Each async method runs `instrument`ed by
//! its span, so the span is **guaranteed to end** when the instrumented future
//! resolves or is dropped (the `tracing::Span` is the Rust analogue of Go's
//! `defer span.End()` — 00 §4.6). The optional inner capabilities are re-exposed
//! wrapped, exactly like [`MeterVm`](super::MeterVm).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use ava_database::DynDatabase;
use ava_snow::{ChainContext, EngineState};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;

use crate::app::{AppError, AppHandler};
use crate::app_sender::AppSender;
use crate::block::batched::BatchedChainVm;
use crate::block::chain_vm::{BuildBlockWithContext, ChainVm, SetPreferenceWithContext};
use crate::block::state_sync::{StateSummary, StateSyncableVm};
use crate::block::{Block, BlockContext};
use crate::connector::Connector;
use crate::error::{Error, Result};
use crate::health::HealthCheck;
use crate::vm::{Fx, HttpHandler, Vm, VmEvent};

/// `tracedvm.blockVM` — a tracing [`ChainVm`] decorator.
///
/// Wrap a VM with [`TracedVm::new`]; the wrapper opens a span per method and
/// re-exposes the inner VM's optional capabilities (see the module docs).
pub struct TracedVm<V: ChainVm> {
    inner: V,
    name: String,
    supports_build_with_context: bool,
    supports_set_preference_with_context: bool,
    supports_batched: bool,
    supports_state_sync: bool,
}

impl<V: ChainVm> TracedVm<V> {
    /// `tracedvm.NewBlockVM` — probe the inner VM's capabilities once and return
    /// the tracing wrapper. `name` is the chain alias used as the span prefix
    /// (e.g. `"primaryAlias"` / `"proposervm"`, mirroring the chain pipeline,
    /// specs 07 §8).
    pub fn new(inner: V, name: String) -> Self {
        let supports_build_with_context = inner.as_build_with_context().is_some();
        let supports_set_preference_with_context = inner.as_set_preference_with_context().is_some();
        let supports_batched = inner.as_batched().is_some();
        let supports_state_sync = inner.as_state_syncable().is_some();
        Self {
            inner,
            name,
            supports_build_with_context,
            supports_set_preference_with_context,
            supports_batched,
            supports_state_sync,
        }
    }

    /// Borrows the inner VM (test/introspection helper).
    #[must_use]
    pub fn inner(&self) -> &V {
        &self.inner
    }

    /// Builds the per-method span named `"<name>.<method>"` (Go's span tag).
    fn span(&self, method: &'static str) -> tracing::Span {
        tracing::info_span!("tracedvm", vm = %self.name, method = method)
    }
}

#[async_trait]
impl<V: ChainVm> AppHandler for TracedVm<V> {
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> Result<()> {
        let span = self.span("appRequest");
        self.inner
            .app_request(token, node, request_id, deadline, request)
            .instrument(span)
            .await
    }

    async fn app_request_failed(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> Result<()> {
        let span = self.span("appRequestFailed");
        self.inner
            .app_request_failed(token, node, request_id, err)
            .instrument(span)
            .await
    }

    async fn app_response(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> Result<()> {
        let span = self.span("appResponse");
        self.inner
            .app_response(token, node, request_id, response)
            .instrument(span)
            .await
    }

    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> Result<()> {
        let span = self.span("appGossip");
        self.inner
            .app_gossip(token, node, msg)
            .instrument(span)
            .await
    }
}

#[async_trait]
impl<V: ChainVm> HealthCheck for TracedVm<V> {
    async fn health_check(&self, token: &CancellationToken) -> Result<serde_json::Value> {
        let span = self.span("healthCheck");
        self.inner.health_check(token).instrument(span).await
    }
}

#[async_trait]
impl<V: ChainVm> Connector for TracedVm<V> {
    async fn connected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> Result<()> {
        let span = self.span("connected");
        self.inner
            .connected(token, node, version)
            .instrument(span)
            .await
    }

    async fn disconnected(&mut self, token: &CancellationToken, node: NodeId) -> Result<()> {
        let span = self.span("disconnected");
        self.inner.disconnected(token, node).instrument(span).await
    }
}

#[async_trait]
impl<V: ChainVm> Vm for TracedVm<V> {
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
    ) -> Result<()> {
        let span = self.span("initialize");
        self.inner
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
            .instrument(span)
            .await
    }

    async fn set_state(&mut self, token: &CancellationToken, state: EngineState) -> Result<()> {
        let span = self.span("setState");
        self.inner.set_state(token, state).instrument(span).await
    }

    async fn shutdown(&mut self, token: &CancellationToken) -> Result<()> {
        let span = self.span("shutdown");
        self.inner.shutdown(token).instrument(span).await
    }

    async fn version(&self, token: &CancellationToken) -> Result<String> {
        let span = self.span("version");
        self.inner.version(token).instrument(span).await
    }

    async fn create_handlers(
        &mut self,
        token: &CancellationToken,
    ) -> Result<HashMap<String, HttpHandler>> {
        let span = self.span("createHandlers");
        self.inner.create_handlers(token).instrument(span).await
    }

    async fn new_http_handler(&mut self, token: &CancellationToken) -> Result<Option<HttpHandler>> {
        let span = self.span("newHTTPHandler");
        self.inner.new_http_handler(token).instrument(span).await
    }

    async fn wait_for_event(&self, token: &CancellationToken) -> Result<VmEvent> {
        let span = self.span("waitForEvent");
        self.inner.wait_for_event(token).instrument(span).await
    }
}

#[async_trait]
impl<V: ChainVm> ChainVm for TracedVm<V> {
    async fn build_block(&mut self, token: &CancellationToken) -> Result<Arc<dyn Block>> {
        let span = self.span("buildBlock");
        self.inner.build_block(token).instrument(span).await
    }

    async fn get_block(&self, token: &CancellationToken, id: Id) -> Result<Arc<dyn Block>> {
        let span = self.span("getBlock");
        self.inner.get_block(token, id).instrument(span).await
    }

    async fn parse_block(&self, token: &CancellationToken, bytes: &[u8]) -> Result<Arc<dyn Block>> {
        let span = self.span("parseBlock");
        self.inner.parse_block(token, bytes).instrument(span).await
    }

    async fn set_preference(&mut self, token: &CancellationToken, id: Id) -> Result<()> {
        let span = self.span("setPreference");
        self.inner.set_preference(token, id).instrument(span).await
    }

    async fn last_accepted(&self, token: &CancellationToken) -> Result<Id> {
        let span = self.span("lastAccepted");
        self.inner.last_accepted(token).instrument(span).await
    }

    async fn get_block_id_at_height(&self, token: &CancellationToken, height: u64) -> Result<Id> {
        let span = self.span("getBlockIDAtHeight");
        self.inner
            .get_block_id_at_height(token, height)
            .instrument(span)
            .await
    }

    fn as_build_with_context(&self) -> Option<&dyn BuildBlockWithContext> {
        if self.supports_build_with_context {
            Some(self)
        } else {
            None
        }
    }

    fn as_set_preference_with_context(&self) -> Option<&dyn SetPreferenceWithContext> {
        if self.supports_set_preference_with_context {
            Some(self)
        } else {
            None
        }
    }

    fn as_batched(&self) -> Option<&dyn BatchedChainVm> {
        if self.supports_batched {
            Some(self)
        } else {
            None
        }
    }

    fn as_state_syncable(&self) -> Option<&dyn StateSyncableVm> {
        if self.supports_state_sync {
            Some(self)
        } else {
            None
        }
    }
}

#[async_trait]
impl<V: ChainVm> BuildBlockWithContext for TracedVm<V> {
    async fn build_block_with_context(
        &self,
        token: &CancellationToken,
        ctx: &BlockContext,
    ) -> Result<Arc<dyn Block>> {
        let inner = self
            .inner
            .as_build_with_context()
            .ok_or(Error::RemoteVmNotImplemented)?;
        let span = self.span("buildBlockWithContext");
        inner
            .build_block_with_context(token, ctx)
            .instrument(span)
            .await
    }
}

#[async_trait]
impl<V: ChainVm> SetPreferenceWithContext for TracedVm<V> {
    async fn set_preference_with_context(
        &self,
        token: &CancellationToken,
        id: Id,
        ctx: &BlockContext,
    ) -> Result<()> {
        let inner = self
            .inner
            .as_set_preference_with_context()
            .ok_or(Error::RemoteVmNotImplemented)?;
        let span = self.span("setPreferenceWithContext");
        inner
            .set_preference_with_context(token, id, ctx)
            .instrument(span)
            .await
    }
}

#[async_trait]
impl<V: ChainVm> BatchedChainVm for TracedVm<V> {
    async fn get_ancestors(
        &self,
        token: &CancellationToken,
        blk_id: Id,
        max_blocks_num: usize,
        max_blocks_size: usize,
        max_retrieval_time: Duration,
    ) -> Result<Vec<Vec<u8>>> {
        let inner = self
            .inner
            .as_batched()
            .ok_or(Error::RemoteVmNotImplemented)?;
        let span = self.span("getAncestors");
        inner
            .get_ancestors(
                token,
                blk_id,
                max_blocks_num,
                max_blocks_size,
                max_retrieval_time,
            )
            .instrument(span)
            .await
    }

    async fn batched_parse_block(
        &self,
        token: &CancellationToken,
        blks: &[Vec<u8>],
    ) -> Result<Vec<Arc<dyn Block>>> {
        let inner = self
            .inner
            .as_batched()
            .ok_or(Error::RemoteVmNotImplemented)?;
        let span = self.span("batchedParseBlock");
        inner
            .batched_parse_block(token, blks)
            .instrument(span)
            .await
    }
}

#[async_trait]
impl<V: ChainVm> StateSyncableVm for TracedVm<V> {
    async fn state_sync_enabled(&self, token: &CancellationToken) -> Result<bool> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(Error::StateSyncableVmNotImplemented)?;
        let span = self.span("stateSyncEnabled");
        inner.state_sync_enabled(token).instrument(span).await
    }

    async fn get_ongoing_sync_state_summary(
        &self,
        token: &CancellationToken,
    ) -> Result<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(Error::StateSyncableVmNotImplemented)?;
        let span = self.span("getOngoingSyncStateSummary");
        inner
            .get_ongoing_sync_state_summary(token)
            .instrument(span)
            .await
    }

    async fn get_last_state_summary(
        &self,
        token: &CancellationToken,
    ) -> Result<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(Error::StateSyncableVmNotImplemented)?;
        let span = self.span("getLastStateSummary");
        inner.get_last_state_summary(token).instrument(span).await
    }

    async fn parse_state_summary(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> Result<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(Error::StateSyncableVmNotImplemented)?;
        let span = self.span("parseStateSummary");
        inner
            .parse_state_summary(token, bytes)
            .instrument(span)
            .await
    }

    async fn get_state_summary(
        &self,
        token: &CancellationToken,
        height: u64,
    ) -> Result<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(Error::StateSyncableVmNotImplemented)?;
        let span = self.span("getStateSummary");
        inner
            .get_state_summary(token, height)
            .instrument(span)
            .await
    }
}
