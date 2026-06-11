// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-(chain, container-kind) accepted-container index (Go `indexer/index.go`;
//! spec 12 §5).
//!
//! An [`Index`] records containers in their order of acceptance. Each
//! [`Index::accept`] writes — under one versioned batch committed atomically
//! to the base DB —
//!
//! - `index → marshaled Container` (prefix `0x01`),
//! - `containerID → index` (prefix `0x02`),
//! - the advanced `nextAcceptedIndex` (key `0x00`).
//!
//! Invariants (Go `index.go`): thread-safe; `accept` is called *before* the
//! VM commits the container, in acceptance order; a replayed accept after a
//! crash-restart is deduped (first write wins).

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use parking_lot::RwLock;

use ava_database::helpers::{get_u64, pack_u64, put_u64};
use ava_database::{Database, KeyValueReader, KeyValueWriter, PrefixDb, VersionDb};
use ava_types::id::Id;
use ava_utils::clock::Clock;

use crate::container::Container;
use crate::error::{Error, Result};

/// Maximum number of containers fetched by one `getContainerRange` call
/// (Go `MaxFetchedByRange`).
pub const MAX_FETCHED_BY_RANGE: u64 = 1024;

/// Key of the persisted next-accepted index (Go `nextAcceptedIndexKey`).
pub(crate) const NEXT_ACCEPTED_INDEX_KEY: [u8; 1] = [0x00];
/// Namespace of the `index → container` mapping (Go `indexToContainerPrefix`).
pub(crate) const INDEX_TO_CONTAINER_PREFIX: [u8; 1] = [0x01];
/// Namespace of the `containerID → index` mapping (Go `containerToIDPrefix`).
pub(crate) const CONTAINER_TO_ID_PREFIX: [u8; 1] = [0x02];

/// The read surface the JSON-RPC service is generic over (object-safe so
/// [`crate::service::IndexService`] stays non-generic).
pub trait IndexReader: Send + Sync {
    /// The `index`th accepted container (Go `GetContainerByIndex`).
    ///
    /// # Errors
    /// [`Error::NoContainerAtIndex`] if nothing was accepted at `index`.
    fn get_container_by_index(&self, index: u64) -> Result<Container>;

    /// Containers at `start_index ..` capped both by `num_to_fetch` and the
    /// last accepted index (Go `GetContainerRange`).
    ///
    /// # Errors
    /// [`Error::NumToFetchInvalid`] outside `[1, MAX_FETCHED_BY_RANGE]`,
    /// [`Error::NoneAccepted`] on an empty index, [`Error::StartIndexTooHigh`]
    /// past the end.
    fn get_container_range(&self, start_index: u64, num_to_fetch: u64) -> Result<Vec<Container>>;

    /// The acceptance index of `container_id` (Go `GetIndex`).
    ///
    /// # Errors
    /// [`Error::Database`] with `NotFound` if the container is not indexed.
    fn get_index(&self, container_id: &Id) -> Result<u64>;

    /// The container with id `container_id` (Go `GetContainerByID`).
    ///
    /// # Errors
    /// [`Error::Database`] with `NotFound` if the container is not indexed.
    fn get_container_by_id(&self, container_id: &Id) -> Result<Container>;

    /// The most recently accepted container (Go `GetLastAccepted`).
    ///
    /// # Errors
    /// [`Error::NoneAccepted`] if nothing has been accepted.
    fn get_last_accepted(&self) -> Result<Container>;
}

/// An append-only index of accepted containers (Go `indexer.index`).
///
/// `D` is the per-chain database the indexer carves out for this index
/// (`chainID ‖ kind-byte` prefix over the indexer's DB).
pub struct Index<D: Database> {
    clock: Arc<dyn Clock>,
    /// The next acceptance index; the lock orders writers like Go's
    /// `index.lock` (readers snapshot it for bound checks).
    next_accepted_index: RwLock<u64>,
    /// The base DB, closed on [`Index::close`] (Go's `baseDB`).
    base_db: Arc<D>,
    /// The versioned overlay every write goes through; committed per accept.
    vdb: Arc<VersionDb<D>>,
    /// `index → container` (prefix `0x01` over `vdb`).
    index_to_container: PrefixDb<VersionDb<D>>,
    /// `containerID → index` (prefix `0x02` over `vdb`).
    container_to_index: PrefixDb<VersionDb<D>>,
}

