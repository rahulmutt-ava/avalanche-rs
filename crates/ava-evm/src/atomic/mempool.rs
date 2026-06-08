// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Atomic X<->C mempool (spec 10 §6.4, §17.4).
//!
//! Port of coreth `plugin/evm/atomic/txpool/{mempool,txs,tx_heap}.go`. A
//! *sidecar* pool, separate from the EVM (reth) txpool, because its items
//! ([`Tx`]) are atomic X<->C transactions, **not** revm txs. It orders pending
//! txs by **effective gas price** (highest first), dedups + conflict-checks by
//! **source UTXO id**, and the block builder pulls **one gas-limited batch per
//! block** ([`AtomicMempool::next_batch`]).
//!
//! ## Tx lifecycle (coreth `txpool/txs.go`)
//!
//! A tx in the pool is in exactly one of four states:
//!
//! - **Pending** — eligible for the builder; ordered by effective gas price.
//! - **Current** — pulled into the block currently being built.
//! - **Issued** — included in a block this node already built.
//! - **Discarded** — previously pooled, later deemed invalid; remembered (an LRU
//!   in coreth) so it is not re-requested. Local re-adds bypass this check.
//!
//! ## Effective gas price (`atomic.EffectiveGasPrice`, coreth `tx.go:295`)
//!
//! `gasPrice = burned(AVAX) * X2CRate / gasUsed`, rounded down (integer). coreth
//! computes this in `uint256`; `burned ≤ u64` and `X2CRate = 1e9` so the product
//! can exceed `u64`, hence we accumulate in `u128` (no floats — overview §6.1).
//! `gasUsed == 0` is the [`MempoolError::NoGasUsed`] sentinel.
//!
//! ## Wake-on-nonempty
//!
//! [`AtomicMempool::subscribe`] hands out a [`tokio::sync::Notify`]; every
//! admission calls `notify_one`, so the builder driver (M6.20) can park on
//! `notified()` and wake when the pool gains work (coreth `pending chan`).
//!
//! ## Gossip
//!
//! [`Tx`] implements the local [`Gossipable`] seam so atomic txs gossip over the
//! p2p SDK (spec 05). `ava-network` does not yet expose a generic push/pull
//! gossip framework (only IP gossip exists; see `ava-avm` `network/gossip.rs`),
//! so — exactly like the X-Chain — this is a minimal local trait the deferred
//! transport will adopt.
//!
//! ## Context type (`AvaNextBlockCtx`)
//!
//! [`next_batch`](AtomicMempool::next_batch) takes a context carrying the atomic
//! gas budget for the next block. As of **M6.13** that context is the canonical
//! [`AvaNextBlockCtx`] (timestamp(ms), recipient, gas limit, P-chain height,
//! parent fee state; spec 10 §17.3), defined in `evmconfig` and re-exported
//! here. The mempool reads only its `atomic_gas_limit` budget.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use ava_avm::txs::components::Input as FxInput;
use ava_types::id::Id;
use tokio::sync::Notify;

use crate::atomic::tx::{AtomicTx, TX_BYTES_GAS, Tx, X2C_RATE};
// M6.13: the canonical next-block build/fee context now lives in `evmconfig`;
// the mempool consumes only its `atomic_gas_limit` budget (the M6.16 local stub
// was folded into it). Re-exported here so `next_batch` callers keep importing
// it from `mempool` unchanged.
pub use crate::evmconfig::AvaNextBlockCtx;

pub use gossip::Gossipable;

/// `ap5.AtomicTxIntrinsicGas` — the fixed gas added to every atomic tx's
/// `GasUsed` once Apricot Phase 5 is active (coreth `upgrade/ap5/params.go:38`).
///
/// The mempool always charges it (coreth passes `fixedFee = true` to
/// `EffectiveGasPrice`), matching the post-AP5 mainnet/Fuji reality.
pub const ATOMIC_TX_INTRINSIC_GAS: u64 = 10_000;

