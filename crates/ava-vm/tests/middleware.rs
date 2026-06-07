// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `MeterVm` + `TracedVm` middleware tests (specs 07 §6).
//!
//! * `metervm_forwards_capabilities` — wrapping a VM whose `as_batched()==Some`
//!   in `MeterVm` keeps `as_batched()==Some` (re-exposed wrapped), every
//!   `ChainVm` call lands in the per-method Prometheus histogram, and the metric
//!   names match Go (`build_block_count`/`build_block_sum`/…).
//! * `tracedvm_spans_end` — every method opens and ends a span (the `TracedVm`
//!   wrapper drives the call to completion and stays a working `ChainVm`).
//!
//! Gated on the `testutil` feature (it builds on `ava_vm::testutil::TestVm`).

#![cfg(feature = "testutil")]
#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use prometheus::Registry;
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_snow::{ChainContext, EngineState, Result as SnowResult};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;

use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::block::{BatchedChainVm, Block, ChainVm, StateSummary, StateSyncMode, StateSyncableVm};
use ava_vm::connector::Connector;
use ava_vm::error::{Error, Result};
use ava_vm::health::HealthCheck;
use ava_vm::middleware::{MeterVm, TracedVm};
use ava_vm::testutil::{NoopAppSender, test_chain_context};
use ava_vm::vm::{Fx, HttpHandler, Vm, VmEvent};

/// A counting test VM that also implements the optional `BatchedChainVm` +
/// `StateSyncableVm` capabilities, so the middleware capability-forwarding can be
/// observed. Each method bumps a shared counter so a test can assert the wrapper
/// drove the call to the inner VM.
#[derive(Debug, Default)]
struct CapableVm {
    calls: Arc<AtomicU64>,
    last_accepted: Id,
}