impl<D: Database> Index<D> {
    /// Opens (or creates) the index over `base_db`, restoring the persisted
    /// `nextAcceptedIndex` (Go `newIndex`).
    ///
    /// # Errors
    /// Propagates a database failure reading the next-accepted marker.
    pub fn new(base_db: Arc<D>, clock: Arc<dyn Clock>) -> Result<Self> {
        let vdb = Arc::new(VersionDb::new_arc(Arc::clone(&base_db)));
        let index_to_container = PrefixDb::new_arc(&INDEX_TO_CONTAINER_PREFIX, Arc::clone(&vdb));
        let container_to_index = PrefixDb::new_arc(&CONTAINER_TO_ID_PREFIX, Arc::clone(&vdb));

        let next_accepted_index = match get_u64(&*vdb, &NEXT_ACCEPTED_INDEX_KEY) {
            Ok(v) => v,
            Err(ava_database::Error::NotFound) => 0,
            Err(e) => return Err(e.into()),
        };
        tracing::info!(next_accepted_index, "created new index");

        Ok(Self {
            clock,
            next_accepted_index: RwLock::new(next_accepted_index),
            base_db,
            vdb,
            index_to_container,
            container_to_index,
        })
    }

    /// Closes this index and its base DB (Go `index.Close`).
    ///
    /// # Errors
    /// Returns the first close failure after attempting every close.
    pub fn close(&self) -> Result<()> {
        let r1 = Database::close(&self.index_to_container);
        let r2 = Database::close(&self.container_to_index);
        let r3 = Database::close(&*self.vdb);
        let r4 = Database::close(&*self.base_db);
        r1.and(r2).and(r3).and(r4).map_err(Error::from)
    }

    /// Indexes an accepted container (Go `index.Accept`). An error is fatal
    /// to the indexer: the VM must not commit this or later containers.
    ///
    /// A container already indexed (a restart replay: this index committed it
    /// but the node died before the VM did) is skipped.
    ///
    /// # Errors
    /// Propagates database/codec failures; all-or-nothing via the versioned
    /// batch.
    pub fn accept(&self, container_id: Id, container_bytes: &[u8]) -> Result<()> {
        let mut next = self.next_accepted_index.write();

        match self.container_to_index.get(container_id.as_bytes()) {
            Ok(_) => {
                tracing::debug!(%container_id, "not indexing already accepted container");
                return Ok(());
            }
            Err(ava_database::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }

        tracing::debug!(
            next_accepted_index = *next,
            %container_id,
            "indexing container"
        );

        // index → container.
        let next_bytes = pack_u64(*next);
        let marshaled = Container {
            id: container_id,
            bytes: container_bytes.to_vec(),
            timestamp: unix_nanos(self.clock.as_ref()),
        }
        .marshal()?;
        self.index_to_container.put(&next_bytes, &marshaled)?;

        // containerID → index.
        self.container_to_index
            .put(container_id.as_bytes(), &next_bytes)?;

        // Advance + persist nextAcceptedIndex (cannot overflow in practice;
        // Go increments unchecked).
        *next = next.saturating_add(1);
        put_u64(&*self.vdb, &NEXT_ACCEPTED_INDEX_KEY, *next)?;

        // Atomically commit all three writes to the base DB.
        self.vdb.commit()?;
        Ok(())
    }

    /// The last accepted index, if anything has been accepted.
    fn last_accepted_index(next: u64) -> Option<u64> {
        next.checked_sub(1)
    }

    /// Reads + decodes the container stored under the packed `index` key.
    fn container_by_index_bytes(&self, key: &[u8]) -> Result<Container> {
        let bytes = self.index_to_container.get(key).map_err(|e| {
            tracing::error!(error = %e, "couldn't read container from database");
            Error::ReadFailed(e)
        })?;
        Container::unmarshal(&bytes)
    }

    /// Bounds-checked fetch of the container at `index` given the current
    /// `next` snapshot.
    fn container_at(&self, next: u64, index: u64) -> Result<Container> {
        let last = Self::last_accepted_index(next).ok_or(Error::NoContainerAtIndex(index))?;
        if index > last {
            return Err(Error::NoContainerAtIndex(index));
        }
        self.container_by_index_bytes(&pack_u64(index))
    }
}

/// The clock's wall reading as Unix nanoseconds (Go `time.Time.UnixNano`).
fn unix_nanos(clock: &dyn Clock) -> i64 {
    match clock.now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        // Pre-epoch reading: negative nanos, mirroring Go.
        Err(e) => i64::try_from(e.duration().as_nanos())
            .unwrap_or(i64::MAX)
            .checked_neg()
            .unwrap_or(i64::MIN),
    }
}

impl<D: Database> IndexReader for Index<D> {
    fn get_container_by_index(&self, index: u64) -> Result<Container> {
        let next = self.next_accepted_index.read();
        self.container_at(*next, index)
    }