/// The number of discarded tx ids the pool remembers (coreth
/// `discardedTxsCacheSize`). coreth uses an LRU; we use a bounded FIFO with the
/// same capacity and eviction-on-overflow semantics (the only observable
/// difference — which old id is forgotten first under churn — is not
/// consensus-affecting; discarded is a courtesy de-dup, re-issue is always
/// allowed locally).
pub const DISCARDED_TXS_CACHE_SIZE: usize = 50;

/// Why an atomic tx was not admitted to (or was evicted from) the mempool.
///
/// Mirrors coreth `txpool/mempool.go`'s sentinels (`ErrAlreadyKnown`,
/// `ErrConflict`, `ErrInsufficientFee`, `ErrMempoolFull`) plus the
/// `EffectiveGasPrice` `ErrNoGasUsed` failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MempoolError {
    /// `ErrAlreadyKnown` — the tx is already Pending/Current/Issued (or, for a
    /// remote add, Discarded).
    #[error("already known")]
    AlreadyKnown,

    /// `ErrConflict` — the tx spends a source UTXO already spent by a pending
    /// tx, and does not pay a strictly higher effective gas price than every
    /// conflict (so the incumbents win).
    #[error("conflict present")]
    Conflict,

    /// `ErrInsufficientFee` — the pool is full and the tx does not outbid the
    /// cheapest pending tx it would replace.
    #[error("insufficient fee")]
    InsufficientFee,

    /// `ErrMempoolFull` — the pool is full and there is no pending tx to evict
    /// (the whole size allowance is Current/Issued).
    #[error("mempool full")]
    MempoolFull,

    /// `ErrNoGasUsed` — the tx reported zero gas, so it has no effective gas
    /// price (`atomic.ErrNoGasUsed`).
    #[error("no gas used")]
    NoGasUsed,

    /// Checked arithmetic overflowed while computing gas/burn (overview §6.1).
    #[error("gas/fee arithmetic overflow")]
    Overflow,
}

/// A pending tx plus its precomputed effective gas price (the heap key).
#[derive(Debug, Clone)]
struct PendingEntry {
    tx: Tx,
    gas_price: u128,
}

/// Atomic X<->C transaction mempool (coreth `atomic/txpool`).
///
/// Single-threaded by design: the VM holds it behind its own lock (mirroring
/// coreth's `lock sync.RWMutex`), so methods take `&mut self`. Ordering, dedup,
/// and conflict semantics are byte-for-byte faithful to coreth; see the module
/// docs.
#[derive(Debug)]
pub struct AtomicMempool {
    /// `maxSize` — capacity in transactions (Pending + Current + Issued).
    max_size: usize,
    /// `ctx.AVAXAssetID` — the asset effective-gas-price is denominated in.
    avax_asset_id: Id,

    /// Pending txs by id, each with its cached effective gas price. A `HashMap`
    /// (not a binary heap) because conflict-eviction and fee-replacement need
    /// arbitrary removal; the heap order is materialized on demand in
    /// [`Self::next_batch`] (coreth keeps a min+max heap, but the pool is small
    /// and O(n log n) per build is negligible vs block production).
    pending: HashMap<Id, PendingEntry>,
    /// `currentTxs` — txs pulled into the block being built.
    current: HashMap<Id, Tx>,
    /// `issuedTxs` — txs included in a block this node already built.
    issued: HashMap<Id, Tx>,
    /// `utxoSpenders` — source UTXO id -> the Pending/Current/Issued tx spending
    /// it. Discarded txs are NOT recorded here.
    utxo_spenders: HashMap<Id, Id>,
    /// `discardedTxs` — bounded set of ids deemed invalid (FIFO eviction).
    discarded: HashMap<Id, ()>,
    /// Insertion order for the discarded FIFO eviction.
    discarded_order: std::collections::VecDeque<Id>,

    /// Wakes the builder when the pool gains a pending tx (coreth `pending`
    /// chan). Shared via [`Self::subscribe`].
    notify: Arc<Notify>,
}

impl AtomicMempool {
    /// Builds an empty mempool with capacity `max_size` txs, pricing burns in
    /// `avax_asset_id`.
    #[must_use]
    pub fn new(max_size: usize, avax_asset_id: Id) -> Self {
        Self {
            max_size,
            avax_asset_id,
            pending: HashMap::new(),
            current: HashMap::new(),
            issued: HashMap::new(),
            utxo_spenders: HashMap::new(),
            discarded: HashMap::new(),
            discarded_order: std::collections::VecDeque::new(),
            notify: Arc::new(Notify::new()),
        }
    }

