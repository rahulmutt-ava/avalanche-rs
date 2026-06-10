// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Cross-chain (atomic) tx gossip for the C-Chain (specs/11 §8 upstream-delta;
//! Go `vms/saevm/cchain/gossip.go`, `ab442aa244` #5408).
//!
//! Port of Go's `gossipTx` / `gossipMarshaller` / `gossipTxPool`, plus the
//! [`BloomSet`] + push/pull gossipers that `cchain/vm.go::Initialize` wires onto
//! the atomic [`AtomicTxpool`].
//!
//! # AS-BUILT deviations (bloom + transport stand-ins)
//!
//! Two pieces of Go infrastructure have no Rust analog yet, so — following the
//! M7.20 `ava-saevm-txgossip` local-seam precedent — this module implements the
//! genuinely consensus-relevant behaviour behind minimal local seams:
//!
//! * **No `network/p2p/gossip` framework.** Go's `gossip.NewBloomSet` /
//!   `gossip.NewSystem` (push/pull `Gossiper`s over a `common.AppSender`) do not
//!   exist in `ava-network` (only `Network::gossip(...)` + a read-only bloom
//!   filter). So [`Gossipable`] is a local trait (mirroring `txgossip`), and the
//!   gossip "Set" — [`BloomSet`] — couples the [`AtomicTxpool`] with an owned
//!   `HashSet<Id>` of seen gossip ids. The bloom-filter *writer* (the outbound
//!   pull membership filter) is the `HashSet`; this matches Go's **observable**
//!   behaviour (dedup on admission + "do I already have this tx?" filtering for
//!   pull gossip), and is documented as the bloom stand-in until a generic
//!   gossip framework lands. `// TODO(M8): real `utils/bloom` writer + metrics`.
//! * **No live `AppSender`.** The push/pull loops run over the
//!   [`GossipTransport`] seam (the same shape `txgossip` uses); the production
//!   wiring of that seam to `ava_network::Network::gossip` is M8. The in-memory
//!   `ava-saevm-testutil` network harness drives the seam for multi-node tests.
//!
//! # Gossip id representation
//!
//! Go's atomic-tx `GossipID` is an avalanche `ids.ID` (32 bytes); `txgossip`'s
//! EVM gossip id is a reth `B256` (also 32 bytes). They are layout-compatible;
//! the atomic side uses [`ava_types::id::Id`] directly (the tx's own id), so no
//! conversion is needed.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use ava_types::id::Id;
use parking_lot::Mutex;

use crate::tx::Tx;
use crate::txpool::{AtomicTxpool, Error as TxpoolError};

/// Push-gossip period: re-broadcast newly admitted atomic txs every 100 ms
/// in production (Go `cchain/vm.go` default `pushGossipPeriod`). Tests pass a
/// shorter period via [`GossipConfig`](crate::vm::GossipConfig).
pub const PUSH_GOSSIP_PERIOD: Duration = Duration::from_millis(100);

/// Pull-gossip period: reconcile the atomic pool against peers every 1 s
/// (Go `cchain/vm.go` default `pullGossipPeriod`).
pub const PULL_GOSSIP_PERIOD: Duration = Duration::from_secs(1);

/// A value identified for gossip deduplication (Go `gossip.Gossipable`).
///
/// Local trait — there is no generic `gossip::Gossipable` in `ava-network` (see
/// the module docs). For atomic txs the gossip id is the tx id.
pub trait Gossipable {
    /// The gossip deduplication id (Go `GossipID()`).
    fn gossip_id(&self) -> Id;
}

/// The atomic [`Tx`] wrapped so it is [`Gossipable`] (Go `gossipTx`).
///
/// The wire form is the linear-codec [`Tx`] bytes ([`GossipMarshaller`]); the
/// gossip id is the tx id (`sha256(bytes)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GossipTx(Tx);

impl GossipTx {
    /// Wraps a [`Tx`] for gossip (Go `toGossipTx`).
    #[must_use]
    pub fn new(tx: Tx) -> Self {
        Self(tx)
    }

    /// Borrows the inner tx (Go `gossipTx.toTx`).
    #[must_use]
    pub fn as_tx(&self) -> &Tx {
        &self.0
    }

    /// Consumes the wrapper, returning the inner tx.
    #[must_use]
    pub fn into_tx(self) -> Tx {
        self.0
    }
}

impl Gossipable for GossipTx {
    fn gossip_id(&self) -> Id {
        self.0.id()
    }
}