    fn get_container_range(&self, start_index: u64, num_to_fetch: u64) -> Result<Vec<Container>> {
        if num_to_fetch == 0 || num_to_fetch > MAX_FETCHED_BY_RANGE {
            return Err(Error::NumToFetchInvalid(num_to_fetch));
        }

        let next = self.next_accepted_index.read();
        let last = Self::last_accepted_index(*next).ok_or(Error::NoneAccepted)?;
        if start_index > last {
            return Err(Error::StartIndexTooHigh {
                start: start_index,
                last,
            });
        }

        // Truncate the window to what exists; `num_to_fetch >= 1` so the
        // subtraction is safe and `last_index >= start_index`.
        let last_index = start_index
            .saturating_add(num_to_fetch.saturating_sub(1))
            .min(last);
        let capacity = usize::try_from(last_index.saturating_sub(start_index).saturating_add(1))
            .unwrap_or(usize::MAX);
        let mut containers = Vec::with_capacity(capacity);
        for index in start_index..=last_index {
            containers.push(self.container_at(*next, index)?);
        }
        Ok(containers)
    }

    fn get_index(&self, container_id: &Id) -> Result<u64> {
        let _guard = self.next_accepted_index.read();
        Ok(get_u64(&self.container_to_index, container_id.as_bytes())?)
    }

    fn get_container_by_id(&self, container_id: &Id) -> Result<Container> {
        let _guard = self.next_accepted_index.read();
        // Go returns the raw database error (e.g. `not found`) unwrapped.
        let index_bytes = self.container_to_index.get(container_id.as_bytes())?;
        self.container_by_index_bytes(&index_bytes)
    }

    fn get_last_accepted(&self) -> Result<Container> {
        let next = self.next_accepted_index.read();
        let last = Self::last_accepted_index(*next).ok_or(Error::NoneAccepted)?;
        self.container_at(*next, last)
    }
}

