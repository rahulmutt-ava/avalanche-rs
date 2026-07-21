// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The per-chain [`ChainHandler`] actor (port of
//! `snow/networking/handler/handler.go`, specs 06 §5.2).
//!
//! **The canonical goroutine→task mapping.** Each chain runs as **one tokio
//! task** that owns the consensus state and drains, via `tokio::select!`:
//! - a bounded `mpsc` of [`HandlerMessage`]s (sync AND async ops both dispatch
//!   inline on this task today — see the [`MessageClass::Async`] arm of
//!   [`ChainHandler::dispatch`] for why pool-based concurrent dispatch of
//!   `App*` ops is deferred rather than spawned onto the [`JoinSet`]);
//! - the VM→engine notification channel (`msg_from_vm`);
//! - a gossip ticker.
//!
//! Engine selection is by `(EngineState, EngineType)` via the [`EngineManager`].
//! A consensus message taking longer than [`SYNC_PROCESSING_TIME_WARN_LIMIT`]
//! logs a warning (Go `syncProcessingTimeWarnLimit`).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use ava_snow::state::{EngineState, EngineType};
use ava_types::node_id::NodeId;
use ava_vm::VmEvent;

use super::message_queue::MessageClass;
use super::router::{ChainMessageSink, InboundOp};

/// Go `handler.syncProcessingTimeWarnLimit` — a sync (consensus) message taking
/// longer than this logs a warning.
pub const SYNC_PROCESSING_TIME_WARN_LIMIT: Duration = Duration::from_secs(30);

/// A message handed to the chain handler task: an inbound op from a peer plus its
/// sync/async class.
#[derive(Debug)]
pub struct HandlerMessage {
    /// The (pre-authenticated) sender.
    pub node: NodeId,
    /// The op to process.
    pub op: InboundOp,
    /// Whether this op touches consensus state.
    pub class: MessageClass,
}

impl HandlerMessage {
    /// Classify an op into sync (consensus, serialized) vs. async (`App*`;
    /// delivered inline today — see [`MessageClass::Async`]).
    #[must_use]
    pub fn classify(node: NodeId, op: InboundOp) -> Self {
        let class = match &op {
            InboundOp::AppRequestFailed { .. }
            | InboundOp::AppRequest { .. }
            | InboundOp::AppResponse { .. }
            | InboundOp::AppGossip { .. } => MessageClass::Async,
            _ => MessageClass::Sync,
        };
        Self { node, op, class }
    }
}

/// Trait the active engine implements to process a routed op. Kept minimal and
/// object-safe so the handler can dispatch `&mut dyn ChainEngine` per
/// `(EngineState, EngineType)`; the full `Engine`/`Handler` family (06 §4.1)
/// composes onto this in M3.11+.
#[async_trait]
pub trait ChainEngine: Send {
    /// Activation hook (Go `Engine.Start`). Called by the handler on the engine
    /// active when the loop begins, and on the newly-active engine after every
    /// state transition. Default no-op; the bootstrapper adapter overrides it to
    /// begin frontier discovery.
    async fn start(&mut self) {}

    /// Process one inbound op from `node`.
    async fn handle(&mut self, node: NodeId, op: InboundOp);

    /// Periodic gossip tick (Go `Gossip`).
    async fn gossip(&mut self) {}

    /// A VM→engine notification (`PendingTxs`/`StateSyncDone`).
    async fn notify(&mut self, _event: VmEvent) {}
}

/// `EngineManager` — the `{state_syncer, bootstrapper, consensus}` per
/// [`EngineType`] selector. Dispatch picks the engine for the current
/// `(EngineState, EngineType)`; an op tagged for an inactive engine is dropped.
pub struct EngineManager {
    engines: HashMap<(EngineState, EngineType), Box<dyn ChainEngine>>,
    engine_type: EngineType,
}

impl EngineManager {
    /// Build an empty manager for the given default engine type.
    #[must_use]
    pub fn new(engine_type: EngineType) -> Self {
        Self {
            engines: HashMap::new(),
            engine_type,
        }
    }

