// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`ChainEngine`] adapters wrapping the consensus engines so the per-chain
//! [`ChainHandler`](super::handler::ChainHandler) can drive them (M4.30a, the
//! deferred handler-driven consensus path of 06 §5.2).
//!
//! Two adapters translate the handler's object-safe [`ChainEngine`] surface onto
//! the concrete engines:
//!
//! - [`BootstrapperEngineAdapter`] wraps the [`Bootstrapper`]: its `start` hook
//!   begins frontier discovery; inbound bootstrap responses
//!   (`AcceptedFrontier`/`Accepted`/`Ancestors`/`GetAncestorsFailed`) drive the
//!   state machine; once the bootstrapper [`is_finished`](Bootstrapper::is_finished)
//!   it requests an `EngineState::NormalOp` transition on the transition channel.
//! - [`SnowmanEngineAdapter`] wraps the [`SnowmanEngine`]: inbound consensus ops
//!   (`Put`/`GetFailed`/`QueryFailed`/`PushQuery`/`PullQuery`/`Chits`) become
//!   engine calls; `gossip`/`notify(PendingTxs)` drive the steady-state loop.
//!
//! The [`ChainEngine`] trait methods return `()`; fatal engine errors are
//! surfaced via `tracing` (a handler method cannot return a `Result`). See
//! `tests/PORTING.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

use ava_snow::state::EngineState;
use ava_types::node_id::NodeId;
use ava_validators::ValidatorManager;
use ava_vm::VmEvent;
use ava_vm::block::ChainVm;

use super::handler::ChainEngine;
use super::router::InboundOp;
use crate::common::sender::Sender;
use crate::error::Result;
use crate::snowman::bootstrap::Bootstrapper;
use crate::snowman::engine::SnowmanEngine;
use crate::snowman::getter::Getter;

/// Convert a wire-relative deadline (nanoseconds from now) into a monotonic
/// [`Instant`] (determinism-lint-safe: `Instant::now()` is monotonic, never
/// wall-clock). Saturates to "now" on overflow rather than panicking.
fn deadline_from_nanos(deadline_nanos: u64) -> Instant {
    Instant::now()
        .checked_add(Duration::from_nanos(deadline_nanos))
        .unwrap_or_else(Instant::now)
}

/// Build the transition channel handed to engine adapters (`tx` clones) and the
/// [`ChainHandler`](super::handler::ChainHandler) (`rx`). An adapter sends the
/// next [`EngineState`] on `tx` to request the handler move to it.
///
/// `capacity` bounds the channel; a small value suffices (transitions are rare).
#[must_use]
pub fn transition_channel(
    capacity: usize,
) -> (mpsc::Sender<EngineState>, mpsc::Receiver<EngineState>) {
    mpsc::channel(capacity)
}

/// Log a fatal engine error (a [`ChainEngine`] method cannot return `Result`).
fn log_engine_error(op: &str, err: &crate::error::Error) {
    tracing::error!(op, error = %err, "consensus engine op failed");
}

/// Adapts a [`Bootstrapper`] to the handler's [`ChainEngine`] surface.
pub struct BootstrapperEngineAdapter<V, S> {
    boot: Bootstrapper<V, S>,
    transition: mpsc::Sender<EngineState>,
    start_req_id: u32,
    getter: Arc<Getter<V, S>>,
    /// The same typed VM `Arc` the bootstrapper/getter share (not a second
    /// wrap): App ops are forwarded straight to the VM's `AppHandler`.
    vm: Arc<AsyncMutex<V>>,
    /// Cancellation token threaded into each `AppHandler` call (`Bootstrapper`/
    /// `Getter`/`SnowmanEngine` hold their own copy internally; the adapter
    /// needs its own since `ChainEngine::handle` takes no token).
    token: CancellationToken,
}