#[cfg(test)]
// Tests index into fixtures and `serde_json::Value` replies and do plain
// test-fixture arithmetic (`UNIX_EPOCH + ...`), both idiomatic in tests
// (precedent: ava-api jsonrpc tests).
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use ava_database::helpers;
    use ava_database::memdb::MemDb;
    use ava_types::id::Id;
    use ava_utils::clock::MockClock;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::error::Error;

    fn test_clock(unix_nanos: u64) -> Arc<MockClock> {
        Arc::new(MockClock::at(UNIX_EPOCH + Duration::from_nanos(unix_nanos)))
    }

    fn test_id(i: u8) -> Id {
        Id::from([i; 32])
    }

    fn test_bytes(i: u8) -> Vec<u8> {
        vec![i, i.wrapping_add(1), i.wrapping_add(2)]
    }

    // ------------------------------------------------------------------
    // Red (M8.24): `Accept` writes containerID→bytes, index→containerID,
    // containerID→index, and advances nextAcceptedIndex atomically (12 §5);
    // the persisted markers survive a restart (Go TestIndex).
    // ------------------------------------------------------------------
    #[test]
    fn accept_ordering_and_markers() {
        const N: u8 = 16;
        // Mirror Go TestIndex: the index's base is a fresh versiondb over a
        // shared memdb per "run", committed before close so the second run
        // sees the state.
        let base: Arc<MemDb> = Arc::new(MemDb::new());
        let run_db = Arc::new(ava_database::VersionDb::new_arc(Arc::clone(&base)));
        let clock = test_clock(1_700_000_000_000_000_000);
        let index = Index::new(Arc::clone(&run_db), clock.clone()).expect("Index::new()");

        for i in 0..N {
            // Timestamps advance per accept (injectable clock).
            clock.advance(Duration::from_secs(1));
            let (id, bytes) = (test_id(i), test_bytes(i));
            index.accept(id, &bytes).expect("Index::accept()");

            let i = u64::from(i);
            // containerID → index.
            assert_eq!(i, index.get_index(&id).expect("Index::get_index()"));
            // index → containerID + bytes.
            let got = index
                .get_container_by_index(i)
                .expect("Index::get_container_by_index()");
            assert_eq!(id, got.id, "get_container_by_index id");
            assert_eq!(bytes, got.bytes, "get_container_by_index bytes");
            // containerID → bytes.
            let got = index
                .get_container_by_id(&id)
                .expect("Index::get_container_by_id()");
            assert_eq!(bytes, got.bytes, "get_container_by_id bytes");
            // Most recent accept is the last accepted.
            let last = index
                .get_last_accepted()
                .expect("Index::get_last_accepted()");
            assert_eq!(id, last.id, "get_last_accepted id");
            // Range of one starting at i.
            let range = index
                .get_container_range(i, 1)
                .expect("Index::get_container_range(i, 1)");
            assert_eq!(1, range.len(), "get_container_range(i, 1) length");
            assert_eq!(bytes, range[0].bytes, "get_container_range(i, 1) bytes");
            // Asking past the end is truncated, not an error.
            let range = index
                .get_container_range(i, 2)
                .expect("Index::get_container_range(i, 2)");
            assert_eq!(1, range.len(), "get_container_range(i, 2) length");
        }

        // nextAcceptedIndex marker is persisted (key 0x00, big-endian u64)
        // through the versioned batch: a fresh Index over the same base picks
        // it up (restart semantics; Go TestIndex part 2).
        run_db.commit().expect("VersionDb::commit()");
        index.close().expect("Index::close()");
        let run2_db = Arc::new(ava_database::VersionDb::new_arc(Arc::clone(&base)));
        let reopened = Index::new(run2_db, clock).expect("Index::new() reopen");
        let all = reopened
            .get_container_range(0, u64::from(N))
            .expect("Index::get_container_range() after reopen");
        assert_eq!(usize::from(N), all.len(), "all containers after reopen");
        let mut last_ts = i64::MIN;
        for (i, container) in all.iter().enumerate() {
            assert_eq!(test_id(u8::try_from(i).expect("i fits u8")), container.id);
            assert_eq!(
                test_bytes(u8::try_from(i).expect("i fits u8")),
                container.bytes
            );
            // Timestamps are non-decreasing in acceptance order.
            assert!(container.timestamp >= last_ts, "timestamps non-decreasing");
            last_ts = container.timestamp;
        }

        // The raw nextAcceptedIndex marker is persisted under key 0x00 in
        // Go's PackUInt64 (big-endian) layout, committed through to the base.
        let next =
            helpers::get_u64(&*base, &NEXT_ACCEPTED_INDEX_KEY).expect("nextAcceptedIndex marker");
        assert_eq!(u64::from(N), next, "persisted nextAcceptedIndex");
    }

    // Replayed accepts (restart replay before the VM committed) are deduped:
    // the first write wins (Go TestDontIndexSameContainerTwice).
    #[test]
    fn dont_index_same_container_twice() {
        let base: Arc<MemDb> = Arc::new(MemDb::new());
        let index = Index::new(base, test_clock(0)).expect("Index::new()");

        let id = test_id(7);
        index.accept(id, &[1, 2, 3]).expect("Index::accept() first");
        index
            .accept(id, &[4, 5, 6])
            .expect("Index::accept() replay");

        let err = index
            .get_container_by_index(1)
            .expect_err("no container at 1");
        assert!(
            matches!(err, Error::NoContainerAtIndex(1)),
            "Index::get_container_by_index(1) after dedupe: {err}"
        );
        let got = index
            .get_container_by_id(&id)
            .expect("Index::get_container_by_id()");
        assert_eq!(vec![1, 2, 3], got.bytes, "first accept wins");
    }

    // GetContainerRange is capped at MAX_FETCHED_BY_RANGE = 1024 and its
    // error strings are Go-byte-stable (Go TestIndexGetContainerByRangeMaxPageSize).
    #[test]
    fn get_container_range_bounds() {
        let base: Arc<MemDb> = Arc::new(MemDb::new());
        let index = Index::new(base, test_clock(0)).expect("Index::new()");

        // Empty index: range errors with the none-accepted sentinel.
        let err = index.get_container_range(0, 1).expect_err("none accepted");
        assert_eq!(
            "no containers have been accepted",
            err.to_string(),
            "Index::get_container_range() on empty index"
        );

        for i in 0..=MAX_FETCHED_BY_RANGE {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&i.to_be_bytes());
            index
                .accept(Id::from(id), &id[..8])
                .expect("Index::accept()");
        }

        let err = index
            .get_container_range(0, MAX_FETCHED_BY_RANGE + 1)
            .expect_err("page size too large");
        assert_eq!(
            "numToFetch must be in [1,1024] but is 1025",
            err.to_string(),
            "Index::get_container_range() page-size error"
        );
        let err = index.get_container_range(0, 0).expect_err("zero page size");
        assert_eq!(
            "numToFetch must be in [1,1024] but is 0",
            err.to_string(),
            "Index::get_container_range() zero-page error"
        );
        let err = index
            .get_container_range(MAX_FETCHED_BY_RANGE + 1, 1)
            .expect_err("start beyond last");
        assert_eq!(
            "start index (1025) > last accepted index (1024)",
            err.to_string(),
            "Index::get_container_range() start-bound error"
        );

        let containers = index
            .get_container_range(0, MAX_FETCHED_BY_RANGE)
            .expect("Index::get_container_range(0, max)");
        assert_eq!(
            usize::try_from(MAX_FETCHED_BY_RANGE).expect("usize"),
            containers.len()
        );

        let shifted = index
            .get_container_range(1, MAX_FETCHED_BY_RANGE)
            .expect("Index::get_container_range(1, max)");
        assert_eq!(containers[1], shifted[0], "ranges overlap consistently");

        // The tail window is truncated to what exists.
        let tail = index
            .get_container_range(MAX_FETCHED_BY_RANGE - 1, MAX_FETCHED_BY_RANGE)
            .expect("Index::get_container_range(tail)");
        assert_eq!(2, tail.len(), "tail window length");
    }
}
