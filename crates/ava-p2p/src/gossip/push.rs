// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PushGossiper` — periodic new-item + regossip queues (Go
//! `network/p2p/gossip/gossip.go` `PushGossiper[T]`).
//!
//! ## Simplifications recorded here (task-6 brief Step 1)
//!
//! - **Push targeting** (pre-authorized — see `gossip/mod.rs`'s
//!   [`super::GossipParams`] doc): Go's `gossip()` samples validators by
//!   stake in addition to a flat connected-node count
//!   (`p.validators.Top(ctx, gossipParams.StakePercentage)`,
//!   `gossip.go:547-557`). This port expresses targeting purely via
//!   [`ava_vm::app_sender::SendConfig`] (`push_cfg`/`regossip_cfg`'s
//!   `validators` counts); the production `AppSender` resolves the actual
//!   node sampling.
//! - **Per-item regossip throttling folded into one cycle-level gate.** Go
//!   tracks a `lastGossiped` timestamp *per gossipable* and skips
//!   re-sending an item until `now - lastGossiped >= maxRegossipFrequency`
//!   (`gossip.go:427-430,506-512`) — every call to `Gossip()` (driven every
//!   `push_period`) touches the regossip queue, but each item individually
//!   self-throttles. This port instead gates the *whole* regossip drain
//!   behind a single [`PushState::last_regossip`] instant on the
//!   `PushGossiper`: [`PushGossiper::gossip_cycle`] only drains
//!   `to_regossip` once `regossip_period` has elapsed since the previous
//!   drain, then drains the entire queue (up to `target_message_size`) in
//!   one shot. No per-item metrics/bookkeeping surface exists in this port
//!   to make the finer-grained version pay for itself.
//! - **Marshal failure drops the item, not the whole cycle.** Go's
//!   `gossip()` returns the marshal error immediately, aborting the rest of
//!   that drain for the cycle (`gossip.go:514-519`). This port instead just
//!   removes the offending item from tracking and continues draining the
//!   rest of the queue — one bad item shouldn't stall everything behind it.
//! - **Discarded-cache "pretend recently gossiped" behavior is ported**,
//!   just backed by a small bounded FIFO ([`DiscardedCache`]) instead of
//!   Go's generic `lru.Cache` (same eviction policy — oldest-inserted evicted
//!   first once full — simpler implementation): an id dropped because the
//!   set no longer has it (`gossip.go:499-503`) is recorded there, and a
//!   later [`PushGossiper::add`] for that same id skips straight to
//!   `to_regossip` (Go `gossip.go:586-591`) rather than `to_gossip`.
//! - No metrics (Prometheus `gossip.Metrics`, `gossip.go:105-178`) — this
//!   port has no metrics registry wired up yet.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::Mutex;
use prost::Message;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_vm::app_sender::SendConfig;

use crate::client::Client;
use crate::error::Result;
use crate::gossip::{GossipParams, Gossipable, Marshaller, Set};
use crate::pb::sdk;

/// A small bounded FIFO of recently discarded gossip ids (see the module
/// doc's "discarded-cache" note). Oldest-inserted is evicted first once at
/// capacity, matching Go's `lru.Cache` eviction policy for this use (the
/// cache is only ever probed for membership, never re-ordered on hit, so a
/// plain FIFO is observably equivalent for this port's purposes).
struct DiscardedCache {
    order: VecDeque<Id>,
    members: HashSet<Id>,
    capacity: usize,
}

impl DiscardedCache {
    fn new(capacity: usize) -> Self {
        Self {
            order: VecDeque::new(),
            members: HashSet::new(),
            // A zero-capacity cache would make `insert` a no-op loop guard
            // moot and is not a meaningful configuration; floor at 1.
            capacity: capacity.max(1),
        }
    }

    fn contains(&self, id: &Id) -> bool {
        self.members.contains(id)
    }

    fn insert(&mut self, id: Id) {
        if self.members.contains(&id) {
            return;
        }
        if self.order.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.members.remove(&oldest);
        }
        self.order.push_back(id);
        self.members.insert(id);
    }
}

/// Mutable gossip/regossip queues (Go `PushGossiper`'s `tracking`/
/// `toGossip`/`toRegossip`/`discarded` fields, `gossip.go:387-393`).
struct PushState<T> {
    /// Ids currently enqueued in either queue, so a repeated [`PushGossiper::add`]
    /// for an already-tracked id is a no-op (Go `gossip.go:578-581`).
    tracked: HashSet<Id>,
    to_gossip: VecDeque<T>,
    to_regossip: VecDeque<T>,
    discarded: DiscardedCache,
    /// When the regossip queue was last drained. Seeded to "now" at
    /// construction ([`PushGossiper::new`]) rather than `None`/"always due":
    /// an item newly moved onto `to_regossip` by a push drain must not be
    /// resent within the very same cycle (Go's per-item `lastGossiped`
    /// throttle, `gossip.go:506-512`, guards exactly this — freshly-gossiped
    /// items fail the `maxLastGossipTimeToRegossip.Before(lastGossiped)`
    /// check and get skipped). This port's cycle-level gate reproduces that
    /// guard for the always-relevant "just pushed it" case by simply not
    /// being due yet immediately after construction.
    last_regossip: tokio::time::Instant,
}