impl<V, S> BootstrapperEngineAdapter<V, S>
where
    V: ChainVm,
    S: Sender,
{
    /// Wrap `boot`, requesting transitions on `transition`. `start_req_id` is the
    /// request id passed to [`Bootstrapper::start`] when the handler activates
    /// this engine. `getter` answers inbound `Get*` requests. `vm` is the same
    /// shared VM `Arc` the bootstrapper/getter hold — App ops are dispatched to
    /// it directly. `token` is threaded into each `AppHandler` call.
    #[must_use]
    pub fn new(
        boot: Bootstrapper<V, S>,
        transition: mpsc::Sender<EngineState>,
        start_req_id: u32,
        getter: Arc<Getter<V, S>>,
        vm: Arc<AsyncMutex<V>>,
        token: CancellationToken,
    ) -> Self {
        Self {
            boot,
            transition,
            start_req_id,
            getter,
            vm,
            token,
        }
    }

    /// If the bootstrapper has handed off, request the `NormalOp` transition.
    async fn maybe_request_normal_op(&self) {
        if self.boot.is_finished() {
            // Best-effort: if the handler stopped, the send fails harmlessly.
            let _ = self.transition.send(EngineState::NormalOp).await;
        }
    }

    /// Run `result`, logging a fatal error under `op`, then request a transition
    /// if the bootstrapper finished.
    async fn after(&self, op: &str, result: Result<()>) {
        if let Err(err) = result {
            log_engine_error(op, &err);
        }
        self.maybe_request_normal_op().await;
    }
}

#[async_trait]
impl<V, S> ChainEngine for BootstrapperEngineAdapter<V, S>
where
    V: ChainVm + Send,
    S: Sender,
{
    async fn start(&mut self) {
        let res = self.boot.start(self.start_req_id).await;
        self.after("bootstrap.start", res).await;
    }

    async fn handle(&mut self, node: NodeId, op: InboundOp) {
        match op {
            InboundOp::AcceptedFrontier {
                request_id,
                container_id,
            } => {
                let res = self
                    .boot
                    .accepted_frontier(node, request_id, container_id)
                    .await;
                self.after("bootstrap.accepted_frontier", res).await;
            }
            InboundOp::Accepted {
                request_id,
                container_ids,
            } => {
                let res = self.boot.accepted(node, request_id, &container_ids).await;
                self.after("bootstrap.accepted", res).await;
            }
            InboundOp::Ancestors {
                request_id,
                containers,
            } => {
                let res = self.boot.ancestors(node, request_id, &containers).await;
                self.after("bootstrap.ancestors", res).await;
            }
            InboundOp::GetAncestorsFailed { request_id } => {
                let res = self.boot.get_ancestors_failed(node, request_id).await;
                self.after("bootstrap.get_ancestors_failed", res).await;
            }
            // Inbound Get* requests are answered by the Getter regardless of phase.
            InboundOp::GetAcceptedFrontier { request_id } => {
                if let Err(err) = self.getter.get_accepted_frontier(node, request_id).await {
                    log_engine_error("getter.get_accepted_frontier", &err);
                }
            }
            InboundOp::GetAncestors {
                request_id,
                container_id,
            } => {
                if let Err(err) = self
                    .getter
                    .get_ancestors(node, request_id, container_id)
                    .await
                {
                    log_engine_error("getter.get_ancestors", &err);
                }
            }
            InboundOp::GetAccepted {
                request_id,
                container_ids,
            } => {
                if let Err(err) = self
                    .getter
                    .get_accepted(node, request_id, &container_ids)
                    .await
                {
                    log_engine_error("getter.get_accepted", &err);
                }
            }
            InboundOp::Get {
                request_id,
                container_id,
            } => {
                if let Err(err) = self.getter.get(node, request_id, container_id).await {
                    log_engine_error("getter.get", &err);
                }
            }
            InboundOp::GetAcceptedFrontierFailed { request_id } => {
                let res = self
                    .boot
                    .get_accepted_frontier_failed(node, request_id)
                    .await;
                self.after("bootstrap.get_accepted_frontier_failed", res)
                    .await;
            }
            InboundOp::GetAcceptedFailed { request_id } => {
                let res = self.boot.get_accepted_failed(node, request_id).await;
                self.after("bootstrap.get_accepted_failed", res).await;
            }
            // App messages are VM-defined and reach the VM directly regardless
            // of engine phase (Go dispatches AppHandler off the chain Handler,
            // not the bootstrapper/consensus engine).
            InboundOp::AppRequest {
                request_id,
                deadline_nanos,
                bytes,
            } => {
                let deadline = deadline_from_nanos(deadline_nanos);
                let mut vm = self.vm.lock().await;
                if let Err(err) = vm
                    .app_request(&self.token, node, request_id, deadline, &bytes)
                    .await
                {
                    log_engine_error("vm.app_request", &crate::error::Error::from(err));
                }
            }
            InboundOp::AppResponse { request_id, bytes } => {
                let mut vm = self.vm.lock().await;
                if let Err(err) = vm.app_response(&self.token, node, request_id, &bytes).await {
                    log_engine_error("vm.app_response", &crate::error::Error::from(err));
                }
            }
            InboundOp::AppGossip { bytes } => {
                let mut vm = self.vm.lock().await;
                if let Err(err) = vm.app_gossip(&self.token, node, &bytes).await {
                    log_engine_error("vm.app_gossip", &crate::error::Error::from(err));
                }
            }
            InboundOp::AppRequestFailed {
                request_id,
                code,
                message,
            } => {
                let mut vm = self.vm.lock().await;
                let app_err = ava_vm::app::AppError::new(code, message);
                if let Err(err) = vm
                    .app_request_failed(&self.token, node, request_id, app_err)
                    .await
                {
                    log_engine_error("vm.app_request_failed", &crate::error::Error::from(err));
                }
            }
            // Ops the bootstrapper does not consume (queries, puts, other
            // failures) are dropped: they are not part of the boot state machine.
            _ => {}
        }
    }
}