/// Errors from (de)serializing a gossiped atomic tx.
#[derive(Debug, thiserror::Error)]
pub enum MarshalError {
    /// The bytes were not a valid linear-codec atomic [`Tx`].
    #[error("decoding gossiped tx: {0}")]
    Decode(#[from] crate::tx::Error),
}

/// Marshaller for gossiping a [`GossipTx`] (Go `gossipMarshaller`).
///
/// `marshal` emits the canonical linear-codec [`Tx`] bytes (Go
/// `MarshalGossip` → `tx.Bytes()`); `unmarshal` decodes via [`Tx::parse`] (Go
/// `UnmarshalGossip` → `tx.Parse`). The round-trip is id-stable.
#[derive(Debug, Default, Clone, Copy)]
pub struct GossipMarshaller;

impl GossipMarshaller {
    /// A new (stateless) marshaller.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Serializes `tx` for the wire (Go `MarshalGossip`).
    ///
    /// # Errors
    /// [`MarshalError::Decode`] if the tx cannot be marshalled (a malformed tx).
    // Stateless today, but `&self` is kept for parity with Go's `gossipMarshaller`
    // and the future generic gossip framework's codec-carrying `Marshaller`.
    #[allow(clippy::unused_self, clippy::trivially_copy_pass_by_ref)]
    pub fn marshal(&self, tx: &GossipTx) -> Result<Vec<u8>, MarshalError> {
        Ok(tx.0.marshal()?)
    }

    /// Decodes a [`GossipTx`] from its wire bytes (Go `UnmarshalGossip`).
    ///
    /// # Errors
    /// [`MarshalError::Decode`] if the bytes are not a valid atomic [`Tx`].
    #[allow(clippy::unused_self, clippy::trivially_copy_pass_by_ref)]
    pub fn unmarshal(&self, bytes: &[u8]) -> Result<GossipTx, MarshalError> {
        Ok(GossipTx::new(Tx::parse(bytes)?))
    }
}

/// The gossip "Set": the atomic [`AtomicTxpool`] coupled with a seen-id dedup
/// view (Go `gossip.BloomSet` over `gossipTxPool`; the bloom stand-in — see the
/// module docs).
///
/// [`BloomSet::add`] admits a tx into the pool (Go `gossipTxPool.Add` →
/// `Txpool.Add`) and records its id as seen; [`BloomSet::snapshot`] snapshots
/// the pooled txs (Go `gossipTxPool.Iterate`); [`BloomSet::seen`] is the
/// pull-gossip membership filter (Go `gossip.BloomFilter` containment).
pub struct BloomSet {
    pool: Arc<AtomicTxpool>,
    /// Seen gossip ids — the dedup / bloom-membership view (stand-in for the
    /// real `utils/bloom` writer; see the module docs).
    seen: Mutex<HashSet<Id>>,
}

impl BloomSet {
    /// Builds a gossip set over `pool`, pre-seeding the seen view from the
    /// pool's current contents (so a pre-existing tx is already "known").
    #[must_use]
    pub fn new(pool: Arc<AtomicTxpool>) -> Self {
        let seen = pool.txs().iter().map(Tx::id).collect();
        Self {
            pool,
            seen: Mutex::new(seen),
        }
    }

    /// The underlying atomic txpool.
    #[must_use]
    pub fn pool(&self) -> &Arc<AtomicTxpool> {
        &self.pool
    }

    /// Admits `tx` into the pool and records its id as seen (Go
    /// `gossipTxPool.Add`). Re-admitting an already-known tx records it as seen
    /// and returns `Ok(())` (the pool's `AlreadyKnown` is ignored, matching Go).
    ///
    /// # Errors
    /// Returns [`TxpoolError`] only for a non-`AlreadyKnown` rejection (none
    /// today; future state-verified admission may reject).
    // The `Result` is Go-faithful (`gossipSet.Add` returns an error) and
    // future-proof: M7.x's state-verified `Txpool.Add` introduces
    // non-`AlreadyKnown` rejections. Today the only error is `AlreadyKnown`
    // (ignored), so the body never takes the error path.
    #[allow(clippy::unnecessary_wraps)]
    pub fn add(&self, tx: GossipTx) -> Result<(), TxpoolError> {
        let id = tx.gossip_id();
        self.seen.lock().insert(id);
        match self.pool.add(tx.into_tx()) {
            Ok(()) | Err(TxpoolError::AlreadyKnown) => Ok(()),
        }
    }

    /// Snapshots the pooled txs as [`GossipTx`]es (Go `gossipTxPool.Iterate`).
    #[must_use]
    pub fn snapshot(&self) -> Vec<GossipTx> {
        self.pool.txs().into_iter().map(GossipTx::new).collect()
    }

    /// Whether gossip id `id` has been seen — the pull-gossip membership filter
    /// (Go `bloom.Contains`). The bloom stand-in is exact (a `HashSet`), so
    /// there are no false positives.
    #[must_use]
    pub fn seen(&self, id: Id) -> bool {
        self.seen.lock().contains(&id)
    }

