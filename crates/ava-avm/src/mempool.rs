// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain (AVM) transaction mempool (specs 09 §7.1, 07 §7).
//!
//! Mirrors the P-Chain concrete mempool (`vms/txs/mempool/mempool.go` via
//! `crates/ava-platformvm/src/txs/mempool.rs`). Behavior:
//!
//! - **Insertion-ordered (FIFO)** — [`Mempool::add`] appends; [`Mempool::peek`]
//!   returns the oldest tx; [`Mempool::iterate`] / [`Mempool::snapshot`] walk in
//!   oldest → newest order. This is the order the block builder (M5.17
//!   `build_block`) packs txs in.
//! - **Deduped by tx ID** — re-adding a tx already present is a no-op error
//!   ([`Error::DuplicateTx`]); it does **not** disturb the existing tx's FIFO
//!   position.
//! - **Bounded** — capped by both a byte budget ([`MAX_MEMPOOL_SIZE`], summing
//!   each tx's serialized [`Tx::size`]) and a per-tx max ([`MAX_TX_SIZE`]); an
//!   add that would exceed the byte budget is rejected with [`Error::MempoolFull`]
//!   (drop-on-full — the pool never evicts an existing tx to make room).
//! - **Conflict-free** — a tx whose consumed UTXO inputs overlap a tx already in
//!   the pool is rejected with [`Error::ConflictsWithOtherTx`].

use std::collections::HashSet;

use ava_types::id::Id;
use ava_utils::linked::LinkedHashmap;

use crate::txs::Tx;

/// `MaxTxSize` — the maximum serialized size (bytes) a single tx may have to be
/// admitted (Go `mempool.MaxTxSize`, 64 KiB).
pub const MAX_TX_SIZE: usize = 64 * 1024;

/// `maxMempoolSize` — the byte budget shared across all txs in the pool (Go
/// `mempool.maxMempoolSize`, 64 MiB).
pub const MAX_MEMPOOL_SIZE: usize = 64 * 1024 * 1024;

/// Mempool-specific failure modes (Go `mempool` sentinels). Kept local to this
/// module because they are not AVM consensus errors; the gossip handler maps
/// them to "drop, no divergence". Do not add these to [`crate::error::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `ErrDuplicateTx` — a tx with this ID is already in the pool.
    #[error("duplicate tx")]
    DuplicateTx,

    /// `ErrTxTooLarge` — the tx's serialized size exceeds [`MAX_TX_SIZE`].
    #[error("tx too large")]
    TxTooLarge,

    /// `ErrMempoolFull` — admitting the tx would exceed [`MAX_MEMPOOL_SIZE`].
    #[error("mempool is full")]
    MempoolFull,

    /// `ErrConflictsWithOtherTx` — the tx consumes a UTXO already consumed by a
    /// tx in the pool.
    #[error("tx conflicts with other tx")]
    ConflictsWithOtherTx,
}

/// `mempool` — an insertion-ordered, byte-bounded, conflict-free tx pool
/// (specs 09 §7.1, port of Go `vms/txs/mempool/mempool.go`).
///
/// The pool owns the txs; the builder drains them in FIFO order. It is **not**
/// internally synchronized — callers wrap it in their own lock (the VM holds a
/// `Mutex<Mempool>`), mirroring how every other state structure in this crate is
/// guarded externally.
#[derive(Debug, Default)]
pub struct Mempool {
    /// Insertion-ordered `tx_id -> Tx` (Go `unissuedTxs *linked.Hashmap`).
    unissued: LinkedHashmap<Id, Tx>,
    /// `tx_id -> consumed UTXO input IDs` (Go `consumedUTXOs *setmap.SetMap`),
    /// used for the conflict check and released on removal.
    consumed: LinkedHashmap<Id, HashSet<Id>>,
    /// Remaining byte budget (Go `bytesAvailable`; starts at [`MAX_MEMPOOL_SIZE`]).
    bytes_available: usize,
}