    /// A [`Notify`] handle that fires (`notify_one`) whenever a tx is admitted to
    /// Pending. The builder driver parks on `notified()` to wake on new work
    /// (coreth `SubscribePendingTxs`).
    #[must_use]
    pub fn subscribe(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// Total txs counted against `maxSize` (Pending + Current + Issued); coreth
    /// `length()`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len() + self.current.len() + self.issued.len()
    }

    /// Whether the pool holds no txs in any non-discarded state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of Pending txs (eligible for the next batch); coreth `PendingLen`.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Whether `tx_id` is Pending, Current, or Issued (not Discarded); coreth
    /// `Has`.
    #[must_use]
    pub fn has(&self, tx_id: &Id) -> bool {
        self.pending.contains_key(tx_id)
            || self.current.contains_key(tx_id)
            || self.issued.contains_key(tx_id)
    }

    /// Whether `tx_id` is currently in the Discarded set.
    #[must_use]
    pub fn is_discarded(&self, tx_id: &Id) -> bool {
        self.discarded.contains_key(tx_id)
    }

    /// The cached signed bytes of `tx_id` if the pool holds it in any active
    /// state (Pending/Current/Issued); `None` otherwise. Used by `avax.getAtomicTx`
    /// to return a processing tx's bytes (coreth `Mempool.GetTx`).
    #[must_use]
    pub fn get_tx_bytes(&self, tx_id: &Id) -> Option<Vec<u8>> {
        self.pending
            .get(tx_id)
            .map(|e| &e.tx)
            .or_else(|| self.current.get(tx_id))
            .or_else(|| self.issued.get(tx_id))
            .map(|tx| tx.bytes().to_vec())
    }

    /// `Add` / `AddRemoteTx` — add `tx` as a **remote** tx. Remote txs that fail
    /// admission (other than already-known / full) are recorded as Discarded so
    /// they are not re-requested.
    ///
    /// # Errors
    /// See [`MempoolError`].
    pub fn add(&mut self, tx: Tx) -> Result<(), MempoolError> {
        self.add_remote(tx)
    }

    /// `AddRemoteTx` — see [`Self::add`].
    ///
    /// # Errors
    /// See [`MempoolError`].
    pub fn add_remote(&mut self, tx: Tx) -> Result<(), MempoolError> {
        let tx_id = tx.id();
        match self.add_tx(tx, false, false) {
            Ok(()) => Ok(()),
            Err(e) => {
                // coreth `errsNotToDiscard`: AlreadyKnown / MempoolFull are not
                // the tx's fault, so don't poison it.
                if !matches!(e, MempoolError::AlreadyKnown | MempoolError::MempoolFull) {
                    self.put_discarded(tx_id);
                }
                Err(e)
            }
        }
    }

    /// `AddLocalTx` — add `tx` as a **local** tx (skips the Discarded check, so a
    /// previously-discarded tx can be re-issued once its atomic UTXO is present).
    ///
    /// # Errors
    /// See [`MempoolError`].
    pub fn add_local(&mut self, tx: Tx) -> Result<(), MempoolError> {
        self.add_tx(tx, true, false)
    }

    /// `ForceAddTx` — add `tx` bypassing the Discarded check **and** all conflict
    /// / fee-eviction checks.
    ///
    /// # Errors
    /// Returns [`MempoolError::NoGasUsed`]/[`MempoolError::Overflow`] only (gas
    /// pricing still runs); all admission gates are skipped.
    pub fn force_add(&mut self, tx: Tx) -> Result<(), MempoolError> {
        self.add_tx(tx, true, true)
    }

