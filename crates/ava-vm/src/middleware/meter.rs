// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `metervm.blockVM` — the metering `ChainVm` decorator (specs 07 §6; Go
//! `vms/metervm/`).
//!
//! [`MeterVm`] wraps a [`ChainVm`] and *is* a [`ChainVm`], timing every method
//! into a per-method averager (Go's `metric.Averager`, which is a `<name>_count`
//! counter together with a `<name>_sum` gauge summing the observed nanoseconds —
//! see `utils/metric/averager.go`). The metric names mirror Go's
//! `vms/metervm/block_metrics.go` exactly (`build_block`, `parse_block`, …) so
//! the dashboards/alerts port unchanged.
//!
//! Optional inner capabilities (`BatchedChainVm` / `StateSyncableVm` /
//! `BuildBlockWithContext` / `SetPreferenceWithContext`) are probed **once at
//! construction**; the wrapper re-exposes them via the `as_*` accessors and, for
//! the batched / state-sync surfaces, re-implements the trait so the forwarded
//! calls are themselves metered (matching Go's interface embedding +
//! type-assertion). The error-only averagers (`build_block_err`, …) are only
//! observed when the inner call returns `Err`, exactly like Go.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use prometheus::{Counter, Gauge, Registry};
use tokio_util::sync::CancellationToken;

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

/// `metric.Averager` — a `<name>_count` counter + a `<name>_sum` gauge (Go
/// `utils/metric/averager.go`). `observe` adds the observed nanoseconds to the
/// sum and bumps the count by one.
#[derive(Clone)]
struct Averager {
    count: Counter,
    sum: Gauge,
}

impl Averager {
    /// Builds + registers a `<name>_count` / `<name>_sum` pair into `reg`,
    /// mirroring Go's `NewAveragerWithErrs(name, "time (in ns) of a "+name, …)`.
    fn new(name: &str, reg: &Registry) -> Result<Self> {
        let desc = format!("time (in ns) of a {name}");
        let count = Counter::with_opts(prometheus::Opts::new(
            format!("{name}_count"),
            format!("Total # of observations of {desc}"),
        ))
        .map_err(|_| Error::FailedRegistering)?;
        let sum = Gauge::with_opts(prometheus::Opts::new(
            format!("{name}_sum"),
            format!("Sum of {desc}"),
        ))
        .map_err(|_| Error::FailedRegistering)?;
        reg.register(Box::new(count.clone()))
            .map_err(|_| Error::FailedRegistering)?;
        reg.register(Box::new(sum.clone()))
            .map_err(|_| Error::FailedRegistering)?;
        Ok(Self { count, sum })
    }

    /// `Averager.Observe(float64)` — record an elapsed duration (nanoseconds).
    fn observe(&self, d: Duration) {
        self.count.inc();
        // Nanoseconds as the unit, matching Go's `float64(time.Since(start))`.
        // `as f64` is the standard float-conversion (this is a metrics path, off
        // any consensus/codec path — floats are allowed, 00 §6.1).
        #[allow(clippy::cast_precision_loss)]
        self.sum.add(d.as_nanos() as f64);
    }
}

/// `metervm.blockMetrics` — the full set of per-method averagers (specs 07 §6;
/// Go `vms/metervm/block_metrics.go`). The capability-specific averagers are
/// only registered when the inner VM supports that capability.
pub struct BlockMetrics {
    build_block: Averager,
    build_block_err: Averager,
    parse_block: Averager,
    parse_block_err: Averager,
    get_block: Averager,
    get_block_err: Averager,
    set_preference: Averager,
    last_accepted: Averager,
    get_block_id_at_height: Averager,
    // Block verification with context. Go registers these always (they meter the
    // `meterBlock` wrapper's `ShouldVerifyWithContext`/`VerifyWithContext`). This
    // port does not wrap individual blocks, so they are registered for metric
    // name-parity but observed by the block wrapper once that lands (PORTING.md).
    #[allow(dead_code)]
    should_verify_with_context: Averager,
    #[allow(dead_code)]
    verify_with_context: Averager,
    #[allow(dead_code)]
    verify_with_context_err: Averager,
    // Block building with context (only when supported).
    build_block_with_context: Option<Averager>,
    build_block_with_context_err: Option<Averager>,
    // Setting preference with context (only when supported).
    set_preference_with_context: Option<Averager>,
    // Batched (only when supported).
    get_ancestors: Option<Averager>,
    batched_parse_block: Option<Averager>,
    // State sync (only when supported).
    state_sync_enabled: Option<Averager>,
    get_ongoing_sync_state_summary: Option<Averager>,
    get_last_state_summary: Option<Averager>,
    parse_state_summary: Option<Averager>,
    parse_state_summary_err: Option<Averager>,
    get_state_summary: Option<Averager>,
    get_state_summary_err: Option<Averager>,
}

