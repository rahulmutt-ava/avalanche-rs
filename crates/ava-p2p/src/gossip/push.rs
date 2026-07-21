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
//! - **Per-item regossip throttling — ported faithfully (review fix-up).**
//!   Go's `Gossip()` attempts the regossip drain on **every** call
//!   (`gossip.go:433-473`, unconditionally calling `p.gossip(...,
//!   p.toRegossip, p.toRegossip, ...)` each time), throttling **per item**
//!   via a `lastGossiped` timestamp: an item already in `toRegossip` is
//!   skipped — put back at the front, and the whole drain stops right there
//!   — until `now.Sub(lastGossiped) >= maxRegossipFrequency`
//!   (`gossip.go:427-430,506-512`). An earlier version of this port
//!   collapsed that into a single cycle-level gate (one `last_regossip`
//!   instant on the whole `PushGossiper`), which diverges observably once a
//!   regossip backlog exceeds one `target_message_size` batch: the
//!   cycle-level gate would wait a full extra `regossip_period` before
//!   sending the remainder, instead of draining it on the very next cycle
//!   like Go does. This port now stores a per-entry timestamp in
//!   `to_regossip` (`VecDeque<(Instant, T)>`) and reproduces Go's exact
//!   per-item check in [`PushGossiper::drain_regossip`]; the queue stays
//!   ordered oldest-timestamp-first (entries are always re-appended to the
//!   back after being resent), which is what lets "stop at the first
//!   not-yet-due item" be correct — see `drain_regossip`'s doc. With
//!   per-item timestamps the old cycle-level gate's first-cycle
//!   double-send hazard can't occur either: a just-pushed item's timestamp
//!   is fresh, so it fails its own due check on the very next drain
//!   attempt within the same cycle — no separate seeding trick needed.
//! - **Marshal failure drops the item, not the whole cycle.** Go's
//!   `gossip()` returns the marshal error immediately, aborting the rest of
//!   that drain for the cycle (`gossip.go:514-519`). This port instead just
//!   removes the offending item from tracking and continues draining the
//!   rest of the queue — one bad item shouldn't stall everything behind it.
//! - **Discarded-cache promote-on-hit — corrected (review fix-up).** Go's
//!   `discarded` field is a real `lru.Cache` (`cache/lru/cache.go`), and its
//!   `Get` **does** promote the hit to most-recently-used
//!   (`cache/lru/cache.go:52-59`), including the `p.discarded.Get(gossipID)`
//!   probe in `PushGossiper.Add` (`gossip.go:588`). An earlier version of
//!   this port's [`DiscardedCache`] was a plain FIFO (insertion order never
//!   changed by a lookup) and claimed FIFO-vs-LRU eviction-policy
//!   equivalence — that claim was wrong for any id that gets looked up more
//!   than once before eviction. `DiscardedCache` is now backed by
//!   [`ava_utils::linked::LinkedHashmap`] (insertion-ordered, `put`
//!   promotes an existing key to the back — Go `utils/linked.Hashmap`
//!   parity), and [`DiscardedCache::contains`] itself promotes on a hit, so
//!   it is now a real LRU, matching Go's `Get`-promotes behavior exactly —
//!   the "simpler implementation" is now only "a different generic ordered
//!   map than Go's, not different *observable* eviction behavior."
//! - An id dropped because the set no longer has it (`gossip.go:499-503`) is
//!   recorded in the discarded cache, and a later [`PushGossiper::add`] for
//!   that same id skips straight to `to_regossip` (Go `gossip.go:586-591`)
//!   rather than `to_gossip`.
//! - No metrics (Prometheus `gossip.Metrics`, `gossip.go:105-178`) — this
//!   port has no metrics registry wired up yet.
//!
//! ## Reentrancy hazard for `Set` implementors (read this before Task 11)
//!
//! [`PushGossiper::drain_push`]/[`PushGossiper::drain_regossip`] call
//! `self.set.has(&id)` (and [`PushGossiper::add`] calls into
//! `self.state`'s own lock) while holding `state: Mutex<PushState<T>>`
//! locked. `parking_lot::Mutex` is **not reentrant**: if a future concrete
//! `Set` implementation (e.g. Task 11's C-Chain tx pool) synchronously calls
//! back into `PushGossiper::add`/`gossip_cycle` from inside its own
//! `has`/`iterate`/`add` — for instance, a tx-pool eviction callback that
//! tries to re-push a replacement tx — that reentrant call will try to lock
//! this same `PushGossiper`'s `state` mutex again on the same thread and
//! **deadlock**. Go has the identical hazard (`p.lock sync.Mutex` is locked
//! across `p.set.Has(...)`/`p.gossip(...)` too, `gossip.go:439,475-560`, and
//! `sync.Mutex` is likewise non-reentrant) — this isn't a regression
//! introduced by the port, but it is real and worth flagging explicitly: a
//! `Set` impl must only talk back to its owning `PushGossiper` from a
//! separate task/thread (e.g. via a channel or by scheduling the `add` call
//! after returning), never synchronously from within `has`/`iterate`/`add`
//! itself.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::Mutex;
use prost::Message;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_utils::linked::LinkedHashmap;
use ava_vm::app_sender::SendConfig;