impl Mempool {
    /// An empty pool with the full [`MAX_MEMPOOL_SIZE`] byte budget (Go `New`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            unissued: LinkedHashmap::new(),
            consumed: LinkedHashmap::new(),
            bytes_available: MAX_MEMPOOL_SIZE,
        }
    }

    /// `Add` — admit `tx`, appending it to the back (newest) of the FIFO order.
    ///
    /// Returns the corresponding [`Error`] without mutating the pool when the tx
    /// is a duplicate, too large, would overflow the byte budget (drop-on-full),
    /// or conflicts with a pooled tx.
    ///
    /// # Errors
    /// See [`Error`].
    pub fn add(&mut self, tx: Tx) -> Result<(), Error> {
        let tx_id = tx.id();

        if self.unissued.contains(&tx_id) {
            return Err(Error::DuplicateTx);
        }

        let tx_size = tx.size();
        if tx_size > MAX_TX_SIZE {
            return Err(Error::TxTooLarge);
        }
        if tx_size > self.bytes_available {
            return Err(Error::MempoolFull);
        }

        let inputs: HashSet<Id> = tx.unsigned.input_ids().into_iter().collect();
        if self.has_overlap(&inputs) {
            return Err(Error::ConflictsWithOtherTx);
        }

        // Reserve budget, record consumed UTXOs, then store the tx (back of FIFO).
        self.bytes_available = self.bytes_available.saturating_sub(tx_size);
        self.consumed.put(tx_id, inputs);
        self.unissued.put(tx_id, tx);
        Ok(())
    }

    /// `Get` — the tx with `tx_id`, if present.
    #[must_use]
    pub fn get(&self, tx_id: &Id) -> Option<&Tx> {
        self.unissued.get(tx_id)
    }

    /// Reports whether a tx with `tx_id` is in the pool.
    #[must_use]
    pub fn contains(&self, tx_id: &Id) -> bool {
        self.unissued.contains(tx_id)
    }

    /// `Remove` — drop the tx with `tx_id` (if present), releasing its byte
    /// budget and consumed-UTXO reservation. Removing an absent tx is a no-op.
    ///
    /// Returns the removed tx, if any.
    pub fn remove(&mut self, tx_id: &Id) -> Option<Tx> {
        let tx = self.unissued.delete(tx_id)?;
        self.consumed.delete(tx_id);
        self.bytes_available = self
            .bytes_available
            .saturating_add(tx.size())
            .min(MAX_MEMPOOL_SIZE);
        Some(tx)
    }

    /// `Peek` — the oldest (front) tx in FIFO order, without removing it.
    #[must_use]
    pub fn peek(&self) -> Option<&Tx> {
        self.unissued.oldest().map(|(_, tx)| tx)
    }

    /// The number of txs in the pool (Go `Len`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.unissued.len()
    }

    /// Reports whether the pool is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.unissued.is_empty()
    }

    /// `Iterate` — visit txs in FIFO order (oldest → newest) until `f` returns
    /// `false`. Read-only; does not drain.
    pub fn iterate<F: FnMut(&Tx) -> bool>(&self, mut f: F) {
        for (_, tx) in self.unissued.iter() {
            if !f(tx) {
                return;
            }
        }
    }

    /// A FIFO-ordered snapshot of the pooled txs (oldest → newest), cloned. This
    /// is what the builder packs from; it does **not** remove them (the builder
    /// removes accepted txs on accept), matching Go where the builder iterates
    /// then removes the packed subset.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Tx> {
        self.unissued.iter().map(|(_, tx)| tx.clone()).collect()
    }

    /// Reports whether `inputs` overlaps the UTXOs consumed by any pooled tx.
    fn has_overlap(&self, inputs: &HashSet<Id>) -> bool {
        if inputs.is_empty() {
            return false;
        }
        self.consumed
            .iter()
            .any(|(_, used)| used.iter().any(|id| inputs.contains(id)))
    }
}