impl BlockMetrics {
    /// `blockMetrics.Initialize` — registers the always-present averagers plus
    /// the capability-gated ones (specs 07 §6; Go `block_metrics.go`).
    fn new(
        supports_build_with_context: bool,
        supports_set_preference_with_context: bool,
        supports_batched: bool,
        supports_state_sync: bool,
        reg: &Registry,
    ) -> Result<Self> {
        let build_block_with_context = if supports_build_with_context {
            Some(Averager::new("build_block_with_context", reg)?)
        } else {
            None
        };
        let build_block_with_context_err = if supports_build_with_context {
            Some(Averager::new("build_block_with_context_err", reg)?)
        } else {
            None
        };
        let set_preference_with_context = if supports_set_preference_with_context {
            Some(Averager::new("set_preference_with_context", reg)?)
        } else {
            None
        };
        let (get_ancestors, batched_parse_block) = if supports_batched {
            (
                Some(Averager::new("get_ancestors", reg)?),
                Some(Averager::new("batched_parse_block", reg)?),
            )
        } else {
            (None, None)
        };
        let (
            state_sync_enabled,
            get_ongoing_sync_state_summary,
            get_last_state_summary,
            parse_state_summary,
            parse_state_summary_err,
            get_state_summary,
            get_state_summary_err,
        ) = if supports_state_sync {
            (
                Some(Averager::new("state_sync_enabled", reg)?),
                Some(Averager::new("get_ongoing_state_sync_summary", reg)?),
                Some(Averager::new("get_last_state_summary", reg)?),
                Some(Averager::new("parse_state_summary", reg)?),
                Some(Averager::new("parse_state_summary_err", reg)?),
                Some(Averager::new("get_state_summary", reg)?),
                Some(Averager::new("get_state_summary_err", reg)?),
            )
        } else {
            (None, None, None, None, None, None, None)
        };

        Ok(Self {
            build_block: Averager::new("build_block", reg)?,
            build_block_err: Averager::new("build_block_err", reg)?,
            parse_block: Averager::new("parse_block", reg)?,
            parse_block_err: Averager::new("parse_block_err", reg)?,
            get_block: Averager::new("get_block", reg)?,
            get_block_err: Averager::new("get_block_err", reg)?,
            set_preference: Averager::new("set_preference", reg)?,
            last_accepted: Averager::new("last_accepted", reg)?,
            get_block_id_at_height: Averager::new("get_block_id_at_height", reg)?,
            should_verify_with_context: Averager::new("should_verify_with_context", reg)?,
            verify_with_context: Averager::new("verify_with_context", reg)?,
            verify_with_context_err: Averager::new("verify_with_context_err", reg)?,
            build_block_with_context,
            build_block_with_context_err,
            set_preference_with_context,
            get_ancestors,
            batched_parse_block,
            state_sync_enabled,
            get_ongoing_sync_state_summary,
            get_last_state_summary,
            parse_state_summary,
            parse_state_summary_err,
            get_state_summary,
            get_state_summary_err,
        })
    }
}

/// `metervm.blockVM` — a metering [`ChainVm`] decorator.
///
/// Wrap a VM with [`MeterVm::new`]; the wrapper times every method and re-exposes
/// the inner VM's optional capabilities (see the module docs).
pub struct MeterVm<V: ChainVm> {
    inner: V,
    metrics: BlockMetrics,
    supports_build_with_context: bool,
    supports_set_preference_with_context: bool,
    supports_batched: bool,
    supports_state_sync: bool,
}

