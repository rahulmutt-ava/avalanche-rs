// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Outbound message byte throttler (`specs/05` §5).
//!
//! Mirrors Go `network/throttling/outbound_msg_throttler.go`. Three byte pools
//! — a per-validator pool, an at-large pool, and a per-node cap on at-large
//! consumption — meter how many marshaled bytes a node may have queued for
//! sending at once.
//!
//! Unlike the inbound throttler, the outbound `acquire` is **non-blocking**: on
//! refusal it returns `None` (Go returns `false` and drops the message) instead
//! of waiting for bytes to free up.
//!
//! Every successful `acquire` yields an [`OutboundReleasePermit`] whose `Drop`
//! returns the reserved bytes to the pools. This replaces Go's `ReleaseFunc`
//! "call-exactly-once-or-leak" footgun (`specs/05` §5, §10).
//!
//! ## Simplification (this task)
//!
//! Go weights each node's validator allocation by its share of total stake.
//! This port takes the per-node validator weight as an explicit
//! `acquire_for(..., vdr_bytes)` budget and defaults [`OutboundMsgThrottler::acquire`]
//! to a non-validator (validator budget `0`), so all non-validator traffic is
//! charged to the at-large pool capped by `node_max_at_large_bytes`. Wiring the
//! validator-weight source is deferred to the validator-set integration task.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use ava_types::node_id::NodeId;

/// Default per-node validator pool size: 32 MiB (`specs/05` §5 table).
pub const DEFAULT_VDR_ALLOC_SIZE: u64 = 32 * 1024 * 1024;
/// Default at-large pool size: 32 MiB (`specs/05` §5 table).
pub const DEFAULT_AT_LARGE_ALLOC_SIZE: u64 = 32 * 1024 * 1024;
/// Default per-node at-large cap: 2 MiB (`specs/05` §5 table, node-max).
pub const DEFAULT_NODE_MAX_AT_LARGE_BYTES: u64 = 2 * 1024 * 1024;

/// Static sizing for the three outbound byte pools.
///
/// Mirrors Go `MsgByteThrottlerConfig` (the outbound subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundMsgThrottlerConfig {
    /// Total bytes reserved for the validator pool (shared across validators by
    /// stake weight). Mirrors Go `VdrAllocSize`.
    pub vdr_alloc_size: u64,
    /// Total bytes in the at-large pool (shared by all nodes). Mirrors Go
    /// `AtLargeAllocSize`.
    pub at_large_alloc_size: u64,
    /// Maximum bytes a single node may consume from the at-large pool. Mirrors
    /// Go `NodeMaxAtLargeBytes`.
    pub node_max_at_large_bytes: u64,
}

impl Default for OutboundMsgThrottlerConfig {
    fn default() -> Self {
        Self {
            vdr_alloc_size: DEFAULT_VDR_ALLOC_SIZE,
            at_large_alloc_size: DEFAULT_AT_LARGE_ALLOC_SIZE,
            node_max_at_large_bytes: DEFAULT_NODE_MAX_AT_LARGE_BYTES,
        }
    }
}

/// Mutable pool accounting, guarded by a single non-async mutex.
#[derive(Debug)]
struct Inner {
    config: OutboundMsgThrottlerConfig,
    /// Remaining bytes in the validator pool.
    remaining_vdr_bytes: u64,
    /// Remaining bytes in the at-large pool.
    remaining_at_large_bytes: u64,
    /// Per-node bytes currently charged to the at-large pool (for the node cap).
    node_to_at_large_bytes_used: HashMap<NodeId, u64>,
}

impl Inner {
    /// Attempts to reserve `msg_size` bytes for `node`, charging the validator
    /// pool first (up to `vdr_bytes`), then the at-large pool (capped by the
    /// per-node maximum). Returns the `(vdr, at_large)` split actually charged,
    /// or `None` if the request cannot be satisfied without blocking.
    fn acquire(&mut self, msg_size: u64, node: NodeId, vdr_bytes: u64) -> Option<(u64, u64)> {
        if msg_size == 0 {
            return Some((0, 0));
        }

        // Charge the validator allocation first, bounded by this node's weight
        // budget and the bytes physically remaining in the validator pool.
        let vdr_to_use = msg_size.min(vdr_bytes).min(self.remaining_vdr_bytes);

        // Whatever the validator pool can't cover must come from at-large.
        let at_large_needed = msg_size.saturating_sub(vdr_to_use);

        // The at-large draw is capped both by the node's remaining at-large
        // headroom and by the bytes physically left in the at-large pool.
        let used = self
            .node_to_at_large_bytes_used
            .get(&node)
            .copied()
            .unwrap_or(0);
        let node_headroom = self.config.node_max_at_large_bytes.saturating_sub(used);
        let at_large_available = node_headroom.min(self.remaining_at_large_bytes);

        if at_large_needed > at_large_available {
            // Cannot satisfy without waiting — drop (non-blocking refusal).
            return None;
        }

        self.remaining_vdr_bytes = self.remaining_vdr_bytes.saturating_sub(vdr_to_use);
        self.remaining_at_large_bytes = self
            .remaining_at_large_bytes
            .saturating_sub(at_large_needed);
        if at_large_needed > 0 {
            let used = self.node_to_at_large_bytes_used.entry(node).or_insert(0);
            *used = used.saturating_add(at_large_needed);
        }

        Some((vdr_to_use, at_large_needed))
    }