    /// The number of distinct gossip ids seen (the bloom "count" analog used by
    /// the Go `assertTxBloomEmpty` / `assertTxBloomContains` test helpers).
    #[must_use]
    pub fn seen_count(&self) -> usize {
        self.seen.lock().len()
    }
}

/// The transport seam the gossipers push/pull through (mirrors
/// `ava-saevm-txgossip`'s `GossipTransport`; the production impl wires to
/// `ava_network::Network::gossip`, M8).
///
/// A **push** broadcasts the marshalled atomic-tx payloads of newly admitted
/// txs to connected peers; a **pull** asks peers for txs the local node has not
/// seen. The in-memory `ava-saevm-testutil` network harness implements this for
/// multi-node tests.
pub trait GossipTransport: Send + Sync + 'static {
    /// Push the given marshalled atomic-tx payloads to connected peers. Returns
    /// the number of (peer, tx) deliveries performed.
    fn push(&self, payloads: &[Vec<u8>]) -> usize;

    /// Issue a pull request: ask connected peers to send back any txs whose
    /// gossip id is not in `have`. Returns the number of txs pulled in.
    fn pull(&self, have: &[Id]) -> usize;
}

/// A [`GossipTransport`] that does nothing — the default when the VM is
/// initialized without a live network transport (M8). Push/pull loops are not
/// spawned in this case (see [`crate::vm::Vm::initialize`]); the type exists so
/// the `None` transport case has a concrete type.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGossipTransport;

impl GossipTransport for NoGossipTransport {
    fn push(&self, _payloads: &[Vec<u8>]) -> usize {
        0
    }
    fn pull(&self, _have: &[Id]) -> usize {
        0
    }
}

/// The periodic **push** gossiper (Go `gossip.PushGossiper` + `gossip.Every`).
///
/// Each tick it snapshots the [`BloomSet`], marshals every pooled tx, and pushes
/// the payloads via the [`GossipTransport`]. [`PushGossiper::run`] is the
/// testable tokio-interval kernel.
pub struct PushGossiper<T> {
    set: Arc<BloomSet>,
    marshaller: GossipMarshaller,
    transport: T,
    period: Duration,
}

impl<T: GossipTransport> PushGossiper<T> {
    /// Builds a push gossiper over `set` + `transport` at `period`.
    #[must_use]
    pub fn new(set: Arc<BloomSet>, transport: T, period: Duration) -> Self {
        Self {
            set,
            marshaller: GossipMarshaller::new(),
            transport,
            period,
        }
    }

    /// Marshals and pushes the current pool contents once. Returns the number of
    /// (peer, tx) deliveries performed. Malformed txs are skipped.
    pub fn gossip_once(&self) -> usize {
        let payloads: Vec<Vec<u8>> = self
            .set
            .snapshot()
            .iter()
            .filter_map(|tx| self.marshaller.marshal(tx).ok())
            .collect();
        if payloads.is_empty() {
            return 0;
        }
        self.transport.push(&payloads)
    }

    /// Runs `iterations` ticks of the push loop (the tokio-task kernel). Returns
    /// the total deliveries performed.
    pub async fn run(&self, iterations: usize) -> usize {
        let mut ticker = tokio::time::interval(self.period);
        let mut total = 0usize;
        for _ in 0..iterations {
            ticker.tick().await;
            total = total.saturating_add(self.gossip_once());
        }
        total
    }
}

/// The periodic **pull** gossiper (Go `gossip.PullGossiper` + `gossip.Every`).
///
/// Each tick it asks connected peers for any tx whose gossip id is not in its
/// [`BloomSet`]'s seen view. [`PullGossiper::run`] is the testable kernel.
pub struct PullGossiper<T> {
    set: Arc<BloomSet>,
    transport: T,
    period: Duration,
}

impl<T: GossipTransport> PullGossiper<T> {
    /// Builds a pull gossiper over `set` + `transport` at `period`.
    #[must_use]
    pub fn new(set: Arc<BloomSet>, transport: T, period: Duration) -> Self {
        Self {
            set,
            transport,
            period,
        }
    }

    /// Issues one pull request for the txs the local set is missing. Returns the
    /// number of txs pulled in.
    pub fn gossip_once(&self) -> usize {
        let have: Vec<Id> = self
            .set
            .snapshot()
            .iter()
            .map(Gossipable::gossip_id)
            .collect();
        self.transport.pull(&have)
    }