#[cfg(test)]
mod conformance {
    use ava_codec::manager::Manager;
    use ava_secp256k1fx::{OutputOwners, TransferOutput};
    use ava_types::short_id::ShortId;

    use super::*;
    use crate::txs::codec;
    use crate::txs::components::{AvaxBaseTx, Output};
    use crate::txs::{BaseTx, UnsignedTx};

    fn owners() -> OutputOwners {
        OutputOwners::new(0, 1, vec![ShortId::from([0xab; 20])])
    }

    /// Builds an initialized [`Tx`] whose `memo` carries `tag`, giving it a
    /// distinct ID and a non-trivial serialized size, with no consumed inputs
    /// (so admissions never conflict). `network_id` keeps it well-formed.
    fn tx_with_tag(c: &Manager, tag: u32) -> Tx {
        let base = BaseTx::new(AvaxBaseTx {
            network_id: 1,
            blockchain_id: Id::EMPTY,
            outs: vec![crate::txs::components::TransferableOutput {
                asset_id: Id::EMPTY,
                out: Output::SecpTransfer(TransferOutput::new(0, owners())),
            }],
            ins: vec![],
            memo: tag.to_be_bytes().to_vec(),
        });
        let mut tx = Tx::new(UnsignedTx::Base(base));
        tx.initialize(c).expect("initialize tx");
        tx
    }

    /// TDD ENTRY POINT (M5.17). FIFO order, dedupe by tx ID, and drop-on-full at
    /// the byte capacity bound. Mirrors `crates/ava-platformvm/src/txs/mempool.rs`
    /// `mempool_dedupe_fifo`.
    #[test]
    fn mempool_dedupe_fifo() {
        let c = codec::codec().expect("codec");
        let mut m = Mempool::new();

        // --- FIFO order ---
        let a = tx_with_tag(&c, 1);
        let b = tx_with_tag(&c, 2);
        let d = tx_with_tag(&c, 3);
        assert_ne!(a.id(), b.id());
        assert_ne!(b.id(), d.id());

        m.add(a.clone()).expect("add a");
        m.add(b.clone()).expect("add b");
        m.add(d.clone()).expect("add d");
        assert_eq!(m.len(), 3);

        // peek returns the oldest; snapshot/iterate are oldest -> newest.
        assert_eq!(m.peek().expect("peek").id(), a.id());
        let order: Vec<Id> = m.snapshot().iter().map(Tx::id).collect();
        assert_eq!(order, vec![a.id(), b.id(), d.id()]);

        // --- dedupe by tx ID: re-add is rejected and does not reorder ---
        assert_eq!(m.add(a.clone()), Err(Error::DuplicateTx));
        assert_eq!(m.len(), 3);
        let order_after_dupe: Vec<Id> = m.snapshot().iter().map(Tx::id).collect();
        assert_eq!(order_after_dupe, vec![a.id(), b.id(), d.id()]);

        // Removing the oldest preserves the order of the rest (FIFO drain).
        let removed = m.remove(&a.id()).expect("remove a");
        assert_eq!(removed.id(), a.id());
        assert_eq!(m.peek().expect("peek").id(), b.id());
        assert_eq!(
            m.snapshot().iter().map(Tx::id).collect::<Vec<_>>(),
            vec![b.id(), d.id()],
        );
        // The removed tx may be re-added (its slot is free again).
        m.add(a.clone()).expect("re-add a");
        assert_eq!(
            m.snapshot().iter().map(Tx::id).collect::<Vec<_>>(),
            vec![b.id(), d.id(), a.id()],
        );

        // --- drop-on-full at the byte capacity bound ---
        let tx_size = a.size();
        assert!(tx_size > 0);
        // A pool whose budget admits exactly two such txs.
        let mut full = Mempool {
            unissued: LinkedHashmap::new(),
            consumed: LinkedHashmap::new(),
            bytes_available: tx_size * 2 + tx_size / 2,
        };
        let t0 = tx_with_tag(&c, 100);
        let t1 = tx_with_tag(&c, 101);
        let t2 = tx_with_tag(&c, 102);
        full.add(t0.clone()).expect("add t0");
        full.add(t1.clone()).expect("add t1");
        // The third does not fit: dropped, not evicting the existing two.
        assert_eq!(full.add(t2.clone()), Err(Error::MempoolFull));
        assert_eq!(full.len(), 2);
        assert_eq!(
            full.snapshot().iter().map(Tx::id).collect::<Vec<_>>(),
            vec![t0.id(), t1.id()],
        );

        // --- oversized tx rejected ---
        let mut oversized = tx_with_tag(&c, 200);
        // Force a size over MAX_TX_SIZE without re-serializing.
        oversized.bytes = bytes::Bytes::from(vec![0u8; MAX_TX_SIZE + 1]);
        assert_eq!(m.add(oversized), Err(Error::TxTooLarge));
    }
}

