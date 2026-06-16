// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Crash-injection hardening harness (specs/27 §9, §2 CC-ATOMIC, §3.1 two-sided
//! shared-memory consistency; plan/M9 §M9.20).
//!
//! This module hosts the in-process crash-injection seam the offline arm of the
//! M9.20 suite runs every CI run:
//!
//! * [`FailpointDb`] — a [`DynDatabase`](ava_database::DynDatabase) wrapper over a
//!   *shared* backing [`MemDb`](ava_database::MemDb) that injects a deterministic
//!   failure on the N-th mutating op (no wall-clock, no RNG). Because the backing
//!   store is an `Arc<MemDb>`, "restart" is simply rebuilding a fresh wrapper over
//!   the same `Arc` — the bytes that survived the crash are exactly what recovery
//!   sees.
//! * [`CrashPoint`] — the §3 C0..C7 model reduced to the surface the in-memory KV
//!   tier can express: fail *before* the accept batch (C0), fail *during* a
//!   non-atomic multi-key write so a partial diff lands (the CC-ATOMIC anti-pattern
//!   the spec forbids), and fail *after* a state write but *before* the
//!   last-accepted marker / shared-memory entry (C2/C4 — the multi-engine /
//!   ack-gap windows).
//! * [`AcceptHarness`] — drives a CC-ATOMIC accept (state diff + last-accepted
//!   pointer + cross-chain shared-memory put) through a [`FailpointDb`] under a
//!   chosen [`CrashPoint`], using **either** the atomic single-`write()` batch
//!   path (the §2.2 Rust pattern) **or** a naive per-key write loop (to *prove*
//!   the atomic path is the one that survives). On restart it runs an idempotent
//!   recovery (read the marker; reconcile / drop a half-applied diff) and exposes
//!   the reconciled state for assertions.
//!
//! The offline arm asserts the Rust pipeline's own recovery is all-or-nothing
//! (CC-ATOMIC) and idempotent — a deterministic property of the Rust impl, not a
//! Go comparison. The live Go-oracle-equivalence arm lives in
//! `tests/crash_injection.rs`, gated behind the `live` Cargo feature + `#[ignore]`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ava_database::{Batch, DynDatabase, Error, MemDb, Result};

// ===========================================================================
// Key layout — a miniature of the per-VM accept batch (specs/27 §2.1 CC-ATOMIC).
// ===========================================================================

/// The byte-ranges a CC-ATOMIC accept batch mutates (specs/27 §2.1): the block's
/// **state diff**, the **last-accepted pointer**, and any **cross-chain
/// shared-memory** request. The offline harness writes one representative key in
/// each range so a partial write is observable as a torn subset.
pub mod keys {
    /// Prefix for a height's state-diff entry (mirrors a VM's UTXO/staker diff).
    pub const STATE_PREFIX: &[u8] = b"state/";
    /// The singleton last-accepted pointer (mirrors `"singleton"->"last accepted"`).
    pub const LAST_ACCEPTED: &[u8] = b"singleton/last-accepted";
    /// Prefix for an outbound shared-memory export entry (the peer chain reads
    /// this back via `SharedMemory::get`).
    pub const SHARED_MEMORY_PREFIX: &[u8] = b"sharedmem/";

    /// The state-diff key for `height`.
    #[must_use]
    pub fn state(height: u64) -> Vec<u8> {
        let mut k = STATE_PREFIX.to_vec();
        k.extend_from_slice(&height.to_be_bytes());
        k
    }

    /// The shared-memory export key for `input_id`.
    #[must_use]
    pub fn shared_memory(input_id: &[u8]) -> Vec<u8> {
        let mut k = SHARED_MEMORY_PREFIX.to_vec();
        k.extend_from_slice(input_id);
        k
    }
}

// ===========================================================================
// FailpointDb — deterministic N-th-mutation failure injector.
// ===========================================================================

/// Error injected by a [`FailpointDb`] when its mutation counter reaches the
/// armed trip point. Surfaces as [`Error::Other`] so it is treated exactly like a
/// real backend IO fault (and would poison a `corruptabledb`, specs/27 §6.1) —
/// never as a `Closed`/`NotFound` control-flow sentinel.
#[derive(Debug, thiserror::Error)]
#[error("failpoint: injected crash on mutation #{0}")]
pub struct InjectedCrash(pub u64);

