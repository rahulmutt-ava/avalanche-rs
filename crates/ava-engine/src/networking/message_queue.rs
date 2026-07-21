// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The per-chain bounded message queue (port of
//! `snow/networking/handler/message_queue.go`, specs 06 §5.2).
//!
//! Go uses a single queue with a sync/async split guarded by a `sync.RWMutex`
//! over the chain state. We model it as a bounded `mpsc` of [`HandlerMessage`]s
//! carrying their [`MessageClass`]; the [`ChainHandler`](super::handler::ChainHandler)
//! task drains it, running *sync* messages one-at-a-time (holding the consensus
//! state). *Async* messages are delivered the same way today (inline, in
//! order) rather than onto a concurrent worker pool — see the
//! `MessageClass::Async` arm of `ChainHandler::dispatch` for why pool-based
//! concurrency is deferred rather than invented as part of wiring `App*` ops
//! through to the VM (Task 7 follow-up).

use tokio::sync::mpsc;

use super::handler::HandlerMessage;

/// Whether a message touches consensus state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageClass {
    /// Consensus ops processed one-at-a-time, in order, holding the chain state
    /// (`Get`/`Put`/`PushQuery`/`PullQuery`/`Chits`/frontier/accepted/ancestors).
    Sync,
    /// VM-specific or cross-chain ops (`AppRequest`/`AppResponse`/`AppGossip`/
    /// `AppRequestFailed`), delivered off the consensus-serialization path.
    /// Concurrent worker-pool dispatch is the target design (Go uses a
    /// goroutine pool); today these are delivered inline, in order, on the
    /// same handler task as `Sync` — see `ChainHandler::dispatch`.
    Async,
}

/// A bounded `mpsc`-backed message queue feeding one chain handler.
///
/// The [`push`](MessageQueue::push) side is held by the router; the
/// [`recv`](MessageQueue::recv) side is owned by the handler task.
pub struct MessageQueue {
    tx: mpsc::Sender<HandlerMessage>,
}

impl MessageQueue {
    /// Create a queue with the given bound, returning the queue (push side) and
    /// the receiver (drained by the handler task).
    #[must_use]
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<HandlerMessage>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    /// Enqueue a message, awaiting capacity (back-pressure). Returns `false` if
    /// the handler task has stopped (receiver dropped).
    pub async fn push(&self, msg: HandlerMessage) -> bool {
        self.tx.send(msg).await.is_ok()
    }

    /// A cheap clone of the push side, for the router to hold per chain.
    #[must_use]
    pub fn sender(&self) -> mpsc::Sender<HandlerMessage> {
        self.tx.clone()
    }
}