/// Adapts a [`SnowmanEngine`] to the handler's [`ChainEngine`] surface.
pub struct SnowmanEngineAdapter<V, S, M> {
    engine: SnowmanEngine<V, S, M>,
    getter: Arc<Getter<V, S>>,
    /// The same typed VM `Arc` the engine/getter share (not a second wrap):
    /// App ops are forwarded straight to the VM's `AppHandler`.
    vm: Arc<AsyncMutex<V>>,
    /// Cancellation token threaded into each `AppHandler` call.
    token: CancellationToken,
}

impl<V, S, M> SnowmanEngineAdapter<V, S, M>
where
    V: ChainVm,
    S: Sender,
    M: ValidatorManager,
{
    /// Wrap `engine` for handler dispatch in `EngineState::NormalOp`. `getter`
    /// answers inbound `Get*` requests during normal operation. `vm` is the
    /// same shared VM `Arc` the engine/getter hold — App ops are dispatched to
    /// it directly. `token` is threaded into each `AppHandler` call.
    #[must_use]
    pub fn new(
        engine: SnowmanEngine<V, S, M>,
        getter: Arc<Getter<V, S>>,
        vm: Arc<AsyncMutex<V>>,
        token: CancellationToken,
    ) -> Self {
        Self {
            engine,
            getter,
            vm,
            token,
        }
    }
}