    /// Runs `iterations` ticks of the pull loop (the tokio-task kernel). Returns
    /// the total txs pulled in.
    pub async fn run(&self, iterations: usize) -> usize {
        let mut ticker = tokio::time::interval(self.period);
        let mut total = 0usize;
        for _ in 0..iterations {
            ticker.tick().await;
            total = total.saturating_add(self.gossip_once());
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::tx::{Credential as TxCredential, Import, Output, Unsigned};
    use ava_secp256k1fx::Credential as SecpCredential;

    use super::*;

    fn import_tx(seed: u8) -> Tx {
        let unsigned = Unsigned::Import(Import {
            network_id: 1,
            blockchain_id: Id::from([0xc0; 32]),
            source_chain: Id::from([0x0b; 32]),
            imported_ins: Vec::new(),
            outs: vec![Output {
                address: {
                    let mut a = [0u8; 20];
                    a[0] = seed;
                    a
                },
                amount: 1_000,
                asset_id: Id::from([0x0a; 32]),
            }],
        });
        Tx {
            unsigned,
            creds: vec![TxCredential::Secp256k1(SecpCredential::new(vec![
                [0u8; 65],
            ]))],
        }
    }

    #[test]
    fn gossip_marshaller_round_trips_id_stable() {
        let m = GossipMarshaller::new();
        let tx = GossipTx::new(import_tx(0x11));
        let bytes = m.marshal(&tx).expect("marshal");
        let back = m.unmarshal(&bytes).expect("unmarshal");
        assert_eq!(back, tx, "round-trips byte-identically");
        assert_eq!(back.gossip_id(), tx.gossip_id(), "gossip id is stable");
    }

    #[test]
    fn gossip_marshaller_rejects_garbage() {
        let m = GossipMarshaller::new();
        let err = m.unmarshal(&[0xff, 0xff, 0xff, 0xff]);
        assert!(matches!(err, Err(MarshalError::Decode(_))));
    }

    #[test]
    fn bloom_set_add_pools_and_records_seen() {
        let pool = Arc::new(AtomicTxpool::new(Id::from([0x0a; 32])));
        let set = BloomSet::new(Arc::clone(&pool));
        let tx = GossipTx::new(import_tx(0x22));
        let id = tx.gossip_id();

        assert!(!set.seen(id), "unseen before add");
        set.add(tx.clone()).expect("add");
        assert!(set.seen(id), "seen after add");
        assert!(pool.has(id), "pooled after add");
        assert_eq!(set.snapshot().len(), 1);

        // Re-add is idempotent (AlreadyKnown ignored) and stays seen.
        set.add(tx).expect("re-add idempotent");
        assert_eq!(pool.len(), 1, "no duplicate pooled");
        assert_eq!(set.seen_count(), 1);
    }

    #[test]
    fn bloom_set_preseeds_seen_from_existing_pool() {
        let pool = Arc::new(AtomicTxpool::new(Id::from([0x0a; 32])));
        let tx = import_tx(0x33);
        let id = tx.id();
        pool.add(tx).expect("seed pool");
        let set = BloomSet::new(pool);
        assert!(set.seen(id), "pre-existing pool tx is pre-seeded as seen");
    }

    /// Fake transport counting push/pull calls.
    #[derive(Clone, Default)]
    struct FakeTransport {
        pushes: Arc<AtomicUsize>,
        pulls: Arc<AtomicUsize>,
        push_payloads: Arc<AtomicUsize>,
    }

    impl GossipTransport for FakeTransport {
        fn push(&self, payloads: &[Vec<u8>]) -> usize {
            self.pushes.fetch_add(1, Ordering::SeqCst);
            self.push_payloads
                .fetch_add(payloads.len(), Ordering::SeqCst);
            payloads.len()
        }
        fn pull(&self, _have: &[Id]) -> usize {
            self.pulls.fetch_add(1, Ordering::SeqCst);
            0
        }
    }

    #[tokio::test(start_paused = true)]
    async fn push_gossiper_pushes_pool_each_tick() {
        let pool = Arc::new(AtomicTxpool::new(Id::from([0x0a; 32])));
        let set = Arc::new(BloomSet::new(Arc::clone(&pool)));
        set.add(GossipTx::new(import_tx(0x44))).expect("add");
        let t = FakeTransport::default();
        let g = PushGossiper::new(Arc::clone(&set), t.clone(), PUSH_GOSSIP_PERIOD);
        let delivered = g.run(3).await;
        assert_eq!(t.pushes.load(Ordering::SeqCst), 3, "one push per tick");
        assert_eq!(delivered, 3, "1 tx * 3 ticks");
    }

    #[tokio::test(start_paused = true)]
    async fn pull_gossiper_requests_each_tick() {
        let pool = Arc::new(AtomicTxpool::new(Id::from([0x0a; 32])));
        let set = Arc::new(BloomSet::new(pool));
        let t = FakeTransport::default();
        let g = PullGossiper::new(set, t.clone(), PULL_GOSSIP_PERIOD);
        g.run(4).await;
        assert_eq!(t.pulls.load(Ordering::SeqCst), 4, "one pull per tick");
    }

    #[test]
    fn no_gossip_transport_is_inert() {
        let t = NoGossipTransport;
        assert_eq!(t.push(&[vec![1, 2, 3]]), 0);
        assert_eq!(t.pull(&[Id::EMPTY]), 0);
    }
}