    /// Register the engine handling `state` for the manager's engine type.
    pub fn register(&mut self, state: EngineState, engine: Box<dyn ChainEngine>) {
        self.engines.insert((state, self.engine_type), engine);
    }

    /// The engine active in `state` (if any).
    fn active_mut(&mut self, state: EngineState) -> Option<&mut Box<dyn ChainEngine>> {
        self.engines.get_mut(&(state, self.engine_type))
    }
}

/// The push side handed to the router: a bounded `mpsc` sender plus the chain's
/// current-state reader.
#[derive(Clone)]
pub struct ChainHandlerSink {
    tx: mpsc::Sender<HandlerMessage>,
}

#[async_trait]
impl ChainMessageSink for ChainHandlerSink {
    async fn push(&self, node: NodeId, op: InboundOp) {
        let msg = HandlerMessage::classify(node, op);
        // Back-pressure: await capacity. Drop silently if the handler stopped.
        let _ = self.tx.send(msg).await;
    }
}

/// The per-chain handler actor.
pub struct ChainHandler {
    engines: EngineManager,
    state: EngineState,
    queue_rx: mpsc::Receiver<HandlerMessage>,
    msg_from_vm: mpsc::Receiver<VmEvent>,
    /// Engine-requested state transitions (Go `Handler.transitionTo`). An active
    /// engine adapter sends the next [`EngineState`] here; on receipt the handler
    /// switches the active engine and calls its `start` hook.
    transition_rx: mpsc::Receiver<EngineState>,
    gossip_frequency: Duration,
    halt: CancellationToken,
    tracker: TaskTracker,
}

impl ChainHandler {
    /// Build a handler + its push-side sink and VM-notification sender.
    ///
    /// `queue_capacity` bounds the inbound message queue; `gossip_frequency`
    /// drives the gossip ticker. `transition_rx` is the receiver end of a
    /// [`transition_channel`](super::engine_adapter::transition_channel) whose
    /// `tx` clones are handed to the engine adapters before registration, so an
    /// active engine can request the handler move to a new
    /// [`EngineState`] (e.g. the bootstrapper requesting `NormalOp`).
    #[must_use]
    pub fn new(
        engines: EngineManager,
        initial_state: EngineState,
        queue_capacity: usize,
        gossip_frequency: Duration,
        halt: CancellationToken,
        transition_rx: mpsc::Receiver<EngineState>,
    ) -> (Self, ChainHandlerSink, mpsc::Sender<VmEvent>) {
        let (tx, queue_rx) = mpsc::channel(queue_capacity);
        let (vm_tx, msg_from_vm) = mpsc::channel(queue_capacity);
        let handler = Self {
            engines,
            state: initial_state,
            queue_rx,
            msg_from_vm,
            transition_rx,
            gossip_frequency,
            halt,
            tracker: TaskTracker::new(),
        };
        (handler, ChainHandlerSink { tx }, vm_tx)
    }

    /// Set the engine phase the handler dispatches to.
    pub fn set_state(&mut self, state: EngineState) {
        self.state = state;
    }

    /// A handle to the task tracker, so callers can `wait()` for all async
    /// (`App*`) workers to drain after shutdown (leaked-task assertion in tests).
    #[must_use]
    pub fn task_tracker(&self) -> TaskTracker {
        self.tracker.clone()
    }

    /// Spawn the handler as one tokio task. It drains the queue + VM channel +
    /// gossip ticker via `tokio::select!`, dispatching to the active engine, until
    /// `halt` is cancelled.
    pub fn start(self) -> JoinHandle<()> {
        tokio::spawn(self.run())
    }

