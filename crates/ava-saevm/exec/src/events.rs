// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Chain-head broadcast and the `WaitUntil{Executed,Settled}` waiters
//! (specs/11 §6, §1.5, §10 invariant 6).
//!
//! Two downstream-notification primitives the async reactor drives after a
//! block executes:
//!
//! * [`HeadEvents`] — a [`tokio::sync::broadcast`] of [`ChainHeadEvent`] (Go
//!   `event.FeedOf[T]`): one event per executed block, carrying the block's
//!   height + hash, for RPC `eth_subscribe("newHeads")`-style consumers.
//! * [`ExecutionWaiters`] — the `WaitUntilExecuted` / `WaitUntilSettled`
//!   signals (Go `chan struct{}` close fan-out), here a pair of
//!   [`tokio::sync::watch`] of the current executed / settled height.
//!
//! # Invariant 6 (atomics-before-broadcast, §10)
//!
//! The internal executed/settled **height pointer is advanced before** the
//! waiter signal fires. With the `watch`-backed implementation this is
//! structural: [`ExecutionWaiters::set_executed`] stores the new height *into*
//! the watch value, and the watch only wakes receivers after the new value is
//! visible. A waiter woken by [`wait_until_executed`](ExecutionWaiters::wait_until_executed)
//! therefore always re-reads a height `>=` the one that woke it — and any
//! consensus pointer the executor advanced *before* calling `set_executed` (per
//! the executor's `X`-step ordering) is likewise visible. A poll-after-wake can
//! never observe *less* than what the broadcast announced.

use ava_saevm_types::B256;
use tokio::sync::{broadcast, watch};

/// The default chain-head broadcast capacity. Lagging receivers (slow RPC
/// subscribers) get a `RecvError::Lagged` they handle by resyncing; the
/// executor never blocks on a slow subscriber.
const HEAD_EVENTS_CAPACITY: usize = 256;

/// A chain-head notification emitted once per executed block (Go
/// `ChainHeadEvent`, specs/11 §1.5).
///
/// Carries only chain-derived data (height + hash) — never a wall-clock
/// instant — so it is determinism-safe (specs/00 §6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainHeadEvent {
    /// The executed block's height (the new `E` frontier height).
    pub height: u64,
    /// The executed block's hash.
    pub hash: B256,
}

/// The chain-head event feed (Go `event.FeedOf[ChainHeadEvent]`).
///
/// A thin wrapper over a [`broadcast::Sender`]; the executor [`emit`](HeadEvents::emit)s
/// one [`ChainHeadEvent`] per executed block and consumers
/// [`subscribe_chain_head`](HeadEvents::subscribe_chain_head).
#[derive(Clone, Debug)]
pub struct HeadEvents {
    tx: broadcast::Sender<ChainHeadEvent>,
}

impl Default for HeadEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadEvents {
    /// Constructs a chain-head feed with the default capacity.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(HEAD_EVENTS_CAPACITY);
        Self { tx }
    }

    /// Subscribes a new receiver to the chain-head feed.
    #[must_use]
    pub fn subscribe_chain_head(&self) -> broadcast::Receiver<ChainHeadEvent> {
        self.tx.subscribe()
    }

    /// Emits a chain-head event to all current subscribers.
    ///
    /// A send with no live receivers is a no-op (not an error) — the executor
    /// emits unconditionally; whether anyone is listening is the consumer's
    /// concern.
    pub fn emit(&self, event: ChainHeadEvent) {
        // `send` errors only when there are no receivers; that is benign here.
        let _ = self.tx.send(event);
    }
}

/// The `WaitUntil{Executed,Settled}` waiters (Go `chan struct{}` close fan-out,
/// specs/11 §6, §10 invariant 6).
///
/// Two monotonically-advancing height watches. A caller awaiting a height is
/// woken once the corresponding frontier reaches it; on wake the frontier
/// pointer is already `>=` the awaited height (invariant 6 — the value is
/// stored into the watch *before* receivers are woken).
#[derive(Debug)]
pub struct ExecutionWaiters {
    executed: watch::Sender<u64>,
    settled: watch::Sender<u64>,
}

impl Default for ExecutionWaiters {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionWaiters {
    /// Constructs waiters with both frontiers at height `0`.
    #[must_use]
    pub fn new() -> Self {
        let (executed, _e) = watch::channel(0);
        let (settled, _s) = watch::channel(0);
        Self { executed, settled }
    }

    /// Advances the executed-frontier height, waking any waiter whose target is
    /// now reached.
    ///
    /// Monotonic: a `height` not greater than the current value is ignored (no
    /// spurious wake, no regression). The new value is stored *before* receivers
    /// are woken (invariant 6).
    pub fn set_executed(&self, height: u64) {
        self.executed.send_if_modified(|cur| {
            if height > *cur {
                *cur = height;
                true
            } else {
                false
            }
        });
    }

    /// Advances the settled-frontier height (see [`set_executed`](Self::set_executed)).
    pub fn set_settled(&self, height: u64) {
        self.settled.send_if_modified(|cur| {
            if height > *cur {
                *cur = height;
                true
            } else {
                false
            }
        });
    }

    /// The current executed-frontier height.
    #[must_use]
    pub fn executed_height(&self) -> u64 {
        *self.executed.borrow()
    }

    /// The current settled-frontier height.
    #[must_use]
    pub fn settled_height(&self) -> u64 {
        *self.settled.borrow()
    }

    /// Awaits until the executed frontier reaches `height`.
    ///
    /// Returns immediately if already reached. On wake the executed height is
    /// `>= height` (invariant 6).
    pub async fn wait_until_executed(&self, height: u64) {
        Self::wait_for(&self.executed, height).await;
    }

    /// Awaits until the settled frontier reaches `height`.
    pub async fn wait_until_settled(&self, height: u64) {
        Self::wait_for(&self.settled, height).await;
    }

    async fn wait_for(sender: &watch::Sender<u64>, height: u64) {
        let mut rx = sender.subscribe();
        // Fast path: already reached (marks the value seen so `changed()` only
        // fires on a genuinely newer height).
        if *rx.borrow_and_update() >= height {
            return;
        }
        loop {
            if rx.changed().await.is_err() {
                // Every sender dropped; this handle holds one, so unreachable.
                // Re-check the latest value and return rather than hang.
                if *rx.borrow() >= height {
                    return;
                }
                tokio::task::yield_now().await;
                continue;
            }
            if *rx.borrow_and_update() >= height {
                return;
            }
        }
    }
}