impl<V: ChainVm> MeterVm<V> {
    /// `metervm.NewBlockVM` — probe the inner VM's capabilities once, register the
    /// metric set into `reg`, and return the metering wrapper.
    ///
    /// # Errors
    /// Returns [`Error::FailedRegistering`] if a metric collides in `reg`.
    pub fn new(inner: V, reg: &Registry) -> Result<Self> {
        let supports_build_with_context = inner.as_build_with_context().is_some();
        let supports_set_preference_with_context = inner.as_set_preference_with_context().is_some();
        let supports_batched = inner.as_batched().is_some();
        let supports_state_sync = inner.as_state_syncable().is_some();
        let metrics = BlockMetrics::new(
            supports_build_with_context,
            supports_set_preference_with_context,
            supports_batched,
            supports_state_sync,
            reg,
        )?;
        Ok(Self {
            inner,
            metrics,
            supports_build_with_context,
            supports_set_preference_with_context,
            supports_batched,
            supports_state_sync,
        })
    }

    /// Borrows the inner VM (test/introspection helper).
    #[must_use]
    pub fn inner(&self) -> &V {
        &self.inner
    }
}

#[async_trait]
impl<V: ChainVm> AppHandler for MeterVm<V> {
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> Result<()> {
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
    ) -> Result<()> {
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
    ) -> Result<()> {
        self.inner
            .app_response(token, node, request_id, response)
            .await
    }

    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> Result<()> {
        self.inner.app_gossip(token, node, msg).await
    }
}

#[async_trait]
impl<V: ChainVm> HealthCheck for MeterVm<V> {
    async fn health_check(&self, token: &CancellationToken) -> Result<serde_json::Value> {
        self.inner.health_check(token).await
    }
}

#[async_trait]
impl<V: ChainVm> Connector for MeterVm<V> {
    async fn connected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> Result<()> {
        self.inner.connected(token, node, version).await
    }

    async fn disconnected(&mut self, token: &CancellationToken, node: NodeId) -> Result<()> {
        self.inner.disconnected(token, node).await
    }
}

#[async_trait]
impl<V: ChainVm> Vm for MeterVm<V> {
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

    async fn set_state(&mut self, token: &CancellationToken, state: EngineState) -> Result<()> {
        self.inner.set_state(token, state).await
    }

    async fn shutdown(&mut self, token: &CancellationToken) -> Result<()> {
        self.inner.shutdown(token).await
    }

    async fn version(&self, token: &CancellationToken) -> Result<String> {
        self.inner.version(token).await
    }

    async fn create_handlers(
        &mut self,
        token: &CancellationToken,
    ) -> Result<HashMap<String, HttpHandler>> {
        self.inner.create_handlers(token).await
    }

    async fn new_http_handler(&mut self, token: &CancellationToken) -> Result<Option<HttpHandler>> {
        self.inner.new_http_handler(token).await
    }

    async fn wait_for_event(&self, token: &CancellationToken) -> Result<VmEvent> {
        self.inner.wait_for_event(token).await
    }
}

#[async_trait]
impl<V: ChainVm> ChainVm for MeterVm<V> {
    async fn build_block(&mut self, token: &CancellationToken) -> Result<Arc<dyn Block>> {
        let start = Instant::now();
        let res = self.inner.build_block(token).await;
        let d = start.elapsed();
        match &res {
            Ok(_) => self.metrics.build_block.observe(d),
            Err(_) => self.metrics.build_block_err.observe(d),
        }
        res
    }

    async fn get_block(&self, token: &CancellationToken, id: Id) -> Result<Arc<dyn Block>> {
        let start = Instant::now();
        let res = self.inner.get_block(token, id).await;
        let d = start.elapsed();
        match &res {
            Ok(_) => self.metrics.get_block.observe(d),
            Err(_) => self.metrics.get_block_err.observe(d),
        }
        res
    }

    async fn parse_block(&self, token: &CancellationToken, bytes: &[u8]) -> Result<Arc<dyn Block>> {
        let start = Instant::now();
        let res = self.inner.parse_block(token, bytes).await;
        let d = start.elapsed();
        match &res {
            Ok(_) => self.metrics.parse_block.observe(d),
            Err(_) => self.metrics.parse_block_err.observe(d),
        }
        res
    }

    async fn set_preference(&mut self, token: &CancellationToken, id: Id) -> Result<()> {
        let start = Instant::now();
        let res = self.inner.set_preference(token, id).await;
        self.metrics.set_preference.observe(start.elapsed());
        res
    }