#[async_trait]
impl<V, S, M> ChainEngine for SnowmanEngineAdapter<V, S, M>
where
    V: ChainVm + Send,
    S: Sender,
    M: ValidatorManager + Send + Sync,
{
    async fn handle(&mut self, node: NodeId, op: InboundOp) {
        // Inbound Get* requests are answered by the Getter regardless of phase.
        match op {
            InboundOp::GetAcceptedFrontier { request_id } => {
                if let Err(err) = self.getter.get_accepted_frontier(node, request_id).await {
                    log_engine_error("getter.get_accepted_frontier", &err);
                }
                return;
            }
            InboundOp::GetAncestors {
                request_id,
                container_id,
            } => {
                if let Err(err) = self
                    .getter
                    .get_ancestors(node, request_id, container_id)
                    .await
                {
                    log_engine_error("getter.get_ancestors", &err);
                }
                return;
            }
            InboundOp::GetAccepted {
                request_id,
                container_ids,
            } => {
                if let Err(err) = self
                    .getter
                    .get_accepted(node, request_id, &container_ids)
                    .await
                {
                    log_engine_error("getter.get_accepted", &err);
                }
                return;
            }
            InboundOp::Get {
                request_id,
                container_id,
            } => {
                if let Err(err) = self.getter.get(node, request_id, container_id).await {
                    log_engine_error("getter.get", &err);
                }
                return;
            }
            // non-Get* ops fall through to the consensus dispatch match below
            _ => {}
        }

        let result = match op {
            InboundOp::Put {
                request_id,
                container,
            } => self.engine.put(node, request_id, &container).await,
            InboundOp::GetFailed { request_id } => self.engine.get_failed(node, request_id).await,
            InboundOp::QueryFailed { request_id } => {
                self.engine.query_failed(node, request_id).await
            }
            InboundOp::PushQuery {
                request_id,
                container,
                requested_height,
            } => {
                self.engine
                    .push_query(node, request_id, &container, requested_height)
                    .await
            }
            InboundOp::PullQuery {
                request_id,
                container_id,
                requested_height,
            } => {
                self.engine
                    .pull_query(node, request_id, container_id, requested_height)
                    .await
            }
            InboundOp::Chits {
                request_id,
                preferred_id,
                preferred_id_at_height,
                accepted_id,
                accepted_height,
            } => {
                self.engine
                    .chits(
                        node,
                        request_id,
                        preferred_id,
                        preferred_id_at_height,
                        accepted_id,
                        accepted_height,
                    )
                    .await
            }
            // App messages are VM-defined and reach the VM directly, bypassing
            // consensus (Go dispatches AppHandler off the chain Handler, not
            // the engine).
            InboundOp::AppRequest {
                request_id,
                deadline_nanos,
                bytes,
            } => {
                let deadline = deadline_from_nanos(deadline_nanos);
                let mut vm = self.vm.lock().await;
                vm.app_request(&self.token, node, request_id, deadline, &bytes)
                    .await
                    .map_err(crate::error::Error::from)
            }
            InboundOp::AppResponse { request_id, bytes } => {
                let mut vm = self.vm.lock().await;
                vm.app_response(&self.token, node, request_id, &bytes)
                    .await
                    .map_err(crate::error::Error::from)
            }
            InboundOp::AppGossip { bytes } => {
                let mut vm = self.vm.lock().await;
                vm.app_gossip(&self.token, node, &bytes)
                    .await
                    .map_err(crate::error::Error::from)
            }
            InboundOp::AppRequestFailed {
                request_id,
                code,
                message,
            } => {
                let mut vm = self.vm.lock().await;
                let app_err = ava_vm::app::AppError::new(code, message);
                vm.app_request_failed(&self.token, node, request_id, app_err)
                    .await
                    .map_err(crate::error::Error::from)
            }
            // Bootstrap-only ops and other failures are not part of the
            // normal-operation state machine: drop them.
            _ => Ok(()),
        };
        if let Err(err) = result {
            log_engine_error("snowman.handle", &err);
        }
    }

    async fn gossip(&mut self) {
        if let Err(err) = self.engine.gossip().await {
            log_engine_error("snowman.gossip", &err);
        }
    }

    async fn notify(&mut self, event: VmEvent) {
        if let VmEvent::PendingTxs = event
            && let Err(err) = self.engine.notify_pending_txs().await
        {
            log_engine_error("snowman.notify_pending_txs", &err);
        }
    }
}