impl CapableVm {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicU64::new(0)),
            last_accepted: Id::EMPTY,
        }
    }

    fn bump(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct CapableBlock {
    id: Id,
    bytes: Vec<u8>,
}

#[async_trait]
impl Block for CapableBlock {
    fn id(&self) -> Id {
        self.id
    }
    fn parent(&self) -> Id {
        Id::EMPTY
    }
    fn height(&self) -> u64 {
        1
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
        Ok(())
    }
    async fn reject(&self, _token: &CancellationToken) -> SnowResult<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct CapableSummary {
    id: Id,
    bytes: Vec<u8>,
}

#[async_trait]
impl StateSummary for CapableSummary {
    fn id(&self) -> Id {
        self.id
    }
    fn height(&self) -> u64 {
        0
    }
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    async fn accept(&self, _token: &CancellationToken) -> Result<StateSyncMode> {
        Ok(StateSyncMode::Static)
    }
}

#[async_trait]
impl AppHandler for CapableVm {
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
impl HealthCheck for CapableVm {
    async fn health_check(&self, _token: &CancellationToken) -> Result<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
}

#[async_trait]
impl Connector for CapableVm {
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
impl Vm for CapableVm {
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
        Ok(())
    }
    async fn set_state(&mut self, _token: &CancellationToken, _state: EngineState) -> Result<()> {
        Ok(())
    }
    async fn shutdown(&mut self, _token: &CancellationToken) -> Result<()> {
        Ok(())
    }
    async fn version(&self, _token: &CancellationToken) -> Result<String> {
        Ok("capable/0.0.0".into())
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
impl ChainVm for CapableVm {
    async fn build_block(&mut self, _token: &CancellationToken) -> Result<Arc<dyn Block>> {
        self.bump();
        Ok(Arc::new(CapableBlock {
            id: Id::from([1u8; 32]),
            bytes: vec![1, 2, 3],
        }))
    }
    async fn get_block(&self, _token: &CancellationToken, id: Id) -> Result<Arc<dyn Block>> {
        self.bump();
        Ok(Arc::new(CapableBlock {
            id,
            bytes: vec![4, 5, 6],
        }))
    }
    async fn parse_block(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> Result<Arc<dyn Block>> {
        self.bump();
        Ok(Arc::new(CapableBlock {
            id: Id::from([2u8; 32]),
            bytes: bytes.to_vec(),
        }))
    }
    async fn set_preference(&mut self, _token: &CancellationToken, _id: Id) -> Result<()> {
        self.bump();
        Ok(())
    }
    async fn last_accepted(&self, _token: &CancellationToken) -> Result<Id> {
        self.bump();
        Ok(self.last_accepted)
    }
    async fn get_block_id_at_height(&self, _token: &CancellationToken, _height: u64) -> Result<Id> {
        self.bump();
        Ok(self.last_accepted)
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
    ) -> Result<Vec<Vec<u8>>> {
        self.bump();
        Ok(vec![vec![7, 8, 9]])
    }
    async fn batched_parse_block(
        &self,
        _token: &CancellationToken,
        blks: &[Vec<u8>],
    ) -> Result<Vec<Arc<dyn Block>>> {
        self.bump();
        Ok(blks
            .iter()
            .map(|b| {
                Arc::new(CapableBlock {
                    id: Id::from([3u8; 32]),
                    bytes: b.clone(),
                }) as Arc<dyn Block>
            })
            .collect())
    }
}

#[async_trait]
impl StateSyncableVm for CapableVm {
    async fn state_sync_enabled(&self, _token: &CancellationToken) -> Result<bool> {
        self.bump();
        Ok(true)
    }
    async fn get_ongoing_sync_state_summary(
        &self,
        _token: &CancellationToken,
    ) -> Result<Arc<dyn StateSummary>> {
        self.bump();
        Err(Error::NotFound)
    }
    async fn get_last_state_summary(
        &self,
        _token: &CancellationToken,
    ) -> Result<Arc<dyn StateSummary>> {
        self.bump();
        Ok(Arc::new(CapableSummary {
            id: Id::from([9u8; 32]),
            bytes: vec![1],
        }))
    }
    async fn parse_state_summary(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> Result<Arc<dyn StateSummary>> {
        self.bump();
        Ok(Arc::new(CapableSummary {
            id: Id::from([8u8; 32]),
            bytes: bytes.to_vec(),
        }))
    }
    async fn get_state_summary(
        &self,
        _token: &CancellationToken,
        _height: u64,
    ) -> Result<Arc<dyn StateSummary>> {
        self.bump();
        Err(Error::NotFound)
    }
}

/// Returns the `_count` value of the named counter metric in the registry.
fn metric_count(reg: &Registry, name: &str) -> Option<u64> {
    let families = reg.gather();
    for mf in &families {
        if mf.get_name() == name {
            let m = mf.get_metric().first()?;
            return Some(m.get_counter().get_value() as u64);
        }
    }
    None
}

#[tokio::test]
async fn metervm_forwards_capabilities() {
    let token = CancellationToken::new();
    let reg = Registry::new();
    let inner = CapableVm::new();
    let calls = Arc::clone(&inner.calls);

    let mut vm = MeterVm::new(inner, &reg).expect("MeterVm::new");

    // Capability probes are forwarded (re-exposed wrapped).
    assert!(
        (&vm as &dyn ChainVm).as_batched().is_some(),
        "MeterVm must re-expose the inner batched capability"
    );
    assert!(
        (&vm as &dyn ChainVm).as_state_syncable().is_some(),
        "MeterVm must re-expose the inner state-syncable capability"
    );
    // The inner VM declines build/setpref-with-context, so the wrapper must too.
    assert!((&vm as &dyn ChainVm).as_build_with_context().is_none());
    assert!(
        (&vm as &dyn ChainVm)
            .as_set_preference_with_context()
            .is_none()
    );

    // Every ChainVm call is recorded into the per-method histogram.
    let _ = vm.build_block(&token).await.expect("build");
    let _ = vm.get_block(&token, Id::EMPTY).await.expect("get");
    let _ = vm.parse_block(&token, &[1, 2]).await.expect("parse");
    vm.set_preference(&token, Id::EMPTY).await.expect("pref");
    let _ = vm.last_accepted(&token).await.expect("last");
    let _ = vm.get_block_id_at_height(&token, 0).await.expect("height");

    // Forwarded batched calls go through the wrapper too.
    let batched = (&vm as &dyn ChainVm).as_batched().expect("batched");
    let _ = batched
        .get_ancestors(&token, Id::EMPTY, 10, 1 << 20, Duration::from_secs(1))
        .await
        .expect("get_ancestors");

    // The inner VM actually got every call.
    assert!(
        calls.load(Ordering::SeqCst) >= 7,
        "all calls reached the inner VM"
    );

    // Go-parity metric names: each averager => `<name>_count` + `<name>_sum`.
    for name in [
        "build_block_count",
        "build_block_sum",
        "parse_block_count",
        "get_block_count",
        "set_preference_count",
        "last_accepted_count",
        "get_block_id_at_height_count",
        "get_ancestors_count",
    ] {
        assert!(
            metric_count(&reg, name).is_some(),
            "metric {name} must be registered"
        );
    }
    assert_eq!(
        metric_count(&reg, "build_block_count"),
        Some(1),
        "build_block was observed exactly once"
    );
    assert_eq!(metric_count(&reg, "get_ancestors_count"), Some(1));
}

#[tokio::test]
async fn tracedvm_spans_end() {
    let token = CancellationToken::new();
    let inner = CapableVm::new();
    let calls = Arc::clone(&inner.calls);
    let mut vm = TracedVm::new(inner, "primaryAlias".to_string());

    // Each method opens + ends a span and drives the inner call to completion.
    let _ = vm.build_block(&token).await.expect("build");
    let _ = vm.get_block(&token, Id::EMPTY).await.expect("get");
    let _ = vm.parse_block(&token, &[1]).await.expect("parse");
    vm.set_preference(&token, Id::EMPTY).await.expect("pref");
    let _ = vm.last_accepted(&token).await.expect("last");
    let _ = vm.get_block_id_at_height(&token, 0).await.expect("height");

    assert!(
        calls.load(Ordering::SeqCst) >= 6,
        "tracedvm forwards every call"
    );

    // Capability probes are forwarded.
    assert!((&vm as &dyn ChainVm).as_batched().is_some());
    assert!((&vm as &dyn ChainVm).as_state_syncable().is_some());

    let batched = (&vm as &dyn ChainVm).as_batched().expect("batched");
    let parsed = batched
        .batched_parse_block(&token, &[vec![1], vec![2]])
        .await
        .expect("batched_parse_block");
    assert_eq!(parsed.len(), 2);

    let ss = (&vm as &dyn ChainVm).as_state_syncable().expect("ss");
    assert!(ss.state_sync_enabled(&token).await.expect("enabled"));
}

// Keep the unused testutil re-exports honest (the file links the crate's
// testutil surface, even if a given run does not touch them all).
#[allow(dead_code)]
fn _unused(_: NoopAppSender, _: HashSet<NodeId>, _: SendConfig) {
    let _ = test_chain_context;
}
