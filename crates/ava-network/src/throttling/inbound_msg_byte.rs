// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Inbound message byte throttler.
//!
//! Port of `network/throttling/inbound_msg_byte_throttler.go`. Meters the total
//! number of *inbound message bytes* a node is allowed to have in flight, so a
//! single peer cannot exhaust memory. There are two byte pools:
//!
//! - **At-large pool** (`AtLargeAllocSize`, default `6 MiB`): shared by every
//!   node, capped per node by `nodeMaxAtLargeBytes` (default `2 MiB`, the max
//!   message size).
//! - **Validator pool** (`VdrAllocSize`, default `32 MiB`): a node draws from a
//!   slice of this pool proportional to its stake weight, *after* the at-large
//!   pool. See the simplification note below.
//!
//! [`InboundMsgByteThrottler::acquire`] returns once `size` bytes are available
//! for the node; the returned [`ReleasePermit`] releases those bytes on `Drop`,
//! replacing Go's `ReleaseFunc` footgun with RAII.
//!
//! ## Fairness / progress guarantee
//!
//! Mirrors Go's `waitingToAcquire` + `nodeToWaitingMsgID`:
//!
//! - A node may have **at most one outstanding (blocked) acquire** at a time.
//!   A second concurrent acquire by the same node is rejected (returns `None`),
//!   matching Go's "node already waiting on message" error path.
//! - Waiters are served **oldest-first** when bytes are released, so a slow
//!   node cannot starve fast nodes and every blocked acquire makes progress.
//! - On release, freed bytes first satisfy this node's own validator-pool
//!   waiter, then any waiter (oldest-first) from the at-large pool — exactly
//!   the two-stage hand-back Go performs.
//!
//! ## Simplification (validator weighting)
//!
//! Go computes a node's validator allocation as
//! `maxVdrBytes * weight / totalWeight` via a `validators.Manager`. That
//! manager is not available to this crate yet (M2.13 is self-contained). This
//! port accepts a **per-node validator-byte allowance** at acquire time (the
//! `vdr_alloc` parameter), defaulting non-validators to `0` so they draw purely
//! from the at-large pool. Callers that have a `validators.Manager` compute
//! `max_vdr_bytes * weight / total_weight` and pass it as `vdr_alloc`; the
//! pool accounting (`remaining_vdr_bytes`, `node_to_vdr_bytes_used`) is
//! otherwise byte-for-byte the Go logic. See `specs/05` §5.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_types::node_id::NodeId;
use parking_lot::Mutex;
use prometheus::IntGauge;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// The two inbound-byte "remaining" gauges (`specs/18` §2.3), pushed by the
/// throttler whenever a pool balance changes:
/// `byte_throttler_inbound_remaining_at_large_bytes` and
/// `byte_throttler_inbound_remaining_validator_bytes`.
#[derive(Clone, Debug)]
struct RemainingGauges {
    at_large: IntGauge,
    validator: IntGauge,
}

/// Fair, blocking inbound message byte throttler. Clone to share; clones refer
/// to the same pools.
#[derive(Clone, Debug)]
pub struct InboundMsgByteThrottler {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    /// Bytes currently free in the validator pool.
    remaining_vdr_bytes: u64,
    /// Bytes currently free in the at-large pool.
    remaining_at_large_bytes: u64,
    /// Per-node cap on bytes taken from the at-large pool.
    node_max_at_large_bytes: u64,

    /// node -> bytes currently charged to its validator allocation.
    node_to_vdr_bytes_used: BTreeMap<NodeId, u64>,
    /// node -> bytes currently charged to the at-large pool.
    node_to_at_large_bytes_used: BTreeMap<NodeId, u64>,

    /// Monotonic id assigned to each blocked acquire.
    next_msg_id: u64,
    /// node -> id of the (single) message it is blocked waiting to acquire.
    node_to_waiting_msg_id: BTreeMap<NodeId, u64>,
    /// msg id -> waiter metadata. `BTreeMap` iterates in ascending key order,
    /// which is insertion order here (ids are monotonic) == oldest-first.
    waiting_to_acquire: BTreeMap<u64, MsgMetadata>,

    /// Optional `specs/18` §2.3 "remaining bytes" gauges, refreshed after every
    /// pool mutation. `None` when the throttler runs without a metrics registry.
    metrics: Option<RemainingGauges>,
}