#[cfg(test)]
mod prop {
    use ava_secp256k1fx::{OutputOwners, TransferOutput};
    use ava_types::short_id::ShortId;
    use proptest::prelude::*;

    use super::*;
    use crate::txs::codec;
    use crate::txs::components::{AvaxBaseTx, Output};
    use crate::txs::{BaseTx, UnsignedTx};

    fn owners() -> OutputOwners {
        OutputOwners::new(0, 1, vec![ShortId::from([0xab; 20])])
    }

    fn tx_with_tag(tag: u32) -> Tx {
        let c = codec::codec().expect("codec");
        let base = BaseTx::new(AvaxBaseTx {
            network_id: 1,
            blockchain_id: Id::EMPTY,
            outs: vec![crate::txs::components::TransferableOutput {
                asset_id: Id::EMPTY,
                out: Output::SecpTransfer(TransferOutput::new(0, owners())),
            }],
            ins: vec![],
            memo: tag.to_be_bytes().to_vec(),
        });
        let mut tx = Tx::new(UnsignedTx::Base(base));
        tx.initialize(&c).expect("initialize tx");
        tx
    }

    proptest! {
        /// add/remove idempotence: adding a set of distinct txs then removing
        /// exactly those (in any order) returns the pool to empty with the full
        /// byte budget restored and no tx lost or duplicated along the way.
        #[test]
        fn mempool_no_loss(tags in proptest::collection::vec(0u32..256, 0..40)) {
            let mut m = Mempool::new();

            // Distinct txs by tag (dedupe collapses repeats).
            let mut added: Vec<Id> = Vec::new();
            for tag in &tags {
                let tx = tx_with_tag(*tag);
                let id = tx.id();
                match m.add(tx) {
                    Ok(()) => {
                        prop_assert!(!added.contains(&id));
                        added.push(id);
                    }
                    Err(Error::DuplicateTx) => {
                        prop_assert!(added.contains(&id));
                    }
                    Err(e) => prop_assert!(false, "unexpected add error: {e:?}"),
                }
            }

            // Every added tx is present and retrievable; nothing extra exists.
            prop_assert_eq!(m.len(), added.len());
            for id in &added {
                prop_assert!(m.contains(id));
                prop_assert_eq!(m.get(id).map(Tx::id), Some(*id));
            }

            // Removing each added tx once returns to empty, no loss/dup.
            let mut order = added.clone();
            order.reverse(); // remove in a different order than insertion
            for id in &order {
                let removed = m.remove(id);
                prop_assert_eq!(removed.map(|t| t.id()), Some(*id));
                // Removing again is a harmless no-op.
                prop_assert!(m.remove(id).is_none());
            }
            prop_assert!(m.is_empty());
            prop_assert_eq!(m.bytes_available, MAX_MEMPOOL_SIZE);
        }
    }
}
