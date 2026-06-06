// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Outbound message queue (`specs/05` §3.3).
//!
//! Mirrors Go `network/peer/message_queue.go`. Two implementations sit behind
//! the [`MessageQueue`] trait:
//!
//! - [`ThrottledMessageQueue`] — the default per-peer queue: an unbounded
//!   [`VecDeque`] guarded by a [`parking_lot::Mutex`] plus a
//!   [`tokio::sync::Notify`] (replacing Go's `sync.Cond`). On `push` it first
//!   reserves bytes from the [`OutboundMsgThrottler`] (skipping the reservation
//!   for `bypass_throttling` messages); the [`OutboundReleasePermit`] is held
//!   alongside the queued message and released on pop/drop/close.
//! - [`BlockingMessageQueue`] — a bounded [`tokio::sync::mpsc`] channel used by
//!   tests and special senders.
//!
//! **Locking discipline (`specs/17` §7):** the synchronous deque mutex is never
//! held across an `.await`. `pop` registers its `Notify` waiter, releases the
//! lock, and only then awaits.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc};

use ava_message::codec::OutboundMessage;
use ava_types::node_id::NodeId;

use crate::throttling::outbound_msg::{OutboundMsgThrottler, OutboundReleasePermit};

/// An outbound message queue.
///
/// Mirrors the Go `MessageQueue` interface (`network/peer/message_queue.go`).
#[async_trait]
pub trait MessageQueue: Send + Sync {
    /// Enqueues `msg`. Returns `false` if the message was dropped (the queue is
    /// closed, full, or the throttler refused the bytes).
    fn push(&self, msg: OutboundMessage) -> bool;

    /// Awaits and removes the next message in FIFO order. Returns `None` once
    /// the queue is closed and fully drained.
    async fn pop(&self) -> Option<OutboundMessage>;

    /// Removes and returns the next message without blocking, or `None` if the
    /// queue is currently empty.
    fn pop_now(&self) -> Option<OutboundMessage>;

    /// Closes the queue. Subsequent `push`es are rejected; pending messages
    /// remain poppable until drained, after which `pop` returns `None`.
    fn close(&self);
}

/// A queued message together with the byte permit reserved for it.
///
/// Dropping the entry (on pop/close) releases the throttler bytes via the
/// permit's `Drop`.
struct Entry {
    msg: OutboundMessage,
    /// `None` for `bypass_throttling` messages, which never reserve bytes.
    _permit: Option<OutboundReleasePermit>,
}

/// Shared, lock-guarded queue state.
struct State {
    deque: VecDeque<Entry>,
    closed: bool,
}

/// The default per-peer outbound queue: unbounded FIFO + byte throttling.
///
/// Mirrors Go `throttledMessageQueue`.
pub struct ThrottledMessageQueue {
    state: Mutex<State>,
    notify: Notify,
    throttler: OutboundMsgThrottler,
    node: NodeId,
}

impl ThrottledMessageQueue {
    /// Builds a queue for `node`, metered by `throttler`.
    #[must_use]
    pub fn new(throttler: OutboundMsgThrottler, node: NodeId) -> Self {
        Self {
            state: Mutex::new(State {
                deque: VecDeque::new(),
                closed: false,
            }),
            notify: Notify::new(),
            throttler,
            node,
        }
    }

    /// The message byte length charged to the throttler. Mirrors Go's use of
    /// `len(msg.Bytes())`.
    fn msg_len(msg: &OutboundMessage) -> u64 {
        msg.bytes.len() as u64
    }
}

#[async_trait]
impl MessageQueue for ThrottledMessageQueue {
    fn push(&self, msg: OutboundMessage) -> bool {
        // `bypass_throttling` messages (handshake / PeerList replies) skip the
        // byte reservation entirely and are always enqueued (unless closed).
        let permit = if msg.bypass_throttling {
            None
        } else {
            match self.throttler.acquire(Self::msg_len(&msg), self.node) {
                Some(p) => Some(p),
                // Throttler refused: drop the message.
                None => return false,
            }
        };

        {
            let mut state = self.state.lock();
            if state.closed {
                // Dropping `permit` here releases any reserved bytes.
                return false;
            }
            state.deque.push_back(Entry {
                msg,
                _permit: permit,
            });
        }
        // Wake a single waiting `pop`.
        self.notify.notify_one();
        true
    }

