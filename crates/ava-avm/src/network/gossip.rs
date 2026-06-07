// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain tx gossip handler + marshaller seam (specs 09 §8;
//! `vms/avm/network/{gossip,atomic}.go`).
//!
//! ## Scope (M5.18, deferred-transport seam)
//!
//! `ava-network` does **not** yet expose a generic p2p gossip framework (no
//! `Gossipable` trait, no push/pull `Gossiper`, no Bloom `Set` — only IP gossip
//! exists). This module implements the gossip **handler logic**: the part that
//! decides, for an inbound gossiped tx, whether to admit it to the [`Mempool`] —
//! behind a minimal local [`Gossipable`] trait and a [`TxMarshaller`] that the
//! deferred transport can call.
//!
//! The actual p2p transport (AppGossip framing, Bloom-filter push/pull
//! reconciliation, peer fan-out) is the **deferred 05/M2 follow-up**. The M5 exit
//! gate does not exercise live gossip, so this handler + marshaller seam is
//! sufficient.
//!
//! Design mirrors `crates/ava-platformvm/src/network.rs` (M4.26).

use ava_codec::error::CodecError;
use ava_types::id::Id;

use crate::mempool::{Error as MempoolError, Mempool};
use crate::network::tx_verifier::TxVerifier;
use crate::txs::Tx;
use crate::txs::codec::Codec;

/// A value that can be identified for gossip deduplication
/// (`vms/avm/network/gossip.go` `Gossipable`).
///
/// The deferred generic push/pull gossip transport will accept `T: Gossipable`;
/// this minimal local seam keeps `ava-avm` independent of the not-yet-written
/// framework (05/M2 follow-up).
pub trait Gossipable {
    /// The gossip deduplication id for this value (`gossip_id = tx_id`).
    fn gossip_id(&self) -> Id;
}

impl Gossipable for Tx {
    fn gossip_id(&self) -> Id {
        self.id()
    }
}

/// `TxMarshaller` — the marshaller seam `ava-avm` supplies to the generic
/// gossip framework (`vms/avm/network/gossip.go` `txParser`).
///
/// `marshal` returns the cached signed bytes; `unmarshal` delegates to
/// [`Tx::parse`] with the process-wide [`Codec`].
///
/// ## Deferred
///
/// When the generic push/pull gossip framework (05/M2) is wired, this struct
/// becomes its `Marshaller` implementation for the `Tx` type.
#[derive(Debug, Default, Clone, Copy)]
pub struct TxMarshaller;

impl TxMarshaller {
    /// Builds a new `TxMarshaller` (stateless).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Serializes `tx` for the wire — returns a copy of the cached signed bytes.
    #[must_use]
    pub fn marshal(&self, tx: &Tx) -> Vec<u8> {
        tx.bytes().to_vec()
    }

    /// Deserializes a `Tx` from `bytes` via [`Tx::parse`] with the shared codec.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if the bytes are malformed, unknown version, or
    /// contain trailing bytes.
    pub fn unmarshal(&self, bytes: &[u8]) -> Result<Tx, CodecError> {
        Tx::parse(Codec(), bytes)
    }
}

/// Why the handler did not admit a gossiped tx. All outcomes are
/// non-divergent: the mempool is left in the same state it would reach from
/// any other ordering of the same gossip stream.
///
/// Mirrors `crates/ava-platformvm/src/network.rs` `DropReason`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropReason {
    /// The tx is already in the mempool (deduped).
    Duplicate,
    /// The tx failed [`TxVerifier::verify_tx`] (carries the reason string).
    Verification(String),
    /// The mempool rejected the (verified) tx — full, too large, or conflicting.
    Mempool(MempoolError),
}

/// The result of handling one inbound gossiped tx.
///
/// Mirrors `crates/ava-platformvm/src/network.rs` `HandleOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleOutcome {
    /// The tx was newly admitted to the mempool.
    Added,
    /// The tx was dropped for the given reason (no mempool mutation).
    Dropped(DropReason),
}

/// The gossip handler: owns the admission policy that maps an inbound tx onto
/// the [`Mempool`] (Go `gossipMempool.Add`; `vms/avm/network/gossip.go`).
///
/// It does **not** own the mempool or verifier — both are borrowed per call, so
/// the VM can hold the mempool behind its own `Mutex` and swap verifiers as the
/// chain transitions. The deferred p2p transport calls
/// [`Self::handle_gossiped_tx`] for each tx it receives off the wire.
///
/// Design mirrors `crates/ava-platformvm/src/network.rs` `TxGossipHandler`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TxGossipHandler;