use crate::client::Client;
use crate::error::Result;
use crate::gossip::{GossipParams, Gossipable, Marshaller, Set};
use crate::pb::sdk;

/// A small bounded LRU of recently discarded gossip ids (see the module
/// doc's "discarded-cache" note). Backed by
/// [`ava_utils::linked::LinkedHashmap`] (insertion-ordered; `put` moves an
/// existing key to the back), which gives true promote-on-hit LRU semantics
/// — [`DiscardedCache::contains`] itself promotes — matching Go's
/// `lru.Cache.Get` (`cache/lru/cache.go:52-59`).
struct DiscardedCache {
    entries: LinkedHashmap<Id, ()>,
    capacity: usize,
}

impl DiscardedCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: LinkedHashmap::new(),
            // A zero-capacity cache would make the eviction loop below spin
            // forever trying to shrink to 0; floor at 1.
            capacity: capacity.max(1),
        }
    }

    /// Returns whether `id` is present, promoting it to the
    /// most-recently-used (back) position on a hit — Go's `lru.Cache.Get`
    /// promotes on every lookup, hit included, and `PushGossiper.Add`'s
    /// `p.discarded.Get(gossipID)` probe (`gossip.go:588`) is exactly such a
    /// lookup.
    fn contains(&mut self, id: &Id) -> bool {
        if self.entries.contains(id) {
            // `put` with the same value moves the existing key to the back
            // without otherwise changing anything (Go `Hashmap.Put` parity).
            self.entries.put(*id, ());
            true
        } else {
            false
        }
    }

    /// Records `id` as discarded, evicting the oldest entry/entries if this
    /// insert pushes the cache over `capacity` (Go `lru.Cache.Put`'s
    /// size-bound eviction).
    fn insert(&mut self, id: Id) {
        self.entries.put(id, ());
        while self.entries.len() > self.capacity {
            let oldest = self.entries.oldest().map(|(k, _)| *k);
            match oldest {
                Some(k) => {
                    self.entries.delete(&k);
                }
                None => break,
            }
        }
    }
}

/// One regossip-queue entry: the item plus the instant it was last (re)sent,
/// i.e. Go's per-gossipable `tracking.lastGossiped` (`gossip.go:428-430`).
type RegossipEntry<T> = (tokio::time::Instant, T);

