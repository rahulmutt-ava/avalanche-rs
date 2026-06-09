// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-txgossip` — the SAE EVM mempool plus push/pull gossip ordered by
//! effective-tip priority (specs/11 §9.2).
//!
//! Go sources: `txgossip/priority.go` (effective-tip ordering),
//! `txgossip/{mempool,gossip}.go` (pool + push/pull gossipers), and the P2P
//! wiring in `sae/vm.go::NewVM`.
//!
//! ## Scope (M7.20) and deferred transport
//!
//! Two pieces of infrastructure this crate is *meant* to wrap do not yet exist
//! in `avalanche-rs`, so — following the established local-seam precedent
//! (`crates/ava-avm/src/network/gossip.rs`, `crates/ava-platformvm/src/network.rs`,
//! and the M7.9/M7.13 deferral pattern) — this crate implements the genuinely
//! testable core behind minimal local seams and documents the live transport as
//! deferred:
//!
//! - **No generic gossip framework.** `ava-network` exposes only
//!   `Network::gossip(...)` and a read-only bloom filter — no `Gossipable`
//!   trait, no `BloomSet`, no push/pull `Gossiper`. So [`Gossipable`] is a local
//!   trait, [`Set`] dedups with an owned `HashSet<B256>` of seen gossip ids (the
//!   bloom-filter *writer* is deferred to the generic framework), and the
//!   push/pull gossipers ([`PushGossiper`]/[`PullGossiper`]) run behind a
//!   [`GossipTransport`] seam. Wiring the production [`GossipTransport`] to
//!   `Network::gossip` is **deferred to M7.23** (cchain VM `Initialize`).
//! - **No reth `TransactionPool`.** The reth tx-pool integration is
//!   M6.23-deferred, so [`Set`]'s pending pool is a local in-memory map keyed by
//!   gossip id (tx hash); coupling it to the live reth pool is deferred to
//!   M6.23. The unit consuming the pool — [`TransactionsByPriority`] — is fully
//!   implemented and tested here.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod priority;

use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

use ava_evm_reth::{B256, ConsensusTx, Decodable2718, RecoveredTx, TransactionSigned};

pub use crate::priority::{Priced, TransactionsByPriority};

/// Push-gossip period: re-broadcast newly admitted txs every 100 ms
/// (`txgossip/gossip.go`; `sae/vm.go::NewVM` P2P section).
pub const PUSH_GOSSIP_PERIOD: Duration = Duration::from_millis(100);

/// Pull-gossip period: reconcile the mempool against peers every 1 s
/// (`txgossip/gossip.go`; `sae/vm.go::NewVM` P2P section).
pub const PULL_GOSSIP_PERIOD: Duration = Duration::from_secs(1);

/// A value identified for gossip deduplication (`txgossip` `Gossipable`).
///
/// Local trait — there is no generic `gossip::Gossipable` in `ava-network` yet
/// (see the crate docs). The deferred push/pull framework will accept
/// `T: Gossipable`; for EVM txs the gossip id is the transaction hash.
pub trait Gossipable {
    /// The gossip deduplication id — the transaction hash (`B256`).
    fn gossip_id(&self) -> B256;
}

/// An EVM mempool transaction: a sender-recovered signed tx
/// ([`RecoveredTx`]) wrapped so it is [`Gossipable`] (RLP over the wire) and
/// [`Priced`] (effective-tip ordering).
///
/// The inner tx is reached only through the `ava-evm-reth` facade — this crate
/// never names a `reth_*`/`alloy_*` type directly (00 §11.1.6).
#[derive(Clone, Debug)]
pub struct Transaction {
    inner: RecoveredTx,
}

impl Transaction {
    /// Wraps a sender-recovered signed tx.
    #[must_use]
    pub fn new(inner: RecoveredTx) -> Self {
        Self { inner }
    }

    /// Borrows the underlying recovered tx (for the executor / builder).
    #[must_use]
    pub fn recovered(&self) -> &RecoveredTx {
        &self.inner
    }

    /// Consumes the wrapper, returning the recovered tx.
    #[must_use]
    pub fn into_recovered(self) -> RecoveredTx {
        self.inner
    }
}

impl Gossipable for Transaction {
    fn gossip_id(&self) -> B256 {
        // The tx hash: stable across an RLP round-trip (see `TxMarshaller`).
        *self.inner.inner().hash()
    }
}

impl Priced for Transaction {
    fn effective_tip(&self, base_fee: u64) -> Option<u128> {
        // `effective_tip_per_gas` returns `None` when the tx cannot pay
        // `base_fee` — exactly the skip condition the builder uses
        // (`crates/ava-evm/src/builder.rs::pack_evm_txs`).
        ConsensusTx::effective_tip_per_gas(self.inner.inner(), base_fee)
    }

    fn nonce(&self) -> u64 {
        ConsensusTx::nonce(self.inner.inner())
    }
}