impl Inner {
    /// Pushes the current pool balances to the §2.3 remaining-bytes gauges (if
    /// a metrics handle is attached). Pool balances are ≤ the configured pool
    /// sizes (tens of MiB), well within `i64`.
    fn publish_remaining(&self) {
        if let Some(g) = &self.metrics {
            g.at_large
                .set(i64::try_from(self.remaining_at_large_bytes).unwrap_or(i64::MAX));
            g.validator
                .set(i64::try_from(self.remaining_vdr_bytes).unwrap_or(i64::MAX));
        }
    }
}

#[derive(Debug)]
struct MsgMetadata {
    /// Bytes still needed before this acquire can return.
    bytes_needed: u64,
    /// Total bytes this acquire is reserving.
    msg_size: u64,
    /// The node that issued this acquire.
    node_id: NodeId,
    /// Fired (by `release`) when `bytes_needed` reaches zero.
    wake: oneshot::Sender<()>,
}

impl InboundMsgByteThrottler {
    /// Creates a throttler with the given pool sizes.
    ///
    /// Defaults from `utils/constants/networking.go`:
    /// `VdrAllocSize = 32 MiB`, `AtLargeAllocSize = 6 MiB`,
    /// `NodeMaxAtLargeBytes = 2 MiB` (== `DefaultMaxMessageSize`).
    #[must_use]
    pub fn new(
        vdr_alloc_size: u64,
        at_large_alloc_size: u64,
        node_max_at_large_bytes: u64,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                remaining_vdr_bytes: vdr_alloc_size,
                remaining_at_large_bytes: at_large_alloc_size,
                node_max_at_large_bytes,
                node_to_vdr_bytes_used: BTreeMap::new(),
                node_to_at_large_bytes_used: BTreeMap::new(),
                next_msg_id: 0,
                node_to_waiting_msg_id: BTreeMap::new(),
                waiting_to_acquire: BTreeMap::new(),
                metrics: None,
            })),
        }
    }

    /// Attaches the `specs/18` §2.3 inbound-byte "remaining" gauges
    /// (`byte_throttler_inbound_remaining_{at_large,validator}_bytes`). After
    /// this call every pool mutation refreshes the gauges, and the current
    /// balances are published immediately so a fresh scrape sees the full pool
    /// sizes. No-op semantics are preserved when never called (the gauges stay
    /// at `0`, their registered default).
    pub fn set_metrics(&self, metrics: &crate::metrics::Metrics) {
        let mut inner = self.inner.lock();
        inner.metrics = Some(RemainingGauges {
            at_large: metrics
                .byte_throttler_inbound_remaining_at_large_bytes
                .clone(),
            validator: metrics
                .byte_throttler_inbound_remaining_validator_bytes
                .clone(),
        });
        inner.publish_remaining();
    }

    /// Acquires `size` bytes for `node`, blocking until they are available.
    /// Equivalent to [`Self::acquire_with_vdr_alloc`] with a zero validator
    /// allowance — i.e. the node draws purely from the at-large pool. This is
    /// the spec-shaped entry point (`specs/05` §5).
    ///
    /// Returns `Some(permit)` once enough bytes are reserved (the permit
    /// releases them on `Drop`), or `None` if `cancel` fires while blocked / if
    /// `node` already has an outstanding acquire.
    pub async fn acquire(
        &self,
        size: u64,
        node: NodeId,
        cancel: &CancellationToken,
    ) -> Option<ReleasePermit> {
        self.acquire_with_vdr_alloc(size, node, 0, cancel).await
    }

    /// Acquires `size` bytes for `node`, blocking until they are available.
    ///
    /// `vdr_alloc` is this node's validator-pool allowance (see the module
    /// simplification note); pass `0` for non-validators. Bytes are charged to
    /// the at-large pool first (capped per node by `node_max_at_large_bytes`),
    /// then to the validator pool.
    ///
    /// Returns `Some(permit)` once enough bytes are reserved; the permit
    /// releases them on `Drop`. Returns `None` if `cancel` fires while blocked,
    /// or if `node` already has an outstanding (blocked) acquire (the
    /// single-outstanding-acquire invariant). On cancel, any bytes already
    /// reserved for the partial acquire are released back to the pools.
    pub async fn acquire_with_vdr_alloc(
        &self,
        size: u64,
        node: NodeId,
        vdr_alloc: u64,
        cancel: &CancellationToken,
    ) -> Option<ReleasePermit> {
        let rx = {
            let mut inner = self.inner.lock();

            // Single-outstanding-acquire invariant: reject a concurrent acquire
            // from a node that is already blocked.
            if inner.node_to_waiting_msg_id.contains_key(&node) {
                return None;
            }

            let mut bytes_needed = size;

            // Stage 1: at-large pool, capped per node.
            let at_large_used = bytes_needed
                .min(
                    inner
                        .node_max_at_large_bytes
                        .saturating_sub(inner.at_large_used(&node)),
                )
                .min(inner.remaining_at_large_bytes);
            if at_large_used > 0 {
                inner.remaining_at_large_bytes =
                    inner.remaining_at_large_bytes.saturating_sub(at_large_used);
                bytes_needed = bytes_needed.saturating_sub(at_large_used);
                let entry = inner.node_to_at_large_bytes_used.entry(node).or_insert(0);
                *entry = entry.saturating_add(at_large_used);
            }

            if bytes_needed == 0 {
                inner.publish_remaining();
                return Some(self.permit(size, node));
            }

            // Stage 2: validator pool, bounded by this node's allowance.
            let vdr_already = inner.vdr_used(&node);
            let vdr_allowed = vdr_alloc.saturating_sub(vdr_already);
            let vdr_used = inner.remaining_vdr_bytes.min(bytes_needed).min(vdr_allowed);
            if vdr_used > 0 {
                let entry = inner.node_to_vdr_bytes_used.entry(node).or_insert(0);
                *entry = entry.saturating_add(vdr_used);
                inner.remaining_vdr_bytes = inner.remaining_vdr_bytes.saturating_sub(vdr_used);
                bytes_needed = bytes_needed.saturating_sub(vdr_used);
            }

            inner.publish_remaining();

            if bytes_needed == 0 {
                return Some(self.permit(size, node));
            }

            // Stage 3: block. Register a waiter keyed by a fresh msg id.
            let (tx, rx) = oneshot::channel();
            inner.next_msg_id = inner.next_msg_id.saturating_add(1);
            let msg_id = inner.next_msg_id;
            inner.waiting_to_acquire.insert(
                msg_id,
                MsgMetadata {
                    bytes_needed,
                    msg_size: size,
                    node_id: node,
                    wake: tx,
                },
            );
            inner.node_to_waiting_msg_id.insert(node, msg_id);
            (msg_id, rx)
        };
        let (msg_id, rx) = rx;

        tokio::select! {
            res = rx => {
                match res {
                    // Woken by `release` with enough bytes: we hold `size`.
                    Ok(()) => Some(self.permit(size, node)),
                    // Sender dropped without firing: should not happen, but
                    // treat as cancellation (release whatever was reserved).
                    Err(_) => {
                        self.abandon(msg_id, node, size);
                        None
                    }
                }
            }
            () = cancel.cancelled() => {
                self.abandon(msg_id, node, size);
                None
            }
        }
    }

    /// Builds a permit for a fully-satisfied acquire of `size` bytes by `node`.
    fn permit(&self, size: u64, node: NodeId) -> ReleasePermit {
        ReleasePermit {
            inner: Arc::clone(&self.inner),
            node,
            msg_size: size,
            released: false,
        }
    }

    /// Cancels a blocked acquire, returning any reserved bytes. `size` is the
    /// full requested size, used when the waiter was already satisfied by a
    /// concurrent `release` (cancel-after-wake race) and thus holds all `size`
    /// bytes.
    fn abandon(&self, msg_id: u64, node: NodeId, size: u64) {
        let mut inner = self.inner.lock();
        if let Some(meta) = inner.waiting_to_acquire.remove(&msg_id) {
            // Still queued: only remove the node->msg mapping if it still
            // points at us, then release the partially-reserved bytes.
            if inner.node_to_waiting_msg_id.get(&node) == Some(&msg_id) {
                inner.node_to_waiting_msg_id.remove(&node);
            }
            let reserved = meta.msg_size.saturating_sub(meta.bytes_needed);
            inner.release_bytes(node, reserved);
        } else {
            // Race: `release` satisfied and removed our waiter (firing `wake`)
            // before we took the cancel branch, so all `size` bytes are
            // reserved to us. Release them in full to avoid a leak.
            inner.release_bytes(node, size);
        }
    }
}