    /// coreth `addTx(tx, local, force)`.
    fn add_tx(&mut self, tx: Tx, local: bool, force: bool) -> Result<(), MempoolError> {
        let tx_id = tx.id();

        // Already present in any active state?
        if self.issued.contains_key(&tx_id)
            || self.current.contains_key(&tx_id)
            || self.pending.contains_key(&tx_id)
        {
            return Err(MempoolError::AlreadyKnown);
        }
        if !local && self.discarded.contains_key(&tx_id) {
            return Err(MempoolError::AlreadyKnown);
        }

        let gas_price = self.effective_gas_price(&tx)?;

        // Conflict check against pending spenders of this tx's source UTXOs.
        let utxos = input_utxos(&tx);
        if !force {
            let (highest, conflicts) = self.check_conflict(&utxos);
            if !conflicts.is_empty() {
                // Must strictly outbid every conflict, else incumbents win.
                if gas_price <= highest {
                    return Err(MempoolError::Conflict);
                }
                for conflict_id in conflicts {
                    self.remove_tx(&conflict_id, true);
                }
            }
        }

        // Capacity: evict at most one lowest-priced pending tx (size is in txs).
        if self.len() >= self.max_size {
            if self.pending.is_empty() {
                return Err(MempoolError::MempoolFull);
            }
            let (min_id, min_price) = self.peek_min();
            if gas_price <= min_price {
                return Err(MempoolError::InsufficientFee);
            }
            self.remove_tx(&min_id, true);
        }

        // Re-issuing a recently discarded tx: drop the discarded marker so it is
        // not in two places.
        self.evict_discarded(&tx_id);

        // Mark Pending + index its source-UTXO spenders.
        for utxo in &utxos {
            self.utxo_spenders.insert(*utxo, tx_id);
        }
        self.pending.insert(tx_id, PendingEntry { tx, gas_price });

        // Signal the builder there is work.
        self.notify.notify_one();
        Ok(())
    }

    /// coreth `checkConflictTx`: among pending txs spending any UTXO in
    /// `utxos`, return `(highest_gas_price, conflicting_tx_ids)`.
    fn check_conflict(&self, utxos: &[Id]) -> (u128, Vec<Id>) {
        let mut highest: u128 = 0;
        let mut conflicts: Vec<Id> = Vec::new();
        for utxo in utxos {
            if let Some(spender_id) = self.utxo_spenders.get(utxo)
                && let Some(entry) = self.pending.get(spender_id)
            {
                if entry.gas_price > highest {
                    highest = entry.gas_price;
                }
                if !conflicts.contains(spender_id) {
                    conflicts.push(*spender_id);
                }
            }
        }
        (highest, conflicts)
    }

    /// The lowest-priced pending tx id + its gas price (coreth `PeekMin`).
    /// Ties broken by tx id (deterministic). Caller ensures `pending` non-empty.
    fn peek_min(&self) -> (Id, u128) {
        let mut best: Option<(Id, u128)> = None;
        for (id, entry) in &self.pending {
            match best {
                None => best = Some((*id, entry.gas_price)),
                Some((best_id, best_price)) => {
                    if entry.gas_price < best_price
                        || (entry.gas_price == best_price && id.to_bytes() < best_id.to_bytes())
                    {
                        best = Some((*id, entry.gas_price));
                    }
                }
            }
        }
        best.unwrap_or((Id::EMPTY, 0))
    }