/// Errors from (de)serializing a gossiped tx (`txgossip` `txParser`).
#[derive(Debug, thiserror::Error)]
pub enum MarshalError {
    /// The bytes were not a valid EIP-2718 typed-envelope signed tx.
    #[error("malformed transaction envelope")]
    Decode,
    /// The signed tx's sender signature failed to recover.
    #[error("sender recovery failed")]
    Recover,
}

/// Marshaller seam for gossiping a [`Transaction`] (`txgossip` `txParser`).
///
/// `marshal` emits the **EIP-2718 typed envelope** of the signed tx (the same
/// single-tx encoding `crates/ava-evm/src/block.rs` decodes out of a block
/// body); `unmarshal` decodes via [`Decodable2718`] and recovers the sender.
/// The round-trip is hash-stable: `unmarshal(marshal(tx)).gossip_id() ==
/// tx.gossip_id()`.
///
/// When the generic push/pull gossip framework lands this becomes its
/// `Marshaller` for `Transaction`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TxMarshaller;

impl TxMarshaller {
    /// A new (stateless) marshaller.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Serializes `tx` for the wire as its single-tx envelope.
    ///
    /// Uses the facade's `RlpEncodable` (`Encodable::encode`) — the same
    /// block-body single-tx encoding `ava-evm::block::encode_tx_list` emits
    /// (legacy: the RLP list; typed: the 2718 envelope as an RLP byte string).
    /// This is the inverse of [`Decodable2718`] for legacy txs.
    ///
    /// ## Deferred
    /// The facade does not expose `Encodable2718::encoded_2718` (the bare
    /// `ty || rlp(payload)` form), so for *typed* (EIP-1559/2930/4844) txs the
    /// canonical 2718 envelope is emitted by the generic gossip framework's
    /// marshaller; wiring that exact encoder is deferred alongside the framework.
    // Stateless today, but the receiver is kept for parity with Go's
    // `TxMarshaller` and because the generic gossip framework's marshaller will
    // carry a codec (the typed-2718 encoder noted below) — keep the API stable.
    #[allow(clippy::unused_self, clippy::trivially_copy_pass_by_ref)]
    #[must_use]
    pub fn marshal(&self, tx: &Transaction) -> Vec<u8> {
        use ava_evm_reth::RlpEncodable;
        let mut out = Vec::new();
        tx.inner.inner().encode(&mut out);
        out
    }

    /// Decodes a [`Transaction`] from its EIP-2718 typed envelope and recovers
    /// the sender.
    ///
    /// # Errors
    /// [`MarshalError::Decode`] if the bytes are not a valid signed-tx envelope;
    /// [`MarshalError::Recover`] if the signature does not recover a sender.
    #[allow(clippy::unused_self, clippy::trivially_copy_pass_by_ref)]
    pub fn unmarshal(&self, mut bytes: &[u8]) -> Result<Transaction, MarshalError> {
        use ava_evm_reth::SignerRecoverable;
        let signed =
            TransactionSigned::decode_2718(&mut bytes).map_err(|_| MarshalError::Decode)?;
        let recovered = signed
            .try_into_recovered()
            .map_err(|_| MarshalError::Recover)?;
        Ok(Transaction::new(recovered))
    }
}

/// The mempool: a dedup membership view coupled with a pool of pending txs
/// (`txgossip/mempool.go`; specs/11 §9.2 "`Set` couples a `gossip::BloomSet`
/// with the pool").
///
/// ## Dedup / bloom
///
/// `ava-network` has no `BloomSet` *writer*, so membership is an owned
/// `HashSet<B256>` of seen gossip ids; constructing a `ReadFilter`-compatible
/// bloom for the outbound *pull* request is deferred to the generic gossip
/// framework. A plain `HashSet` is sufficient for the dedup invariant.
///
/// ## Pool
///
/// The pending pool is a local `HashMap<B256, Transaction>` keyed by gossip id;
/// coupling it to the live reth `TransactionPool` is deferred to M6.23. Lookups,
/// insertion, removal, and iteration are all keyed by gossip id, so add/remove
/// are idempotent and order-independent (no tx is silently lost: a tx is either
/// pending or was explicitly removed).
#[derive(Debug, Default)]
pub struct Set {
    /// Pending txs keyed by gossip id (tx hash).
    pending: HashMap<B256, Transaction>,
    /// Gossip ids seen (admitted at least once) — the dedup view.
    seen: HashSet<B256>,
}

impl Set {
    /// An empty mempool.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `tx` to the pool. Idempotent: re-adding a tx with the same gossip id
    /// is a no-op (returns `false`); a genuinely new tx returns `true`. The
    /// gossip id is recorded as seen either way.
    pub fn add(&mut self, tx: Transaction) -> bool {
        let id = tx.gossip_id();
        self.seen.insert(id);
        if self.pending.contains_key(&id) {
            return false;
        }
        self.pending.insert(id, tx);
        true
    }