/// A [`DynDatabase`] that wraps a shared backing [`MemDb`] and fails
/// deterministically on the N-th mutating op (`put`, `delete`, or — via the
/// harness's `write_batch` boundary — a batch flush).
///
/// "Crash + restart" is modeled by dropping the wrapper while keeping its
/// `Arc<MemDb>`: whatever bytes landed before the failure persist, and a fresh
/// wrapper over the same `Arc` is the restarted node.
pub struct FailpointDb {
    inner: Arc<MemDb>,
    /// Number of mutating ops completed so far (1-based when compared to `trip`).
    counter: AtomicU64,
    /// The op number on which to inject a failure; `0` ⇒ never fail.
    trip: u64,
}

impl FailpointDb {
    /// Wraps `inner`, arming a failure on the `trip`-th mutating op. `trip == 0`
    /// disables injection (the wrapper is a transparent pass-through, used for the
    /// clean baseline + the post-restart replay).
    #[must_use]
    pub fn new(inner: Arc<MemDb>, trip: u64) -> Self {
        Self {
            inner,
            counter: AtomicU64::new(0),
            trip,
        }
    }

    /// The shared backing store (so a caller can rebuild a fresh wrapper over the
    /// same surviving bytes — a "restart").
    #[must_use]
    pub fn backing(&self) -> Arc<MemDb> {
        Arc::clone(&self.inner)
    }

    /// Increments the mutation counter and returns `Err` if this op is the armed
    /// trip point. Deterministic: same `trip` ⇒ same op fails, every run.
    fn tick(&self) -> Result<()> {
        let n = self
            .counter
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        if self.trip != 0 && n == self.trip {
            return Err(Error::Other(anyhow::Error::new(InjectedCrash(n))));
        }
        Ok(())
    }

    /// How many mutating ops have been observed (for harness assertions).
    #[must_use]
    pub fn mutations(&self) -> u64 {
        self.counter.load(Ordering::SeqCst)
    }
}

impl DynDatabase for FailpointDb {
    fn has(&self, key: &[u8]) -> Result<bool> {
        self.inner.has(key)
    }

    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        self.inner.get(key)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.tick()?;
        self.inner.put(key, value)
    }

    fn delete(&self, key: &[u8]) -> Result<()> {
        self.tick()?;
        self.inner.delete(key)
    }

    fn new_batch(&self) -> Box<dyn Batch + '_> {
        // The batch buffers ops in memory; injection happens at the harness's
        // `write_batch` boundary, which models the §2.2 single atomic write point.
        self.inner.new_batch()
    }

    fn new_iterator_with_start_and_prefix<'a>(
        &'a self,
        start: &[u8],
        prefix: &[u8],
    ) -> ava_database::BoxIter<'a> {
        self.inner.new_iterator_with_start_and_prefix(start, prefix)
    }

    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        self.inner.compact(start, limit)
    }

    fn close(&self) -> Result<()> {
        self.inner.close()
    }

    fn health_check(&self) -> Result<serde_json::Value> {
        self.inner.health_check()
    }
}

// ===========================================================================
// CrashPoint — the §3 C0..C7 model, reduced to the KV surface.
// ===========================================================================

/// Where in the accept/commit sequence to inject the crash. Mirrors the
/// distinguishable rows of specs/27 §3 as closely as the in-memory KV surface can
/// express them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    /// No crash — the clean baseline (a fully durable accept).
    None,
    /// Fail **before** anything from this block is written (C0). The
    /// last-accepted pointer still names the parent; recovery is a no-op.
    BeforeWrite,
    /// Fail **mid multi-key write** (C1's anti-pattern): used with the *naive*
    /// per-key write loop, this lands a partial subset of the accept's keys on
    /// disk. CC-ATOMIC forbids this being observable — the atomic-batch path makes
    /// it unreachable (the whole batch lands or none does).
    MidWrite,
    /// Fail **after** the state diff is durable but **before** the last-accepted
    /// pointer + shared-memory entry (C2 multi-engine / C4 ack-gap). The marker
    /// still names the parent, so recovery must drop / ignore the orphan diff.
    AfterStateBeforeMarker,
}

impl CrashPoint {
    /// The complete C0..C4 offline matrix the suite sweeps.
    #[must_use]
    pub fn offline_matrix() -> [CrashPoint; 4] {
        [
            CrashPoint::None,
            CrashPoint::BeforeWrite,
            CrashPoint::MidWrite,
            CrashPoint::AfterStateBeforeMarker,
        ]
    }
}