impl Inner {
    fn at_large_used(&self, node: &NodeId) -> u64 {
        self.node_to_at_large_bytes_used
            .get(node)
            .copied()
            .unwrap_or(0)
    }

    fn vdr_used(&self, node: &NodeId) -> u64 {
        self.node_to_vdr_bytes_used.get(node).copied().unwrap_or(0)
    }

    /// Releases `reserved` of the `msg_size` bytes held by `node`, handing them
    /// back to the pools and waking waiters oldest-first. Port of Go `release`.
    fn release_bytes(&mut self, node: NodeId, reserved: u64) {
        if reserved == 0 {
            return;
        }

        // Split the released bytes between the validator and at-large pools the
        // same way Go does: validator bytes come back first, capped by what the
        // node currently has charged to its validator allocation.
        let vdr_used = self.vdr_used(&node);
        let vdr_to_return = reserved.min(vdr_used);
        let mut at_large_to_return = reserved.saturating_sub(vdr_to_return);

        // --- At-large hand-back ---
        if at_large_to_return > 0 {
            self.remaining_at_large_bytes = self
                .remaining_at_large_bytes
                .saturating_add(at_large_to_return);
            let entry = self.node_to_at_large_bytes_used.entry(node).or_insert(0);
            *entry = entry.saturating_sub(at_large_to_return);
            if *entry == 0 {
                self.node_to_at_large_bytes_used.remove(&node);
            }

            // Give freed at-large bytes to waiting messages, oldest-first.
            let mut satisfied: Vec<u64> = Vec::new();
            // Collect ids in ascending (insertion) order; mutate via lookups.
            let waiting_ids: Vec<u64> = self.waiting_to_acquire.keys().copied().collect();
            for id in waiting_ids {
                if self.remaining_at_large_bytes == 0 {
                    break;
                }
                let (wnode, needed) = match self.waiting_to_acquire.get(&id) {
                    Some(m) => (m.node_id, m.bytes_needed),
                    None => continue,
                };
                let give = needed
                    .min(
                        self.node_max_at_large_bytes
                            .saturating_sub(self.at_large_used(&wnode)),
                    )
                    .min(self.remaining_at_large_bytes);
                if give > 0 {
                    let entry = self.node_to_at_large_bytes_used.entry(wnode).or_insert(0);
                    *entry = entry.saturating_add(give);
                    self.remaining_at_large_bytes =
                        self.remaining_at_large_bytes.saturating_sub(give);
                    at_large_to_return = at_large_to_return.saturating_sub(give);
                    if let Some(m) = self.waiting_to_acquire.get_mut(&id) {
                        m.bytes_needed = m.bytes_needed.saturating_sub(give);
                        if m.bytes_needed == 0 {
                            satisfied.push(id);
                        }
                    }
                }
            }
            for id in satisfied {
                if let Some(meta) = self.waiting_to_acquire.remove(&id) {
                    self.node_to_waiting_msg_id.remove(&meta.node_id);
                    // Ignore send error: the acquirer may have cancelled.
                    let _ = meta.wake.send(());
                }
            }
        }

        // --- Validator hand-back ---
        let mut vdr_to_return = vdr_to_return;
        // First, try to satisfy this node's own waiting message, if any.
        if vdr_to_return > 0 {
            let own_msg_id = self.node_to_waiting_msg_id.get(&node).copied();
            if let Some(msg_id) = own_msg_id {
                let satisfied = if let Some(meta) = self.waiting_to_acquire.get_mut(&msg_id) {
                    let give = meta.bytes_needed.min(vdr_to_return);
                    meta.bytes_needed = meta.bytes_needed.saturating_sub(give);
                    vdr_to_return = vdr_to_return.saturating_sub(give);
                    meta.bytes_needed == 0
                } else {
                    false
                };
                if satisfied && let Some(done) = self.waiting_to_acquire.remove(&msg_id) {
                    self.node_to_waiting_msg_id.remove(&node);
                    let _ = done.wake.send(());
                }
            }
        }
        // Any remainder goes back to the validator pool.
        if vdr_to_return > 0 {
            let entry = self.node_to_vdr_bytes_used.entry(node).or_insert(0);
            *entry = entry.saturating_sub(vdr_to_return);
            if *entry == 0 {
                self.node_to_vdr_bytes_used.remove(&node);
            }
            self.remaining_vdr_bytes = self.remaining_vdr_bytes.saturating_add(vdr_to_return);
        }

        // metrics: pool balances changed — refresh the §2.3 remaining gauges.
        self.publish_remaining();
    }
}

/// RAII permit returned by [`InboundMsgByteThrottler::acquire`]. Releases the
/// reserved bytes back to the pools on `Drop`, waking the next waiter.
#[derive(Debug)]
pub struct ReleasePermit {
    inner: Arc<Mutex<Inner>>,
    node: NodeId,
    msg_size: u64,
    released: bool,
}

impl Drop for ReleasePermit {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        // A live permit always holds the full `msg_size`.
        let mut inner = self.inner.lock();
        inner.release_bytes(self.node, self.msg_size);
    }
}