    /// `next_batch(ctx)` — pull **one gas-limited batch** of the highest-priced
    /// pending txs for the block being built, marking each pulled tx Current.
    ///
    /// Txs are taken in descending effective-gas-price order (ties by tx id, for
    /// determinism) until the next tx would push the cumulative atomic
    /// `GasUsed` over `ctx.atomic_gas_limit`. A tx that individually exceeds the
    /// remaining budget is **skipped** (left Pending — coreth `CancelCurrentTx`
    /// semantics), letting a smaller, lower-priced tx still fit. Pulled txs are
    /// returned in the order they were selected (so the builder can apply them
    /// in priority order).
    pub fn next_batch(&mut self, ctx: &AvaNextBlockCtx) -> Vec<Tx> {
        // Materialize the max-heap order: sort pending by (gas_price desc, id asc).
        let mut ordered: Vec<(Id, u128)> = self
            .pending
            .iter()
            .map(|(id, e)| (*id, e.gas_price))
            .collect();
        ordered.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.to_bytes().cmp(&b.0.to_bytes()))
        });

        let mut batch = Vec::new();
        let mut used: u64 = 0;
        for (id, _price) in ordered {
            let Some(entry) = self.pending.get(&id) else {
                continue;
            };
            let gas = match self.tx_gas_used(&entry.tx) {
                Ok(g) => g,
                // A pending tx whose gas can't be computed is dropped from
                // consideration (should never happen — it priced on add).
                Err(_) => continue,
            };
            let Some(next_used) = used.checked_add(gas) else {
                continue;
            };
            if next_used > ctx.atomic_gas_limit {
                // Over budget with this tx; skip it but keep scanning for a
                // smaller tx that still fits.
                continue;
            }
            used = next_used;
            // Move Pending -> Current. (UTXO spenders stay indexed.)
            if let Some(e) = self.pending.remove(&id) {
                self.current.insert(id, e.tx.clone());
                batch.push(e.tx);
            }
        }
        batch
    }

    /// `IssueCurrentTxs` — mark all Current txs as Issued (the block was built).
    pub fn issue_current_txs(&mut self) {
        for (id, tx) in self.current.drain() {
            self.issued.insert(id, tx);
        }
    }

    /// `CancelCurrentTx` — return the Current tx `tx_id` to Pending (it could not
    /// be included but is still valid, e.g. it would exceed the gas budget).
    pub fn cancel_current_tx(&mut self, tx_id: &Id) {
        if let Some(tx) = self.current.remove(tx_id) {
            self.repend(tx);
        }
    }

    /// `CancelCurrentTxs` — return all Current txs to Pending.
    pub fn cancel_current_txs(&mut self) {
        let drained: Vec<Tx> = self.current.drain().map(|(_, tx)| tx).collect();
        for tx in drained {
            self.repend(tx);
        }
    }

    /// `DiscardCurrentTx` — mark the Current tx `tx_id` as Discarded (it failed
    /// verification, e.g. a conflict with an ancestor block). Clears its UTXO
    /// spenders so the source UTXOs are free for another tx.
    pub fn discard_current_tx(&mut self, tx_id: &Id) {
        if let Some(tx) = self.current.remove(tx_id) {
            self.remove_spenders(&tx);
            self.put_discarded(*tx_id);
        }
    }

    /// `DiscardCurrentTxs` — mark all Current txs as Discarded.
    pub fn discard_current_txs(&mut self) {
        let drained: Vec<(Id, Tx)> = self.current.drain().collect();
        for (id, tx) in drained {
            self.remove_spenders(&tx);
            self.put_discarded(id);
        }
    }

    /// `RemoveTx` — fully remove `tx` from the pool, including its Discarded
    /// marker.
    pub fn remove(&mut self, tx_id: &Id) {
        self.remove_tx(tx_id, false);
    }

    /// coreth `cancelTx`: Current -> Pending (re-prices the tx). If pricing
    /// fails the tx is discarded instead.
    fn repend(&mut self, tx: Tx) {
        let tx_id = tx.id();
        match self.effective_gas_price(&tx) {
            Ok(gas_price) => {
                self.pending.insert(tx_id, PendingEntry { tx, gas_price });
                self.notify.notify_one();
            }
            Err(_) => {
                self.remove_spenders(&tx);
                self.put_discarded(tx_id);
            }
        }
    }

    /// coreth `removeTx(tx, discard)`: drop `tx_id` from every active state and
    /// its UTXO spenders; if `discard` keep a Discarded marker, else evict it.
    fn remove_tx(&mut self, tx_id: &Id, discard: bool) {
        let removed = self
            .pending
            .remove(tx_id)
            .map(|e| e.tx)
            .or_else(|| self.current.remove(tx_id))
            .or_else(|| self.issued.remove(tx_id));
        if let Some(tx) = removed {
            self.remove_spenders(&tx);
        }
        if discard {
            self.put_discarded(*tx_id);
        } else {
            self.evict_discarded(tx_id);
        }
    }

    /// coreth `removeSpenders`: drop every source-UTXO -> tx mapping for `tx`.
    fn remove_spenders(&mut self, tx: &Tx) {
        for utxo in input_utxos(tx) {
            // Only clear the entry if it still points at this tx (a replacement
            // may already own it).
            if let Entry::Occupied(e) = self.utxo_spenders.entry(utxo)
                && *e.get() == tx.id()
            {
                e.remove();
            }
        }
    }

    /// Insert `tx_id` into the bounded Discarded FIFO (evicting the oldest).
    fn put_discarded(&mut self, tx_id: Id) {
        if self.discarded.insert(tx_id, ()).is_none() {
            self.discarded_order.push_back(tx_id);
            while self.discarded_order.len() > DISCARDED_TXS_CACHE_SIZE {
                if let Some(old) = self.discarded_order.pop_front() {
                    self.discarded.remove(&old);
                }
            }
        }
    }

    /// Drop `tx_id` from the Discarded set (coreth `discardedTxs.Evict`).
    fn evict_discarded(&mut self, tx_id: &Id) {
        if self.discarded.remove(tx_id).is_some() {
            self.discarded_order.retain(|id| id != tx_id);
        }
    }

    // -----------------------------------------------------------------------
    // Gas / fee accounting (coreth `atomic/{tx,import_tx,export_tx}.go`)
    // -----------------------------------------------------------------------

    /// `atomic.EffectiveGasPrice(tx, AVAXAssetID, isAP5=true)` — the price per
    /// gas in aAVAX/gas, rounded down: `burned * X2CRate / gasUsed`.
    ///
    /// # Errors
    /// [`MempoolError::NoGasUsed`] if the tx uses zero gas;
    /// [`MempoolError::Overflow`] on checked-arithmetic overflow.
    fn effective_gas_price(&self, tx: &Tx) -> Result<u128, MempoolError> {
        let gas_used = self.tx_gas_used(tx)?;
        if gas_used == 0 {
            return Err(MempoolError::NoGasUsed);
        }
        let burned = burned(tx, self.avax_asset_id)?;
        // burned * X2CRate / gasUsed, in u128 (burned ≤ u64, X2CRate = 1e9).
        let numerator = u128::from(burned)
            .checked_mul(u128::from(X2C_RATE))
            .ok_or(MempoolError::Overflow)?;
        Ok(numerator / u128::from(gas_used))
    }

    /// `tx.GasUsed(fixedFee=true)` — the atomic gas this tx consumes (used both
    /// for pricing and the batch budget). Exposed for tests / the builder.
    ///
    /// # Errors
    /// [`MempoolError::Overflow`] on checked-arithmetic overflow.
    pub fn tx_gas_used(&self, tx: &Tx) -> Result<u64, MempoolError> {
        gas_used(tx)
    }
}

