// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! In-memory pool of cross-chain (atomic) transactions awaiting inclusion in a
//! block (specs/11 §8).
//!
//! Port of `vms/saevm/cchain/txpool`. The [`AtomicTxpool`] is **separate** from
//! the EVM/`txgossip` mempool: atomic Import/Export txs are conflict-checked and
//! ordered here, while EVM txs live in reth's pool. The SAE block builder's
//! `WaitForEvent` must wake when a tx arrives in **either** pool — modelled by
//! [`WaitSource::wait_for_event`], which selects across the two pools' notifies
//! (Go selects on both the atomic pool's `cond` and the EVM pool's head event).
//!
//! Conflict detection and fee-ordered eviction (Go `Txpool.Add` against the
//! last-executed state) are reduced here to id-keyed admission + input-id
//! conflict tracking; the full state-verified admission path is wired by the
//! M7.23 VM lifecycle (which owns the `LastExecutedState` backend). See the
//! `// TODO(M7.23)` markers.

use std::collections::{BTreeMap, BTreeSet};

use ava_types::id::Id;
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::tx::Tx;

/// Errors returned by the atomic [`AtomicTxpool`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The tx is already in the pool.
    #[error("transaction already in pool")]
    AlreadyKnown,
}

/// Which pool woke a [`WaitSource::wait_for_event`] waiter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitSource {
    /// A tx arrived in the atomic (cross-chain) pool.
    Atomic,
    /// A tx arrived in the EVM pool.
    Evm,
}

/// A source the block builder can await for a pending tx.
///
/// Both the [`AtomicTxpool`] and the [`EvmPoolStub`] implement it; the SAE
/// builder waits across both (the "select across two sources" seam).
pub trait WaitPool: Send + Sync {
    /// Returns the notify signalled when a tx is added to this pool.
    fn notify(&self) -> &Notify;
    /// Reports whether the pool currently holds at least one tx.
    fn has_pending(&self) -> bool;
}

impl WaitSource {
    /// `WaitForEvent` — block until a tx is pending in **either** `atomic` or
    /// `evm`, returning which pool woke the waiter.
    ///
    /// If both already have pending txs, [`WaitSource::Atomic`] is reported
    /// (a deterministic tie-break; the caller drains both regardless).
    pub async fn wait_for_event<A: WaitPool + ?Sized, E: WaitPool + ?Sized>(
        atomic: &A,
        evm: &E,
    ) -> WaitSource {
        loop {
            // Arm both notifies before checking state so a tx added between the
            // check and the await is not missed (tokio `Notify` is edge- but
            // permit-buffered for a single waiter; re-checking after arming
            // closes the race).
            let atomic_n = atomic.notify().notified();
            let evm_n = evm.notify().notified();

            if atomic.has_pending() {
                return WaitSource::Atomic;
            }
            if evm.has_pending() {
                return WaitSource::Evm;
            }

            tokio::select! {
                () = atomic_n => {
                    if atomic.has_pending() {
                        return WaitSource::Atomic;
                    }
                }
                () = evm_n => {
                    if evm.has_pending() {
                        return WaitSource::Evm;
                    }
                }
            }
        }
    }
}

/// `txpool.Txpool` — an in-memory pool of cross-chain atomic transactions.
pub struct AtomicTxpool {
    /// The AVAX asset id used to compute each tx's [`Op`](ava_saevm_hook::op::Op).
    avax_asset_id: Id,
    inner: Mutex<Inner>,
    notify: Notify,
}

#[derive(Default)]
struct Inner {
    /// The pooled txs, keyed by id.
    txs: BTreeMap<Id, Tx>,
    /// `txID → consumed input ids`, for conflict detection.
    inputs: BTreeMap<Id, BTreeSet<Id>>,
}

impl AtomicTxpool {
    /// Constructs an empty atomic txpool over the chain's AVAX asset id.
    #[must_use]
    pub fn new(avax_asset_id: Id) -> Self {
        Self {
            avax_asset_id,
            inner: Mutex::new(Inner::default()),
            notify: Notify::new(),
        }
    }

    /// The chain's AVAX asset id.
    #[must_use]
    pub fn avax_asset_id(&self) -> Id {
        self.avax_asset_id
    }

    /// `Txpool.Add` — admit `tx` into the pool and signal waiters.
    ///
    /// Returns [`Error::AlreadyKnown`] if a tx with the same id is present.
    ///
    /// TODO(M7.23): verify credentials + the op against the last-executed
    /// state, and evict lower-fee conflicts (Go `Txpool.Add`).
    ///
    /// # Errors
    /// Returns [`Error::AlreadyKnown`] if `tx` is already pooled.
    pub fn add(&self, tx: Tx) -> Result<(), Error> {
        let id = tx.id();
        let inputs: BTreeSet<Id> = tx.input_ids().into_iter().collect();
        {
            let mut inner = self.inner.lock();
            if inner.txs.contains_key(&id) {
                return Err(Error::AlreadyKnown);
            }
            inner.txs.insert(id, tx);
            inner.inputs.insert(id, inputs);
        }
        self.notify.notify_waiters();
        Ok(())
    }

    /// `Pending.Len` — the number of pooled txs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().txs.len()
    }

    /// Whether the pool is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().txs.is_empty()
    }

    /// `Pending.Has` — whether `tx_id` is pooled.
    #[must_use]
    pub fn has(&self, tx_id: Id) -> bool {
        self.inner.lock().txs.contains_key(&tx_id)
    }

    /// The pooled txs, cloned (for block building / inspection).
    #[must_use]
    pub fn txs(&self) -> Vec<Tx> {
        self.inner.lock().txs.values().cloned().collect()
    }
}

impl WaitPool for AtomicTxpool {
    fn notify(&self) -> &Notify {
        &self.notify
    }
    fn has_pending(&self) -> bool {
        !self.is_empty()
    }
}

/// A minimal stand-in for the EVM/`txgossip` pool, used to exercise the
/// `WaitForEvent` select-across-both-pools seam in tests and as the M7.23
/// integration point.
///
/// TODO(M7.23): replace with the real reth txpool head-event subscription.
#[derive(Default)]
pub struct EvmPoolStub {
    count: Mutex<usize>,
    notify: Notify,
}

impl EvmPoolStub {
    /// Records an EVM tx arrival and signals waiters.
    pub fn add_evm(&self) {
        {
            let mut count = self.count.lock();
            *count = count.saturating_add(1);
        }
        self.notify.notify_waiters();
    }

    /// The number of EVM txs recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        *self.count.lock()
    }

    /// Whether no EVM txs are recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self.count.lock() == 0
    }
}

impl WaitPool for EvmPoolStub {
    fn notify(&self) -> &Notify {
        &self.notify
    }
    fn has_pending(&self) -> bool {
        !self.is_empty()
    }
}
