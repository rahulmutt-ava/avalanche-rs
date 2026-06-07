// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain tx gossip handler (`vms/platformvm/network/{network,gossip}.go`,
//! specs 08 §1; p2p `Gossip` is specs 05).
//!
//! ## Scope (M4.26, read-only sync)
//!
//! `ava-network` does **not** yet expose a generic p2p `Gossip` framework (only
//! IP gossip exists). So this module implements the gossip **handler logic** —
//! the part that decides, for an inbound gossiped tx, whether to admit it to the
//! [`Mempool`] — behind a minimal local [`TxVerifier`] seam (mirroring the local
//! seams M4.18/M4.19 introduced for not-yet-existing infra). The actual p2p
//! transport (`AppGossip` framing, the bloom filter / push-pull pull-gossip set
//! reconciliation, peer fan-out) is the **deferred transport seam**: a follow-up
//! task wires a real `ava-network` gossip protocol to call
//! [`TxGossipHandler::handle_gossiped_tx`].
//!
//! Read-only sync issues no txs, so the handler never *originates* gossip; it
//! only needs to **accept and dedupe inbound gossip without divergence**: a
//! duplicate, conflicting, full, or shape-invalid tx is dropped (returned as a
//! [`HandleOutcome`]/`Err`) and the mempool is left unchanged, so two nodes that
//! receive the same gossip stream in any order converge on the same admitted
//! set.

use ava_types::id::Id;

use crate::txs::Tx;
use crate::txs::mempool::{Error as MempoolError, Mempool};

/// `tx_verifier.VerifyTx` — the shape/semantic gate a gossiped tx must pass
/// before it is admitted to the mempool (Go `network.txVerifier`).
///
/// A minimal local seam: the real verifier runs the executor's syntactic +
/// semantic checks against the preferred state. During read-only sync the VM
/// supplies [`SyntacticVerifier`], which only enforces the cheap, state-free
/// shape checks (enough to keep malformed gossip out of the pool without a state
/// view). The trait keeps the handler decoupled from the executor wiring, which
/// is the deferred piece.
pub trait TxVerifier {
    /// Returns `Ok(())` if `tx` is acceptable, or a human-readable reason it was
    /// rejected. A rejection causes the handler to drop the tx (no divergence).
    ///
    /// # Errors
    /// Returns a descriptive reason string when the tx fails verification.
    fn verify_tx(&self, tx: &Tx) -> Result<(), String>;
}

/// The state-free verifier used during read-only sync: it runs only the tx's own
/// syntactic checks (no UTXO/flow/validator-set lookups), which is sufficient to
/// reject malformed gossip without a state view.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyntacticVerifier;

impl TxVerifier for SyntacticVerifier {
    fn verify_tx(&self, tx: &Tx) -> Result<(), String> {
        // A gossiped tx must be initialized (have a non-empty ID / cached bytes);
        // an uninitialized envelope is malformed gossip.
        if tx.id() == Id::EMPTY || tx.bytes().is_empty() {
            return Err("tx is not initialized".to_string());
        }
        Ok(())
    }
}

/// Why the handler did not admit a gossiped tx. All outcomes are
/// non-divergent: the mempool is left in the same state it would reach from any
/// other ordering of the same gossip stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropReason {
    /// The tx is already in the mempool (deduped).
    Duplicate,
    /// The tx failed [`TxVerifier::verify_tx`] (carries the reason).
    Verification(String),
    /// The mempool rejected the (verified) tx — full, too large, or conflicting.
    Mempool(MempoolError),
}

/// The result of handling one inbound gossiped tx.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleOutcome {
    /// The tx was newly admitted to the mempool.
    Added,
    /// The tx was dropped for the given reason (no mempool mutation).
    Dropped(DropReason),
}

/// The gossip handler: owns the admission policy that maps an inbound tx onto the
/// [`Mempool`] (Go `gossipMempool.Add`).
///
/// It does **not** own the mempool or the verifier — both are borrowed per call,
/// so the VM can hold the mempool behind its own `Mutex` and swap verifiers as
/// the chain transitions out of read-only sync. The deferred p2p transport calls
/// [`Self::handle_gossiped_tx`] (or [`Self::handle_gossiped_txs`]) for each tx it
/// receives off the wire.
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
    /// 1. **dedupe** — if the tx is already pooled, drop it ([`DropReason::Duplicate`]);
    /// 2. **verify shape** — run `verifier`; on failure drop it
    ///    ([`DropReason::Verification`]) without touching the pool;
    /// 3. **admit** — add to the mempool; a mempool rejection (full / too large /
    ///    conflict) drops it ([`DropReason::Mempool`]).
    ///
    /// Every path is divergence-free: a drop leaves the mempool untouched, and an
    /// admit is idempotent across re-gossip (the next time the same tx arrives it
    /// is deduped). Returns [`HandleOutcome`] so the transport can meter/ack.
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
    use crate::txs::codec;
    use crate::txs::components::BaseTx as AvaxBaseTx;
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
    fn admits_then_dedupes_inbound_gossip() {
        let c = codec::codec().expect("codec");
        let h = TxGossipHandler::new();
        let v = SyntacticVerifier;
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
        // Two nodes receiving the same three txs in opposite orders admit the
        // same set (FIFO order differs, admitted membership does not).
        let c = codec::codec().expect("codec");
        let h = TxGossipHandler::new();
        let v = SyntacticVerifier;
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
        let v = SyntacticVerifier;
        let uninit = Tx::new(UnsignedTx::default());
        assert!(v.verify_tx(&uninit).is_err());
    }
}