/// coreth `tx.InputUTXOs()` — the set of source UTXO ids `tx` spends.
///
/// - **Import:** each imported input's `InputID()` (`tx_id.prefix(index)`).
/// - **Export:** a synthetic 32-byte id per EVM input,
///   `PackLong(nonce) || PackBytes(address)` = 8B nonce + 4B len(=20) + 20B
///   address (coreth `export_tx.go:60`).
fn input_utxos(tx: &Tx) -> Vec<Id> {
    match &tx.unsigned {
        AtomicTx::Import(import) => import
            .imported_inputs
            .iter()
            .map(|input| input.input_id())
            .collect(),
        AtomicTx::Export(export) => export.ins.iter().map(export_input_id).collect(),
    }
}

/// coreth `UnsignedExportTx.InputUTXOs` synthetic id for one EVM input:
/// `PackLong(nonce) || PackBytes(address)` into a zeroed 32-byte buffer.
/// `PackBytes` writes a 4-byte big-endian length (always 20) then the address.
fn export_input_id(in_: &crate::atomic::tx::EvmInput) -> Id {
    let mut raw = [0u8; 32];
    raw[0..8].copy_from_slice(&in_.nonce.to_be_bytes());
    raw[8..12].copy_from_slice(&20u32.to_be_bytes());
    raw[12..32].copy_from_slice(&in_.address);
    Id::from(raw)
}

