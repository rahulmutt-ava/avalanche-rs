// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// An inner `ChainVm` that also implements `BatchedChainVm` + `StateSyncableVm`,
// so the wrapper's `as_batched`/`as_state_syncable` delegation can be asserted.
//
// Included via `include!` into a dedicated module in `vm.rs`. It is fully
// self-contained (brings its own `use`s) and wraps an `ava_vm::testutil::TestVm`
// for the base `ChainVm` surface, adding trivial batched + state-sync
// capabilities that report themselves present.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_snow::{ChainContext, EngineState};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{
    BatchedChainVm, Block, ChainVm, StateSummary, StateSyncMode, StateSyncableVm,
};
use ava_vm::connector::Connector;
use ava_vm::health::HealthCheck;
use ava_vm::testutil::TestVm;
use ava_vm::vm::{Fx, HttpHandler, Vm, VmEvent};

/// A trivial state summary.
struct TestSummary;

#[async_trait]
impl StateSummary for TestSummary {
    fn id(&self) -> Id {
        Id::EMPTY
    }
    fn height(&self) -> u64 {
        0
    }
    fn bytes(&self) -> &[u8] {
        b""
    }
    async fn accept(&self, _token: &CancellationToken) -> ava_vm::error::Result<StateSyncMode> {
        Ok(StateSyncMode::Skipped)
    }
}

/// An inner VM with batched + state-syncable capabilities.
pub struct CapableVm {
    inner: TestVm,
}

impl CapableVm {
    pub fn new() -> Self {
        Self {
            inner: TestVm::new(),
        }
    }
}

#[async_trait]
impl AppHandler for CapableVm {
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> ava_vm::error::Result<()> {
        self.inner
            .app_request(token, node, request_id, deadline, request)
            .await
    }
    async fn app_request_failed(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> ava_vm::error::Result<()> {
        self.inner
            .app_request_failed(token, node, request_id, err)
            .await
    }
    async fn app_response(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> ava_vm::error::Result<()> {
        self.inner
            .app_response(token, node, request_id, response)
            .await
    }
    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> ava_vm::error::Result<()> {
        self.inner.app_gossip(token, node, msg).await
    }
}

#[async_trait]
impl HealthCheck for CapableVm {
    async fn health_check(
        &self,
        token: &CancellationToken,
    ) -> ava_vm::error::Result<serde_json::Value> {
        self.inner.health_check(token).await
    }
}

#[async_trait]
impl Connector for CapableVm {
    async fn connected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> ava_vm::error::Result<()> {
        self.inner.connected(token, node, version).await
    }
    async fn disconnected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
    ) -> ava_vm::error::Result<()> {
        self.inner.disconnected(token, node).await
    }
}

#[async_trait]
impl Vm for CapableVm {
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
    ) -> ava_vm::error::Result<()> {
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
            .await
    }
    async fn set_state(
        &mut self,
        token: &CancellationToken,
        state: EngineState,
    ) -> ava_vm::error::Result<()> {
        self.inner.set_state(token, state).await
    }
    async fn shutdown(&mut self, token: &CancellationToken) -> ava_vm::error::Result<()> {
        self.inner.shutdown(token).await
    }
    async fn version(&self, token: &CancellationToken) -> ava_vm::error::Result<String> {
        self.inner.version(token).await
    }
    async fn create_handlers(
        &mut self,
        token: &CancellationToken,
    ) -> ava_vm::error::Result<HashMap<String, HttpHandler>> {
        self.inner.create_handlers(token).await
    }
    async fn new_http_handler(
        &mut self,
        token: &CancellationToken,
    ) -> ava_vm::error::Result<Option<HttpHandler>> {
        self.inner.new_http_handler(token).await
    }
    async fn wait_for_event(&self, token: &CancellationToken) -> ava_vm::error::Result<VmEvent> {
        self.inner.wait_for_event(token).await
    }
}

#[async_trait]
impl ChainVm for CapableVm {
    async fn build_block(
        &mut self,
        token: &CancellationToken,
    ) -> ava_vm::error::Result<Arc<dyn Block>> {
        self.inner.build_block(token).await
    }
    async fn get_block(
        &self,
        token: &CancellationToken,
        id: Id,
    ) -> ava_vm::error::Result<Arc<dyn Block>> {
        self.inner.get_block(token, id).await
    }
    async fn parse_block(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> ava_vm::error::Result<Arc<dyn Block>> {
        self.inner.parse_block(token, bytes).await
    }
    async fn set_preference(
        &mut self,
        token: &CancellationToken,
        id: Id,
    ) -> ava_vm::error::Result<()> {
        self.inner.set_preference(token, id).await
    }
    async fn last_accepted(&self, token: &CancellationToken) -> ava_vm::error::Result<Id> {
        self.inner.last_accepted(token).await
    }
    async fn get_block_id_at_height(
        &self,
        token: &CancellationToken,
        height: u64,
    ) -> ava_vm::error::Result<Id> {
        self.inner.get_block_id_at_height(token, height).await
    }

    fn as_batched(&self) -> Option<&dyn BatchedChainVm> {
        Some(self)
    }
    fn as_state_syncable(&self) -> Option<&dyn StateSyncableVm> {
        Some(self)
    }
}

#[async_trait]
impl BatchedChainVm for CapableVm {
    async fn get_ancestors(
        &self,
        _token: &CancellationToken,
        _blk_id: Id,
        _max_blocks_num: usize,
        _max_blocks_size: usize,
        _max_retrieval_time: Duration,
    ) -> ava_vm::error::Result<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
    async fn batched_parse_block(
        &self,
        _token: &CancellationToken,
        _blks: &[Vec<u8>],
    ) -> ava_vm::error::Result<Vec<Arc<dyn Block>>> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl StateSyncableVm for CapableVm {
    async fn state_sync_enabled(&self, _token: &CancellationToken) -> ava_vm::error::Result<bool> {
        Ok(false)
    }
    async fn get_ongoing_sync_state_summary(
        &self,
        _token: &CancellationToken,
    ) -> ava_vm::error::Result<Arc<dyn StateSummary>> {
        Err(ava_vm::error::Error::NotFound)
    }
    async fn get_last_state_summary(
        &self,
        _token: &CancellationToken,
    ) -> ava_vm::error::Result<Arc<dyn StateSummary>> {
        Ok(Arc::new(TestSummary))
    }
    async fn parse_state_summary(
        &self,
        _token: &CancellationToken,
        _bytes: &[u8],
    ) -> ava_vm::error::Result<Arc<dyn StateSummary>> {
        Ok(Arc::new(TestSummary))
    }
    async fn get_state_summary(
        &self,
        _token: &CancellationToken,
        _height: u64,
    ) -> ava_vm::error::Result<Arc<dyn StateSummary>> {
        Ok(Arc::new(TestSummary))
    }
}