    async fn pop(&self) -> Option<OutboundMessage> {
        loop {
            // Register the waiter BEFORE inspecting the deque so a concurrent
            // `push`/`close` between the check and the await cannot be missed.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            {
                let mut state = self.state.lock();
                if let Some(entry) = state.deque.pop_front() {
                    // `entry._permit` drops here, releasing the bytes.
                    return Some(entry.msg);
                }
                if state.closed {
                    return None;
                }
            }

            notified.await;
        }
    }

    fn pop_now(&self) -> Option<OutboundMessage> {
        let mut state = self.state.lock();
        state.deque.pop_front().map(|entry| entry.msg)
    }

    fn close(&self) {
        {
            let mut state = self.state.lock();
            state.closed = true;
        }
        // Wake all waiters so drained `pop`s can observe the close.
        self.notify.notify_waiters();
    }
}

/// A bounded outbound queue backed by a [`tokio::sync::mpsc`] channel.
///
/// Mirrors Go `blockingMessageQueue` — used by tests and special senders. It
/// performs no byte throttling; back-pressure is the channel capacity.
pub struct BlockingMessageQueue {
    sender: mpsc::Sender<OutboundMessage>,
    receiver: AsyncMutex<mpsc::Receiver<OutboundMessage>>,
}

impl BlockingMessageQueue {
    /// Builds a queue with the given channel `capacity`.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        Self {
            sender,
            receiver: AsyncMutex::new(receiver),
        }
    }
}

#[async_trait]
impl MessageQueue for BlockingMessageQueue {
    fn push(&self, msg: OutboundMessage) -> bool {
        // Non-blocking: drops if the channel is full or closed.
        self.sender.try_send(msg).is_ok()
    }

    async fn pop(&self) -> Option<OutboundMessage> {
        // Single-consumer by contract; the async mutex guards the `&self`
        // borrow of the receiver. The tokio mutex is await-safe to hold here.
        let mut guard = self.receiver.lock().await;
        guard.recv().await
    }

    fn pop_now(&self) -> Option<OutboundMessage> {
        match self.receiver.try_lock() {
            Ok(mut guard) => guard.try_recv().ok(),
            Err(_) => None,
        }
    }

    fn close(&self) {
        // Closing the receiver causes senders' `try_send` to fail.
        if let Ok(mut guard) = self.receiver.try_lock() {
            guard.close();
        }
    }
}

/// Convenience alias for a shared, dyn-dispatched queue handle.
pub type SharedMessageQueue = Arc<dyn MessageQueue>;

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use ava_message::ops::Op;

    use super::*;
    use crate::throttling::outbound_msg::OutboundMsgThrottlerConfig;

    fn node(b: u8) -> NodeId {
        NodeId::from_slice(&[b; 20]).expect("20 bytes")
    }

    fn msg(len: usize) -> OutboundMessage {
        OutboundMessage {
            bypass_throttling: false,
            op: Op::Ping,
            bytes: Bytes::from(vec![0u8; len]),
            bytes_saved_compression: 0,
        }
    }

    #[tokio::test]
    async fn blocking_queue_push_pop() {
        let q = BlockingMessageQueue::new(2);
        assert!(q.push(msg(3)));
        assert_eq!(q.pop().await.expect("msg").bytes.len(), 3);
    }

    #[tokio::test]
    async fn blocking_queue_drops_when_full() {
        let q = BlockingMessageQueue::new(1);
        assert!(q.push(msg(1)));
        assert!(!q.push(msg(1)));
    }

    #[tokio::test]
    async fn pop_blocks_until_push() {
        let throttler = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig::default());
        let q = Arc::new(ThrottledMessageQueue::new(throttler, node(1)));
        let q2 = Arc::clone(&q);
        let handle = tokio::spawn(async move { q2.pop().await.map(|m| m.bytes.len()) });
        // Give the popper a chance to park.
        tokio::task::yield_now().await;
        assert!(q.push(msg(7)));
        assert_eq!(handle.await.expect("join"), Some(7));
    }
}