/// coreth `tx.GasUsed(fixedFee=true)`:
/// `calcBytesCost(len(bytes)) + per-input cost + AtomicTxIntrinsicGas`.
fn gas_used(tx: &Tx) -> Result<u64, MempoolError> {
    // calcBytesCost(n) = n * TxBytesGas. The signed bytes length matches Go's
    // `len(utx.Bytes())` (initialize() caches the full signed bytes).
    let byte_len = u64::try_from(tx.bytes().len()).map_err(|_| MempoolError::Overflow)?;
    let mut cost = byte_len
        .checked_mul(TX_BYTES_GAS)
        .ok_or(MempoolError::Overflow)?;

    match &tx.unsigned {
        AtomicTx::Import(import) => {
            // + each imported input's fx Cost() (len(SigIndices) * CostPerSignature).
            for input in &import.imported_inputs {
                let in_cost = match &input.r#in {
                    FxInput::SecpTransfer(ti) => {
                        ti.input.cost().map_err(|_| MempoolError::Overflow)?
                    }
                };
                cost = cost.checked_add(in_cost).ok_or(MempoolError::Overflow)?;
            }
        }
        AtomicTx::Export(export) => {
            // + numSigs * CostPerSignature, one signature per EVM input.
            let num_sigs = u64::try_from(export.ins.len()).map_err(|_| MempoolError::Overflow)?;
            let sig_cost = num_sigs
                .checked_mul(crate::atomic::tx::COST_PER_SIGNATURE)
                .ok_or(MempoolError::Overflow)?;
            cost = cost.checked_add(sig_cost).ok_or(MempoolError::Overflow)?;
        }
    }

    cost.checked_add(ATOMIC_TX_INTRINSIC_GAS)
        .ok_or(MempoolError::Overflow)
}

/// coreth `tx.Burned(assetID)` — `input(assetID) - spent(assetID)`, checked.
///
/// - **Import:** input = sum of imported-input amounts of `asset_id`; spent =
///   sum of EVM-output amounts of `asset_id`.
/// - **Export:** input = sum of EVM-input amounts of `asset_id`; spent = sum of
///   exported-output amounts of `asset_id`.
fn burned(tx: &Tx, asset_id: Id) -> Result<u64, MempoolError> {
    let (input, spent) = match &tx.unsigned {
        AtomicTx::Import(import) => {
            let mut spent = 0u64;
            for out in &import.outs {
                if out.asset_id == asset_id {
                    spent = spent
                        .checked_add(out.amount)
                        .ok_or(MempoolError::Overflow)?;
                }
            }
            let mut input = 0u64;
            for in_ in &import.imported_inputs {
                if in_.asset_id() == asset_id {
                    input = input
                        .checked_add(in_.amount())
                        .ok_or(MempoolError::Overflow)?;
                }
            }
            (input, spent)
        }
        AtomicTx::Export(export) => {
            let mut spent = 0u64;
            for out in &export.exported_outputs {
                if out.asset_id() == asset_id {
                    spent = spent
                        .checked_add(out.amount())
                        .ok_or(MempoolError::Overflow)?;
                }
            }
            let mut input = 0u64;
            for in_ in &export.ins {
                if in_.asset_id == asset_id {
                    input = input
                        .checked_add(in_.amount)
                        .ok_or(MempoolError::Overflow)?;
                }
            }
            (input, spent)
        }
    };
    input.checked_sub(spent).ok_or(MempoolError::Overflow)
}

/// Local gossip seam (spec 05). `ava-network` does not yet expose a generic
/// push/pull gossip framework (only IP gossip), so — exactly like the X-Chain
/// (`ava-avm` `network/gossip.rs`) — atomic txs gossip behind this minimal local
/// trait that the deferred transport will adopt.
pub mod gossip {
    use ava_types::id::Id;

    use crate::atomic::tx::Tx;

    /// A value identifiable for gossip deduplication
    /// (`network/p2p/gossip.Gossipable`; coreth bloom-filters atomic txs by id).
    pub trait Gossipable {
        /// The gossip deduplication id (`gossip_id = tx_id`).
        fn gossip_id(&self) -> Id;
    }

    impl Gossipable for Tx {
        fn gossip_id(&self) -> Id {
            self.id()
        }
    }
}