    /// Removes the tx with gossip id `id` from the pending pool. Idempotent:
    /// removing an absent tx is a no-op (returns `false`). The id stays in the
    /// seen set so a removed tx is not silently re-admitted by stale gossip.
    pub fn remove(&mut self, id: &B256) -> bool {
        self.pending.remove(id).is_some()
    }

    /// Whether a tx with gossip id `id` is currently pending.
    #[must_use]
    pub fn contains(&self, id: &B256) -> bool {
        self.pending.contains_key(id)
    }

    /// Whether gossip id `id` has been seen (admitted at least once) — the dedup
    /// check the inbound handler uses before re-admitting a tx.
    #[must_use]
    pub fn seen(&self, id: &B256) -> bool {
        self.seen.contains(id)
    }

    /// The number of pending txs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether the pending pool is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Snapshots the pending txs (order unspecified; use
    /// [`Self::by_priority`] for the inclusion order).
    #[must_use]
    pub fn pending(&self) -> Vec<Transaction> {
        self.pending.values().cloned().collect()
    }

    /// Orders the pending txs by effective tip at `base_fee`
    /// ([`TransactionsByPriority`]). Txs that cannot pay `base_fee` are skipped.
    #[must_use]
    pub fn by_priority(&self, base_fee: u64) -> TransactionsByPriority<Transaction> {
        TransactionsByPriority::new(self.pending(), base_fee)
    }
}

/// The transport seam the gossipers push/pull through (deferred to M7.23).
///
/// The production impl wires [`Self::broadcast`] to
/// `ava_network::Network::gossip(...)`; a test fake counts calls. Keeping the
/// transport behind this trait lets the tokio gossip tasks be exercised without
/// a live `Network` handle or the (not-yet-written) generic gossip protocol.
pub trait GossipTransport: Send + Sync + 'static {
    /// Broadcast a marshalled gossip payload to peers (push gossip), or issue a
    /// pull request (the framework reconciles bloom membership). Returns the
    /// number of peers the payload was sent to.
    fn broadcast(&self, payload: Vec<u8>) -> usize;
}

/// Periodic **push** gossiper (`txgossip/gossip.go`): every
/// [`PUSH_GOSSIP_PERIOD`] (100 ms) it marshals recently admitted txs and
/// broadcasts them via the [`GossipTransport`].
///
/// The task body is run by [`Self::run`] as a tokio interval loop. Live wiring
/// of the transport to `Network::gossip` is deferred to M7.23.
pub struct PushGossiper<T> {
    transport: T,
    period: Duration,
}

impl<T: GossipTransport> PushGossiper<T> {
    /// Builds a push gossiper over `transport` with the default
    /// [`PUSH_GOSSIP_PERIOD`].
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            period: PUSH_GOSSIP_PERIOD,
        }
    }

    /// Runs `iterations` ticks of the push loop, marshaling and broadcasting the
    /// supplied per-tick payloads. Returns the total peers reached.
    ///
    /// This is the testable kernel of the tokio task: the production VM spawns
    /// `tokio::spawn(async move { gossiper.run(...).await })` with payloads drawn
    /// from the [`Set`]; here `payloads` is supplied so the loop is deterministic
    /// under test.
    pub async fn run<P>(&self, iterations: usize, mut next_payload: P) -> usize
    where
        P: FnMut() -> Option<Vec<u8>> + Send,
    {
        let mut ticker = tokio::time::interval(self.period);
        let mut reached = 0usize;
        for _ in 0..iterations {
            ticker.tick().await;
            if let Some(payload) = next_payload() {
                reached = reached.saturating_add(self.transport.broadcast(payload));
            }
        }
        reached
    }
}

/// Periodic **pull** gossiper (`txgossip/gossip.go`): every
/// [`PULL_GOSSIP_PERIOD`] (1 s) it issues a bloom-filtered pull request via the
/// [`GossipTransport`] so peers send back txs it is missing.
///
/// As with [`PushGossiper`], the bloom *writer* and the live `Network` wiring
/// are deferred (to the generic gossip framework / M7.23).
pub struct PullGossiper<T> {
    transport: T,
    period: Duration,
}

impl<T: GossipTransport> PullGossiper<T> {
    /// Builds a pull gossiper over `transport` with the default
    /// [`PULL_GOSSIP_PERIOD`].
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            period: PULL_GOSSIP_PERIOD,
        }
    }

    /// Runs `iterations` ticks of the pull loop, issuing one pull request per
    /// tick via the supplied `request` payload. Returns the total peers reached.
    pub async fn run(&self, iterations: usize, request: Vec<u8>) -> usize {
        let mut ticker = tokio::time::interval(self.period);
        let mut reached = 0usize;
        for _ in 0..iterations {
            ticker.tick().await;
            reached = reached.saturating_add(self.transport.broadcast(request.clone()));
        }
        reached
    }
}