impl TxGossipHandler {
    /// A new handler (stateless).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Handle one inbound gossiped `tx` (Go `gossipMempool.Add`):
    ///
    /// 1. **dedupe** — if the tx is already pooled, drop it
    ///    ([`DropReason::Duplicate`]);
    /// 2. **verify shape** — run `verifier`; on failure drop it
    ///    ([`DropReason::Verification`]) without touching the pool;
    /// 3. **admit** — add to the mempool; a mempool rejection (full / too large /
    ///    conflict) drops it ([`DropReason::Mempool`]).
    ///
    /// Every path is divergence-free: a drop leaves the mempool untouched, and
    /// an admit is idempotent across re-gossip (the next time the same tx
    /// arrives it is deduped). Returns [`HandleOutcome`] so the transport can
    /// meter/ack.
    pub fn handle_gossiped_tx<V: TxVerifier>(
        &self,
        mempool: &mut Mempool,
        verifier: &V,
        tx: Tx,
    ) -> HandleOutcome {
        let tx_id = tx.id();

        // 1) Dedupe against what is already pooled.
        if mempool.contains(&tx_id) {
            return HandleOutcome::Dropped(DropReason::Duplicate);
        }

        // 2) Shape/semantic gate. A failure drops without mutating the pool.
        if let Err(reason) = verifier.verify_tx(&tx) {
            return HandleOutcome::Dropped(DropReason::Verification(reason));
        }

        // 3) Admit. The mempool enforces dedupe/full/conflict bounds; any
        //    rejection is a non-divergent drop.
        match mempool.add(tx) {
            Ok(()) => HandleOutcome::Added,
            Err(e) => HandleOutcome::Dropped(DropReason::Mempool(e)),
        }
    }

    /// Handle a batch of inbound gossiped txs in order, returning each tx's
    /// outcome. Convenience over [`Self::handle_gossiped_tx`] for a transport
    /// that delivers an `AppGossip` payload of several txs.
    pub fn handle_gossiped_txs<V: TxVerifier>(
        &self,
        mempool: &mut Mempool,
        verifier: &V,
        txs: impl IntoIterator<Item = Tx>,
    ) -> Vec<HandleOutcome> {
        txs.into_iter()
            .map(|tx| self.handle_gossiped_tx(mempool, verifier, tx))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use ava_codec::manager::Manager;

    use super::*;
    use crate::network::tx_verifier::SyntacticTxVerifier;
    use crate::txs::codec;
    use crate::txs::components::AvaxBaseTx;
    use crate::txs::{BaseTx, UnsignedTx};

    fn tx_with_tag(c: &Manager, tag: u32) -> Tx {
        let base = BaseTx::new(AvaxBaseTx {
            network_id: 1,
            blockchain_id: Id::EMPTY,
            outs: vec![],
            ins: vec![],
            memo: tag.to_be_bytes().to_vec(),
        });
        let mut tx = Tx::new(UnsignedTx::Base(base));
        tx.initialize(c).expect("initialize");
        tx
    }

    /// A verifier that rejects everything, to exercise the verification-drop path.
    struct RejectAll;
    impl TxVerifier for RejectAll {
        fn verify_tx(&self, _tx: &Tx) -> Result<(), String> {
            Err("nope".to_string())
        }
    }

    #[test]
    fn gossipable_id_is_tx_id() {
        let c = codec::codec().expect("codec");
        let tx = tx_with_tag(&c, 99);
        assert_eq!(tx.gossip_id(), tx.id());
        assert_ne!(tx.gossip_id(), Id::EMPTY);
    }

    #[test]
    fn marshaller_roundtrip() {
        let c = codec::codec().expect("codec");
        let tx = tx_with_tag(&c, 1);
        let m = TxMarshaller::new();
        let bytes = m.marshal(&tx);
        let tx2 = m.unmarshal(&bytes).expect("unmarshal");
        assert_eq!(tx2.id(), tx.id());
    }

    #[test]
    fn admits_then_dedupes_inbound_gossip() {
        let c = codec::codec().expect("codec");
        let h = TxGossipHandler::new();
        let v = SyntacticTxVerifier;
        let mut m = Mempool::new();

        let tx = tx_with_tag(&c, 1);
        assert_eq!(
            h.handle_gossiped_tx(&mut m, &v, tx.clone()),
            HandleOutcome::Added
        );
        assert_eq!(m.len(), 1);

        // Re-gossip of the same tx is deduped, mempool unchanged (no divergence).
        assert_eq!(
            h.handle_gossiped_tx(&mut m, &v, tx.clone()),
            HandleOutcome::Dropped(DropReason::Duplicate)
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn order_independent_convergence() {
        let c = codec::codec().expect("codec");
        let h = TxGossipHandler::new();
        let v = SyntacticTxVerifier;
        let txs = [tx_with_tag(&c, 1), tx_with_tag(&c, 2), tx_with_tag(&c, 3)];

        let mut a = Mempool::new();
        h.handle_gossiped_txs(&mut a, &v, txs.iter().cloned());

        let mut b = Mempool::new();
        h.handle_gossiped_txs(&mut b, &v, txs.iter().rev().cloned());

        let mut ids_a: Vec<Id> = a.snapshot().iter().map(Tx::id).collect();
        let mut ids_b: Vec<Id> = b.snapshot().iter().map(Tx::id).collect();
        ids_a.sort();
        ids_b.sort();
        assert_eq!(ids_a, ids_b);
        assert_eq!(ids_a.len(), 3);
    }

    #[test]
    fn verification_failure_drops_without_mutation() {
        let c = codec::codec().expect("codec");
        let h = TxGossipHandler::new();
        let mut m = Mempool::new();
        let tx = tx_with_tag(&c, 7);
        let outcome = h.handle_gossiped_tx(&mut m, &RejectAll, tx);
        assert_eq!(
            outcome,
            HandleOutcome::Dropped(DropReason::Verification("nope".to_string()))
        );
        assert!(m.is_empty());
    }

    #[test]
    fn syntactic_verifier_rejects_uninitialized() {
        let v = SyntacticTxVerifier;
        let uninit = Tx::new(UnsignedTx::default());
        assert!(v.verify_tx(&uninit).is_err());
    }
}