/// Broadcasts gossipables to peers on a push-then-periodic-regossip cadence
/// (Go `network/p2p/gossip/gossip.go` `PushGossiper[T]`).
pub struct PushGossiper<T, M, S> {
    marshaller: Arc<M>,
    set: Arc<S>,
    client: Client,
    params: GossipParams,
    state: Mutex<PushState<T>>,
}

impl<T, M, S> PushGossiper<T, M, S>
where
    T: Gossipable,
    M: Marshaller<T>,
    S: Set<T>,
{
    /// Constructs a `PushGossiper` (Go `NewPushGossiper`, `gossip.go:320-363`
    /// — minus the `BranchingFactor::Verify()`/negative-size validation,
    /// which has no analog since `GossipParams`'s fields are plain
    /// `usize`/`Duration`, not user-supplied signed values).
    #[must_use]
    pub fn new(marshaller: M, set: Arc<S>, client: Client, params: GossipParams) -> Self {
        let discarded = DiscardedCache::new(params.discarded_cache_size);
        Self {
            marshaller: Arc::new(marshaller),
            set,
            client,
            params,
            state: Mutex::new(PushState {
                tracked: HashSet::new(),
                to_gossip: VecDeque::new(),
                to_regossip: VecDeque::new(),
                discarded,
                last_regossip: tokio::time::Instant::now(),
            }),
        }
    }

    /// Enqueues `t` to be pushed on the next cycle, unless it is already
    /// tracked (Go `PushGossiper.Add`, `gossip.go:562-596`, minus the
    /// metrics-only `addedTimeSum` bookkeeping). An id recently discarded
    /// (the set no longer had it at drain time) is pretended to have just
    /// been gossiped and goes straight to the regossip queue.
    pub fn add(&self, t: T) {
        let id = t.gossip_id();
        let mut state = self.state.lock();
        if !state.tracked.insert(id) {
            return;
        }
        if state.discarded.contains(&id) {
            state.to_regossip.push_back(t);
        } else {
            state.to_gossip.push_back(t);
        }
    }

    /// Drains up to `target_message_size` bytes' worth of items from
    /// `to_regossip` (if `from_regossip`) or `to_gossip` (otherwise),
    /// marshaling each and re-queuing it onto `to_regossip` (Go's `gossip()`,
    /// `gossip.go:475-560`, sans metrics/validator-stake sampling). Ids the
    /// set no longer has are dropped and recorded as discarded rather than
    /// marshaled.
    fn drain_queue(&self, from_regossip: bool) -> Vec<Vec<u8>> {
        let mut state = self.state.lock();
        let mut batch = Vec::new();
        let mut sent_bytes = 0usize;
        while sent_bytes < self.params.target_message_size {
            let item = if from_regossip {
                state.to_regossip.pop_front()
            } else {
                state.to_gossip.pop_front()
            };
            let Some(item) = item else {
                break;
            };

            let id = item.gossip_id();
            if !self.set.has(&id) {
                state.tracked.remove(&id);
                state.discarded.insert(id);
                continue;
            }

            match self.marshaller.marshal(&item) {
                Ok(bytes) => {
                    sent_bytes = sent_bytes.saturating_add(bytes.len());
                    batch.push(bytes);
                    state.to_regossip.push_back(item);
                }
                Err(_) => {
                    // Marshal failed: drop the item rather than aborting the
                    // whole drain — see the module doc's simplification note.
                    state.tracked.remove(&id);
                }
            }
        }
        batch
    }

    /// Encodes `batch` as a `PushGossip` and sends it via `cfg` (Go
    /// `MarshalAppGossip` + `Client.AppGossip`, `gossip.go:533,550-559`).
    async fn send(
        &self,
        token: &CancellationToken,
        batch: Vec<Vec<u8>>,
        cfg: SendConfig,
    ) -> Result<()> {
        let msg = sdk::PushGossip {
            gossip: batch.into_iter().map(Bytes::from).collect(),
        };
        self.client
            .app_gossip(token, cfg, msg.encode_to_vec())
            .await
    }

    /// Runs one gossip cycle (Go `PushGossiper.Gossip`, `gossip.go:433-473`):
    /// drains `to_gossip` into a `PushGossip` batch sent via
    /// `params.push_cfg`, moving surviving items to `to_regossip`; then, if
    /// `regossip_period` has elapsed since the last regossip drain, does the
    /// same for `to_regossip` via `params.regossip_cfg`.
    pub async fn gossip_cycle(&self, token: &CancellationToken) -> Result<()> {
        let push_batch = self.drain_queue(false);
        if !push_batch.is_empty() {
            self.send(token, push_batch, self.params.push_cfg.clone())
                .await?;
        }

        let now = tokio::time::Instant::now();
        let due = {
            let state = self.state.lock();
            now.saturating_duration_since(state.last_regossip) >= self.params.regossip_period
        };
        if due {
            self.state.lock().last_regossip = now;
            let regossip_batch = self.drain_queue(true);
            if !regossip_batch.is_empty() {
                self.send(token, regossip_batch, self.params.regossip_cfg.clone())
                    .await?;
            }
        }

        Ok(())
    }
}