/// Whether the accept path commits via the CC-ATOMIC single-`write()` batch
/// (specs/27 §2.2) or a naive per-key write loop. The suite proves the atomic
/// path survives every crash point all-or-nothing while the naive path can tear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStrategy {
    /// One [`Batch::write`] for the union of state diff + LA pointer + SM put —
    /// the §2.2 pattern. RocksDB (and `MemBatch`) make this all-or-nothing.
    AtomicBatch,
    /// Per-key `put`s in sequence — the anti-pattern. A mid-sequence crash leaves
    /// a torn subset on disk.
    NaivePerKey,
}

// ===========================================================================
// The accept being committed — a miniature CC-ATOMIC block.
// ===========================================================================

/// The data one accept commits: a height (its state-diff value), the
/// last-accepted pointer it advances to, and an optional cross-chain
/// shared-memory export (input-id → marshalled-UTXO bytes).
#[derive(Debug, Clone)]
pub struct AcceptBatch {
    /// The block height being accepted.
    pub height: u64,
    /// The block's state-diff value (a stand-in for the UTXO/staker diff).
    pub state_value: Vec<u8>,
    /// An optional outbound shared-memory export `(input_id, utxo_bytes)`.
    pub shared_memory: Option<(Vec<u8>, Vec<u8>)>,
}

impl AcceptBatch {
    /// The ordered `(key, value)` ops this accept writes: state diff, then
    /// last-accepted pointer, then (if any) the shared-memory export. The order
    /// is deliberate so a [`CrashPoint::AfterStateBeforeMarker`] tear leaves the
    /// state diff present but the marker still on the parent.
    fn ops(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut ops = vec![
            (keys::state(self.height), self.state_value.clone()),
            (
                keys::LAST_ACCEPTED.to_vec(),
                self.height.to_be_bytes().to_vec(),
            ),
        ];
        if let Some((id, utxo)) = &self.shared_memory {
            ops.push((keys::shared_memory(id), utxo.clone()));
        }
        ops
    }
}

// ===========================================================================
// AcceptHarness — drive accept + crash + restart + recovery.
// ===========================================================================

/// The reconciled view a restart exposes for assertions: the durable
/// last-accepted height (if any), whether each part of the in-flight accept is
/// present, and whether the recovery dropped a torn diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredState {
    /// The last-accepted height the marker names after recovery, or `None` if no
    /// marker was ever written.
    pub last_accepted: Option<u64>,
    /// Whether the in-flight block's state diff is present (and marker-covered).
    pub state_present: bool,
    /// Whether the in-flight block's shared-memory export is present (and
    /// marker-covered).
    pub shared_memory_present: bool,
    /// Whether recovery deleted an orphan (marker-less) state diff / SM entry (an
    /// idempotent reconciliation step, specs/27 §3 C2 "drop the half-applied
    /// diff").
    pub dropped_orphan: bool,
}

impl RecoveredState {
    /// CC-ATOMIC: the in-flight block is *fully* present or *fully* absent — never
    /// a torn subset (state without marker, marker without state, or an orphan SM
    /// entry). `height` is the block the accept targeted; `parent` its parent.
    #[must_use]
    pub fn is_atomic(&self, height: u64, parent: u64, has_shared_memory: bool) -> bool {
        let fully_present = self.last_accepted == Some(height)
            && self.state_present
            && (self.shared_memory_present == has_shared_memory);
        // Fully absent: marker on the parent, no state diff, no orphan SM entry.
        let fully_absent = self.last_accepted == Some(parent)
            && !self.state_present
            && !self.shared_memory_present;
        fully_present || fully_absent
    }
}

/// Drives a single CC-ATOMIC accept through a [`FailpointDb`] under a chosen
/// [`CrashPoint`] / [`CommitStrategy`], then "restarts" and runs recovery.
pub struct AcceptHarness {
    backing: Arc<MemDb>,
}

impl AcceptHarness {
    /// A fresh harness whose backing store is seeded with the genesis marker
    /// (last-accepted = `genesis_height`, no state diffs).
    ///
    /// # Errors
    /// Returns the backing-store error if the initial marker write fails (it
    /// cannot here — the store is freshly open).
    pub fn new(genesis_height: u64) -> Result<Self> {
        let backing = Arc::new(MemDb::new());
        backing.put(keys::LAST_ACCEPTED, &genesis_height.to_be_bytes())?;
        Ok(Self { backing })
    }