    /// Returns a previously-acquired `(vdr, at_large)` split to the pools.
    fn release(&mut self, vdr_bytes: u64, at_large_bytes: u64, node: NodeId) {
        self.remaining_vdr_bytes = self
            .remaining_vdr_bytes
            .saturating_add(vdr_bytes)
            .min(self.config.vdr_alloc_size);
        self.remaining_at_large_bytes = self
            .remaining_at_large_bytes
            .saturating_add(at_large_bytes)
            .min(self.config.at_large_alloc_size);

        if at_large_bytes > 0
            && let Some(used) = self.node_to_at_large_bytes_used.get_mut(&node)
        {
            *used = used.saturating_sub(at_large_bytes);
            if *used == 0 {
                self.node_to_at_large_bytes_used.remove(&node);
            }
        }
    }
}

/// Non-blocking outbound byte throttler over three byte pools.
///
/// Cheaply cloneable: clones share the same pool accounting (an `Arc<Mutex>`),
/// so a [`OutboundReleasePermit`] can hold its own handle and release on `Drop`.
#[derive(Debug, Clone)]
pub struct OutboundMsgThrottler {
    inner: Arc<Mutex<Inner>>,
}

impl OutboundMsgThrottler {
    /// Builds a throttler with all pools full.
    #[must_use]
    pub fn new(config: OutboundMsgThrottlerConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                config,
                remaining_vdr_bytes: config.vdr_alloc_size,
                remaining_at_large_bytes: config.at_large_alloc_size,
                node_to_at_large_bytes_used: HashMap::new(),
            })),
        }
    }

    /// Attempts to reserve `msg_size` bytes for a non-validator `node`
    /// (validator budget `0` — see the module simplification note).
    ///
    /// Returns an RAII permit on success, or `None` if the message must be
    /// dropped (mirrors Go `Acquire` returning `false`).
    #[must_use]
    pub fn acquire(&self, msg_size: u64, node: NodeId) -> Option<OutboundReleasePermit> {
        self.acquire_for(msg_size, node, 0)
    }

    /// Like [`Self::acquire`] but with an explicit validator-weight byte budget
    /// `vdr_bytes` (the bytes this node may draw from the validator pool).
    #[must_use]
    pub fn acquire_for(
        &self,
        msg_size: u64,
        node: NodeId,
        vdr_bytes: u64,
    ) -> Option<OutboundReleasePermit> {
        let (vdr, at_large) = self.inner.lock().acquire(msg_size, node, vdr_bytes)?;
        Some(OutboundReleasePermit {
            inner: Arc::clone(&self.inner),
            node,
            vdr_bytes: vdr,
            at_large_bytes: at_large,
        })
    }
}

/// RAII permit for a successful outbound byte acquisition.
///
/// Holding the permit keeps the reserved bytes charged; dropping it returns
/// them to the pools. Replaces Go's `ReleaseFunc`.
#[derive(Debug)]
pub struct OutboundReleasePermit {
    inner: Arc<Mutex<Inner>>,
    node: NodeId,
    vdr_bytes: u64,
    at_large_bytes: u64,
}

impl OutboundReleasePermit {
    /// The total number of bytes this permit reserves.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.vdr_bytes.saturating_add(self.at_large_bytes)
    }
}

impl Drop for OutboundReleasePermit {
    fn drop(&mut self) {
        self.inner
            .lock()
            .release(self.vdr_bytes, self.at_large_bytes, self.node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(b: u8) -> NodeId {
        NodeId::from_slice(&[b; 20]).expect("20 bytes")
    }

    #[test]
    fn acquire_and_release_round_trips_at_large() {
        let t = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig {
            vdr_alloc_size: 0,
            at_large_alloc_size: 100,
            node_max_at_large_bytes: 100,
        });
        let p = t.acquire(60, node(1)).expect("acquire");
        assert_eq!(p.size(), 60);
        // 40 left.
        assert!(t.acquire(50, node(1)).is_none());
        drop(p);
        // Full again.
        assert!(t.acquire(100, node(1)).is_some());
    }

    #[test]
    fn node_cap_limits_at_large_draw() {
        let t = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig {
            vdr_alloc_size: 0,
            at_large_alloc_size: 1000,
            node_max_at_large_bytes: 10,
        });
        let _held = t.acquire(10, node(1)).expect("first acquire");
        // Node cap reached even though the pool has room.
        assert!(t.acquire(1, node(1)).is_none());
        // A different node is unaffected.
        assert!(t.acquire(10, node(2)).is_some());
    }

    #[test]
    fn vdr_budget_is_charged_first() {
        let t = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig {
            vdr_alloc_size: 50,
            at_large_alloc_size: 0,
            node_max_at_large_bytes: 0,
        });
        // 40 bytes fit entirely in the validator budget; at-large is empty.
        let p = t.acquire_for(40, node(1), 40).expect("vdr acquire");
        assert_eq!(p.size(), 40);
        // No at-large budget for a non-validator acquire.
        assert!(t.acquire(1, node(1)).is_none());
    }

    #[test]
    fn zero_size_always_acquires() {
        let t = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig {
            vdr_alloc_size: 0,
            at_large_alloc_size: 0,
            node_max_at_large_bytes: 0,
        });
        let p = t.acquire(0, node(1)).expect("zero acquire");
        assert_eq!(p.size(), 0);
    }
}