/// Mutable gossip/regossip queues (Go `PushGossiper`'s `tracking`/
/// `toGossip`/`toRegossip`/`discarded` fields, `gossip.go:387-393`).
struct PushState<T> {
    /// Ids currently enqueued in either queue, so a repeated [`PushGossiper::add`]
    /// for an already-tracked id is a no-op (Go `gossip.go:578-581`).
    tracked: HashSet<Id>,
    to_gossip: VecDeque<T>,
    /// Ordered oldest-`lastGossiped`-first: every successful (re)send
    /// re-appends its entry to the back with a fresh timestamp, so the
    /// front is always the next-most-overdue entry — see
    /// [`PushGossiper::drain_regossip`].
    to_regossip: VecDeque<RegossipEntry<T>>,
    discarded: DiscardedCache,
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
            }),
        }
    }

    /// Enqueues `t` to be pushed on the next cycle, unless it is already
    /// tracked (Go `PushGossiper.Add`, `gossip.go:562-596`, minus the
    /// metrics-only `addedTimeSum` bookkeeping). An id recently discarded
    /// (the set no longer had it at drain time) is pretended to have just
    /// been gossiped — pushed straight onto the regossip queue with a
    /// fresh timestamp, so it isn't immediately re-sent again.
    pub fn add(&self, t: T) {
        let id = t.gossip_id();
        let mut state = self.state.lock();
        if !state.tracked.insert(id) {
            return;
        }
        if state.discarded.contains(&id) {
            state
                .to_regossip
                .push_back((tokio::time::Instant::now(), t));
        } else {
            state.to_gossip.push_back(t);
        }
    }

    /// Drains up to `target_message_size` bytes' worth of items from
    /// `to_gossip`, marshaling each and moving it onto `to_regossip` with a
    /// fresh timestamp (Go's `gossip(ctx, now, p.gossipParams, p.toGossip,
    /// p.toRegossip, ...)` call in `Gossip()`, `gossip.go:449-459`). Ids the
    /// set no longer has are dropped and recorded as discarded rather than
    /// marshaled.
    fn drain_push(&self) -> Vec<Vec<u8>> {
        let mut state = self.state.lock();
        let mut batch = Vec::new();
        let mut sent_bytes = 0usize;
        while sent_bytes < self.params.target_message_size {
            let Some(item) = state.to_gossip.pop_front() else {
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
                    state
                        .to_regossip
                        .push_back((tokio::time::Instant::now(), item));
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

    /// Drains up to `target_message_size` bytes' worth of **due** items from
    /// `to_regossip` (Go's second `gossip(ctx, now, p.regossipParams,
    /// p.toRegossip, p.toRegossip, ...)` call, `gossip.go:461-471`, and the
    /// per-item throttle inside `gossip()` itself, `gossip.go:506-512`).
    ///
    /// Called on **every** [`PushGossiper::gossip_cycle`] invocation, not
    /// gated by any cycle-level timer — matching Go exactly. `to_regossip`
    /// is ordered oldest-timestamp-first (see [`PushState::to_regossip`]),
    /// so this pops from the front and, the moment an item isn't yet due
    /// (`now - last_gossiped < regossip_period`), pushes it back and stops:
    /// every entry behind it is even more recently (re)gossiped, so none of
    /// them can be due either (Go: `toGossip.PushLeft(gossipable); break`,
    /// `gossip.go:507-512` — Go's variable naming there is `toGossip`
    /// because `gossip()` is generic over which queue is "the one being
    /// drained", but for this call site that parameter is bound to
    /// `p.toRegossip`). A due item that's successfully resent is
    /// re-appended to the back with a fresh timestamp, preserving the
    /// oldest-first invariant for the next call. Ids the set no longer has
    /// are dropped and recorded as discarded, same as [`Self::drain_push`].
    fn drain_regossip(&self, now: tokio::time::Instant) -> Vec<Vec<u8>> {
        let mut state = self.state.lock();
        let mut batch = Vec::new();
        let mut sent_bytes = 0usize;
        while sent_bytes < self.params.target_message_size {
            let Some((last_gossiped, item)) = state.to_regossip.pop_front() else {
                break;
            };

            if now.saturating_duration_since(last_gossiped) < self.params.regossip_period {
                state.to_regossip.push_front((last_gossiped, item));
                break;
            }

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
                    state.to_regossip.push_back((now, item));
                }
                Err(_) => {
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
    /// `params.push_cfg`, moving surviving items onto `to_regossip`; then
    /// unconditionally attempts a `to_regossip` drain (per-item throttled,
    /// see [`Self::drain_regossip`]) sent via `params.regossip_cfg`.
    pub async fn gossip_cycle(&self, token: &CancellationToken) -> Result<()> {
        let push_batch = self.drain_push();
        if !push_batch.is_empty() {
            self.send(token, push_batch, self.params.push_cfg.clone())
                .await?;
        }

        let now = tokio::time::Instant::now();
        let regossip_batch = self.drain_regossip(now);
        if !regossip_batch.is_empty() {
            self.send(token, regossip_batch, self.params.regossip_cfg.clone())
                .await?;
        }

        Ok(())
    }
}