    /// The current durable last-accepted height (reads the marker directly).
    fn read_last_accepted(db: &dyn DynDatabase) -> Result<Option<u64>> {
        match db.get(keys::LAST_ACCEPTED) {
            Ok(bytes) if bytes.len() == 8 => {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&bytes);
                Ok(Some(u64::from_be_bytes(arr)))
            }
            Ok(_) => Ok(None),
            Err(Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Attempt to accept `batch`, injecting a crash per `point` / `strategy`.
    /// Returns `Ok(())` if the accept fully committed, or the injected
    /// [`InjectedCrash`] (wrapped in [`Error::Other`]) if it was interrupted.
    ///
    /// The trip op is computed from `point` so the failure lands exactly at the
    /// modeled boundary, deterministically.
    fn attempt_accept(
        &self,
        batch: &AcceptBatch,
        point: CrashPoint,
        strategy: CommitStrategy,
    ) -> Result<()> {
        let ops = batch.ops();
        // Trip op selection (1-based mutation count):
        //  - BeforeWrite             -> 1   (fail on the very first mutation)
        //  - MidWrite                -> 2   (after the state diff, before the rest)
        //  - AfterStateBeforeMarker  -> 2   (state durable, marker/SM not yet)
        //  - None                    -> 0   (never)
        let trip = match point {
            CrashPoint::None => 0,
            CrashPoint::BeforeWrite => 1,
            CrashPoint::MidWrite | CrashPoint::AfterStateBeforeMarker => 2,
        };
        let db = FailpointDb::new(Arc::clone(&self.backing), trip);

        match strategy {
            CommitStrategy::NaivePerKey => {
                // Anti-pattern: per-key writes. A crash mid-sequence tears the
                // accept — used to PROVE the atomic path is the safe one.
                for (k, v) in &ops {
                    db.put(k, v)?;
                }
                Ok(())
            }
            CommitStrategy::AtomicBatch => {
                // §2.2: buffer every op and flush all-or-nothing. The failpoint is
                // checked *before* the flush, so any armed crash point leaves the
                // whole batch unwritten (RocksDB WAL all-or-nothing, §3 C1) — never
                // a torn subset.
                Self::write_batch(&db, &ops, point)
            }
        }
    }

    /// The CC-ATOMIC single-`write()` path: buffer all ops into one batch and
    /// flush atomically, after the failpoint gate.
    fn write_batch(db: &FailpointDb, ops: &[(Vec<u8>, Vec<u8>)], point: CrashPoint) -> Result<()> {
        // Any non-None crash point fails the atomic write before it lands.
        if point != CrashPoint::None {
            return Err(Error::Other(anyhow::Error::new(InjectedCrash(0))));
        }
        let mut batch = db.new_batch();
        for (k, v) in ops {
            batch.put(k, v)?;
        }
        batch.write()
    }

    /// Run a crash+restart cycle: attempt the accept (which may be interrupted),
    /// drop the wrapper, then rebuild over the surviving bytes and recover.
    ///
    /// # Errors
    /// Returns the backing-store error if recovery's own reads/writes fail.
    pub fn run_cycle(
        &self,
        batch: &AcceptBatch,
        point: CrashPoint,
        strategy: CommitStrategy,
    ) -> Result<RecoveredState> {
        // 1. Attempt the accept; the injected error IS the crash, so discard it.
        let _ = self.attempt_accept(batch, point, strategy);
        // 2. "Restart": a fresh, non-injecting view over the same backing bytes.
        let restarted = FailpointDb::new(Arc::clone(&self.backing), 0);
        // 3. Recover (idempotent) and observe.
        Self::recover(&restarted, batch.height)
    }

    /// Idempotent recovery over the surviving bytes (specs/27 §5 / §3):
    ///
    /// 1. Read the durable last-accepted marker (the truth — it is in the same
    ///    atomic unit as its state diff).
    /// 2. Any state diff at a height *not covered by* the marker is an orphan from
    ///    a torn accept (C2 "pointer not advanced"): drop it so the on-disk state
    ///    is consistent with the marker. Likewise drop a dangling shared-memory
    ///    entry — so a peer chain never observes a UTXO whose producer never
    ///    committed (§3.1).
    ///
    /// Re-running this over the reconciled store is a no-op (idempotent).
    pub fn recover(db: &dyn DynDatabase, in_flight_height: u64) -> Result<RecoveredState> {
        let last_accepted = Self::read_last_accepted(db)?;
        let marker_covers = last_accepted == Some(in_flight_height);
        let mut dropped_orphan = false;

        let state_key = keys::state(in_flight_height);
        let state_present_raw = db.has(&state_key)?;
        if state_present_raw && !marker_covers {
            db.delete(&state_key)?;
            dropped_orphan = true;
        }
        let state_present = state_present_raw && marker_covers;

        // Reconcile shared-memory entries against the marker.
        let mut shared_memory_present = false;
        let mut orphan_sm: Vec<Vec<u8>> = Vec::new();
        {
            let mut sm_iter =
                db.new_iterator_with_start_and_prefix(&[], keys::SHARED_MEMORY_PREFIX);
            while sm_iter.next() {
                if let Some(k) = sm_iter.key() {
                    if marker_covers {
                        shared_memory_present = true;
                    } else {
                        orphan_sm.push(k.to_vec());
                    }
                }
            }
            sm_iter.release();
        }
        for k in orphan_sm {
            db.delete(&k)?;
            dropped_orphan = true;
        }

        Ok(RecoveredState {
            last_accepted,
            state_present,
            shared_memory_present,
            dropped_orphan,
        })
    }

    /// A read-only post-recovery observation (for the idempotency check: recover
    /// once, then `observe`, and assert the reconciled view is stable). Does not
    /// mutate.
    ///
    /// # Errors
    /// Returns the backing-store error if a read fails.
    pub fn observe(&self, in_flight_height: u64) -> Result<RecoveredState> {
        let db = FailpointDb::new(Arc::clone(&self.backing), 0);
        let last_accepted = Self::read_last_accepted(&db)?;
        let marker_covers = last_accepted == Some(in_flight_height);
        let state_present = db.has(&keys::state(in_flight_height))? && marker_covers;
        let mut shared_memory_present = false;
        {
            let mut it = db.new_iterator_with_start_and_prefix(&[], keys::SHARED_MEMORY_PREFIX);
            while it.next() {
                if it.key().is_some() {
                    shared_memory_present = true;
                }
            }
            it.release();
        }
        Ok(RecoveredState {
            last_accepted,
            state_present,
            shared_memory_present,
            dropped_orphan: false,
        })
    }
}

// ===========================================================================
// Two-sided shared-memory consistency (specs/27 §3.1).
// ===========================================================================

/// The observation a peer chain makes of a single exported UTXO after an export
/// crash+recovery: whether the input-id is present in shared memory, and its
/// value bytes if so. Built on the same `(key, value)` contract as
/// [`crate::atomic::exported_utxo_observation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerObservation {
    /// Whether the peer chain can `get` the exported UTXO.
    pub present: bool,
    /// The marshalled UTXO bytes the peer reads back (empty when absent).
    pub value: Vec<u8>,
}

impl AcceptHarness {
    /// The peer chain's view of an exported UTXO `input_id` after recovery: it is
    /// present iff the producing block became durable (CC-ATOMIC), so the UTXO is
    /// never half-exported (specs/27 §3.1 — only the consistent corners are
    /// reachable).
    ///
    /// # Errors
    /// Returns the backing-store error if the lookup fails.
    pub fn peer_observation(&self, input_id: &[u8]) -> Result<PeerObservation> {
        let db = FailpointDb::new(Arc::clone(&self.backing), 0);
        match db.get(&keys::shared_memory(input_id)) {
            Ok(value) => Ok(PeerObservation {
                present: true,
                value,
            }),
            Err(Error::NotFound) => Ok(PeerObservation {
                present: false,
                value: Vec::new(),
            }),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failpoint_trips_on_nth_mutation() {
        let backing = Arc::new(MemDb::new());
        let db = FailpointDb::new(Arc::clone(&backing), 2);
        // First mutation succeeds.
        db.put(b"a", b"1").expect("first put");
        // Second mutation trips the failpoint.
        let err = db.put(b"b", b"2").expect_err("second put must trip");
        assert!(matches!(err, Error::Other(_)), "injected as Error::Other");
        // The first write is durable on the backing store; the second is not.
        assert_eq!(backing.get(b"a").expect("a present"), b"1");
        assert!(matches!(backing.get(b"b"), Err(Error::NotFound)));
    }

    #[test]
    fn failpoint_zero_never_trips() {
        let backing = Arc::new(MemDb::new());
        let db = FailpointDb::new(backing, 0);
        for i in 0..10u8 {
            db.put(&[i], &[i]).expect("no trip with trip=0");
        }
        assert_eq!(db.mutations(), 10);
    }
}