    async fn run(mut self) {
        let mut gossip = tokio::time::interval(self.gossip_frequency);
        // Don't fire a burst if ticks are missed (timeouts/pauses).
        gossip.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Bounded async worker pool for App* messages.
        let mut async_pool: JoinSet<()> = JoinSet::new();

        // Activate the initially-selected engine (Go `Engine.Start`).
        if let Some(engine) = self.engines.active_mut(self.state) {
            engine.start().await;
        }

        // Once all transition senders drop, `recv()` is permanently ready with
        // `None`; gate the arm off so `biased` select doesn't busy-loop on it.
        let mut transition_open = true;

        loop {
            tokio::select! {
                biased;
                () = self.halt.cancelled() => break,
                maybe = self.transition_rx.recv(), if transition_open => {
                    match maybe {
                        Some(new_state) => {
                            self.set_state(new_state);
                            if let Some(engine) = self.engines.active_mut(self.state) {
                                engine.start().await;
                            }
                        }
                        // All transition senders dropped: keep running in the
                        // current state (engines may still process ops).
                        None => transition_open = false,
                    }
                }
                maybe = self.queue_rx.recv() => {
                    match maybe {
                        Some(msg) => self.dispatch(msg, &mut async_pool).await,
                        None => break, // all senders dropped
                    }
                }
                maybe = self.msg_from_vm.recv() => {
                    if let Some(event) = maybe
                        && let Some(engine) = self.engines.active_mut(self.state)
                    {
                        engine.notify(event).await;
                    }
                }
                _ = gossip.tick() => {
                    if let Some(engine) = self.engines.active_mut(self.state) {
                        engine.gossip().await;
                    }
                }
                // Reap finished async workers so the JoinSet doesn't grow unbounded.
                Some(_) = async_pool.join_next(), if !async_pool.is_empty() => {}
            }
        }

        // Shutdown: stop accepting new async work and drain.
        self.tracker.close();
        async_pool.shutdown().await;
        self.tracker.wait().await;
    }

    /// Dispatch one message to the active engine's [`ChainEngine::handle`].
    ///
    /// Both `Sync` and `Async` classes are delivered INLINE on this task today
    /// (see the `MessageClass::Async` arm below for why `_async_pool` is not
    /// spawned onto). This fixes a review finding (Task 7 follow-up): the
    /// `Async` arm used to be a placeholder (`let _ = (node, op);`) that never
    /// called `engine.handle()`, so every `App*` op reaching the handler via
    /// the real `decode -> router -> sink -> dispatch` path was silently
    /// dropped — only direct `adapter.handle()` unit tests exercised the
    /// adapters' App arms.
    async fn dispatch(&mut self, msg: HandlerMessage, _async_pool: &mut JoinSet<()>) {
        match msg.class {
            MessageClass::Sync => {
                let start = tokio::time::Instant::now();
                if let Some(engine) = self.engines.active_mut(self.state) {
                    engine.handle(msg.node, msg.op).await;
                }
                let elapsed = start.elapsed();
                if elapsed > SYNC_PROCESSING_TIME_WARN_LIMIT {
                    tracing::warn!(
                        ?elapsed,
                        limit = ?SYNC_PROCESSING_TIME_WARN_LIMIT,
                        "consensus message processing exceeded sync warn limit"
                    );
                }
            }
            MessageClass::Async => {
                // App* ops are delivered to the active engine adapter's
                // `handle()` INLINE, exactly like Sync, rather than spawned
                // onto `_async_pool`. Spawning would need a `'static` future
                // holding a mutable borrow of the boxed active `dyn
                // ChainEngine`, which `EngineManager` does not expose (it owns
                // engines directly, not behind `Arc<Mutex<..>>`); giving it
                // that shape is a bigger architecture change, deferred rather
                // than invented here. This costs nothing in practice: every
                // App op already funnels through the SAME
                // `Arc<tokio::sync::Mutex<V>>` the adapter holds
                // (`engine_adapter.rs`), so cross-op ordering into the VM is
                // serialized at that mutex regardless of whether the
                // *handler* dispatches concurrently or serially. When
                // pool-based concurrent dispatch is implemented, reintroduce
                // `_async_pool.spawn(self.tracker.track_future(..))` here.
                if let Some(engine) = self.engines.active_mut(self.state) {
                    engine.handle(msg.node, msg.op).await;
                }
            }
        }
    }
}