    async fn last_accepted(&self, token: &CancellationToken) -> Result<Id> {
        let start = Instant::now();
        let res = self.inner.last_accepted(token).await;
        self.metrics.last_accepted.observe(start.elapsed());
        res
    }

    async fn get_block_id_at_height(&self, token: &CancellationToken, height: u64) -> Result<Id> {
        let start = Instant::now();
        let res = self.inner.get_block_id_at_height(token, height).await;
        self.metrics.get_block_id_at_height.observe(start.elapsed());
        res
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
impl<V: ChainVm> BuildBlockWithContext for MeterVm<V> {
    async fn build_block_with_context(
        &self,
        token: &CancellationToken,
        ctx: &BlockContext,
    ) -> Result<Arc<dyn Block>> {
        let inner = self
            .inner
            .as_build_with_context()
            .ok_or(Error::RemoteVmNotImplemented)?;
        let start = Instant::now();
        let res = inner.build_block_with_context(token, ctx).await;
        let d = start.elapsed();
        match (
            &res,
            &self.metrics.build_block_with_context,
            &self.metrics.build_block_with_context_err,
        ) {
            (Ok(_), Some(ok), _) => ok.observe(d),
            (Err(_), _, Some(err)) => err.observe(d),
            _ => {}
        }
        res
    }
}

#[async_trait]
impl<V: ChainVm> SetPreferenceWithContext for MeterVm<V> {
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
        let start = Instant::now();
        let res = inner.set_preference_with_context(token, id, ctx).await;
        if let Some(a) = &self.metrics.set_preference_with_context {
            a.observe(start.elapsed());
        }
        res
    }
}

#[async_trait]
impl<V: ChainVm> BatchedChainVm for MeterVm<V> {
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
        let start = Instant::now();
        let res = inner
            .get_ancestors(
                token,
                blk_id,
                max_blocks_num,
                max_blocks_size,
                max_retrieval_time,
            )
            .await;
        if let Some(a) = &self.metrics.get_ancestors {
            a.observe(start.elapsed());
        }
        res
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
        let start = Instant::now();
        let res = inner.batched_parse_block(token, blks).await;
        if let Some(a) = &self.metrics.batched_parse_block {
            a.observe(start.elapsed());
        }
        res
    }
}

#[async_trait]
impl<V: ChainVm> StateSyncableVm for MeterVm<V> {
    async fn state_sync_enabled(&self, token: &CancellationToken) -> Result<bool> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(Error::StateSyncableVmNotImplemented)?;
        let start = Instant::now();
        let res = inner.state_sync_enabled(token).await;
        if let Some(a) = &self.metrics.state_sync_enabled {
            a.observe(start.elapsed());
        }
        res
    }

    async fn get_ongoing_sync_state_summary(
        &self,
        token: &CancellationToken,
    ) -> Result<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(Error::StateSyncableVmNotImplemented)?;
        let start = Instant::now();
        let res = inner.get_ongoing_sync_state_summary(token).await;
        if let Some(a) = &self.metrics.get_ongoing_sync_state_summary {
            a.observe(start.elapsed());
        }
        res
    }

    async fn get_last_state_summary(
        &self,
        token: &CancellationToken,
    ) -> Result<Arc<dyn StateSummary>> {
        let inner = self
            .inner
            .as_state_syncable()
            .ok_or(Error::StateSyncableVmNotImplemented)?;
        let start = Instant::now();
        let res = inner.get_last_state_summary(token).await;
        if let Some(a) = &self.metrics.get_last_state_summary {
            a.observe(start.elapsed());
        }
        res
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
        let start = Instant::now();
        let res = inner.parse_state_summary(token, bytes).await;
        let d = start.elapsed();
        match (
            &res,
            &self.metrics.parse_state_summary,
            &self.metrics.parse_state_summary_err,
        ) {
            (Ok(_), Some(ok), _) => ok.observe(d),
            (Err(_), _, Some(err)) => err.observe(d),
            _ => {}
        }
        res
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
        let start = Instant::now();
        let res = inner.get_state_summary(token, height).await;
        let d = start.elapsed();
        match (
            &res,
            &self.metrics.get_state_summary,
            &self.metrics.get_state_summary_err,
        ) {
            (Ok(_), Some(ok), _) => ok.observe(d),
            (Err(_), _, Some(err)) => err.observe(d),
            _ => {}
        }
        res
    }
}
