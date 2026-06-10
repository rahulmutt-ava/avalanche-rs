// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The three monotonic SAE frontiers and the consensus-critical map (specs/11
//! §1.1, §13.5).
//!
//! Port of `vms/saevm/blocks/access.go::Frontier`. SAE tracks **three**
//! height-monotonic pointers, all reconstructible from disk after restart
//! (specs/11 §10):
//!
//! * **`LastAccepted` (A)** — consensus has committed the ordering.
//! * **`LastExecuted` (E)** — the executor has produced + committed the
//!   post-state (the EVM "head"). Lags A by the queue depth.
//! * **`LastSettled` (S)** — an executed block whose results are referenced by a
//!   later accepted block (demonstrably agreed; "safe"/"finalized").
//!
//! The ordering invariant `height(S) <= height(E) <= height(A)` holds at all
//! times (specs/11 §10 invariant 1). The pointers are
//! [`arc_swap::ArcSwapOption`] so the hot RPC label-resolution paths
//! (`pending`/`latest`/`safe`) read lock-free (specs/11 §13.5).
//!
//! The **consensus-critical map** holds `Arc<Block>` for every block in the
//! closed window `[S, A]` — the blocks that may still be settled or whose
//! retained state revisions are pinned (specs/11 §1.2/§4.2). Go guards its
//! equivalent with an `RWMutex`-wrapped `syncMap`; the Rust port uses a
//! [`parking_lot::RwLock`]-wrapped [`HashMap`] keyed by [`Block::hash`]
//! (a `DashMap` would be a non-workspace dependency).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwapOption;
use parking_lot::RwLock;

use ava_saevm_blocks::Block;
use ava_saevm_types::B256;

/// The three monotonic SAE frontiers (S/E/A) plus the consensus-critical map
/// of the `[S, A]` window (specs/11 §1.1). Mirrors Go's `blocks.Frontier`.
///
/// Reads of the three pointers are lock-free ([`arc_swap::ArcSwapOption`]); the
/// consensus-critical map is guarded by a [`parking_lot::RwLock`]. All three
/// pointers advance by **increasing height only** — an out-of-order or stale
/// advance is ignored.
pub struct Frontier {
    /// `LastSettled` (S). Never `None` after construction (floored at genesis).
    last_settled: ArcSwapOption<Block>,
    /// `LastExecuted` (E). Never `None` after construction.
    last_executed: ArcSwapOption<Block>,
    /// `LastAccepted` (A). Never `None` after construction.
    last_accepted: ArcSwapOption<Block>,
    /// Hash → block for every block in the closed `[S, A]` window.
    consensus_critical: RwLock<HashMap<B256, Arc<Block>>>,
    /// Backing store for the `sae` `last_settled_height` gauge: the height of
    /// the latest settled block (set at construction + on every settle advance).
    ///
    /// AS-BUILT: there is no prometheus registry reaching the SAE crates yet (no
    /// metrics plumbing in `core`/`exec`); this `AtomicU64` is the honest gauge
    /// backing store, read via [`Frontier::last_settled_height`].
    /// `// TODO(M8): register this on the "sae" prometheus namespace
    /// (specs/18 §2.11).`
    last_settled_height_gauge: AtomicU64,
}

impl Frontier {
    /// Constructs a frontier rooted at `genesis` (the synchronous / last pre-SAE
    /// block), which is simultaneously accepted, executed and settled, so all
    /// three pointers start at it and it is the sole consensus-critical block.
    #[must_use]
    pub fn new(genesis: Arc<Block>) -> Self {
        let mut map = HashMap::new();
        map.insert(genesis.hash(), Arc::clone(&genesis));
        // The gauge starts at the genesis (recovered S frontier) height (Go sets
        // it once at startup).
        let genesis_height = genesis.height();
        Self {
            last_settled: ArcSwapOption::from(Some(Arc::clone(&genesis))),
            last_executed: ArcSwapOption::from(Some(Arc::clone(&genesis))),
            last_accepted: ArcSwapOption::from(Some(genesis)),
            consensus_critical: RwLock::new(map),
            last_settled_height_gauge: AtomicU64::new(genesis_height),
        }
    }

    // -- lock-free pointer reads -------------------------------------------

    /// The `LastSettled` (S) frontier block. Always defined (floored at the
    /// genesis the frontier was constructed with).
    ///
    /// # Panics
    /// Never in practice: S is set at construction and only ever advanced. The
    /// `expect` documents that the genesis floor is an invariant, not a runtime
    /// condition.
    #[must_use]
    pub fn last_settled(&self) -> Arc<Block> {
        self.last_settled
            .load_full()
            .expect("LastSettled is floored at genesis")
    }

    /// The `LastExecuted` (E) frontier block, if execution has reached at least
    /// the genesis floor (always `Some` after construction).
    #[must_use]
    pub fn last_executed(&self) -> Option<Arc<Block>> {
        self.last_executed.load_full()
    }

    /// The `LastAccepted` (A) frontier block. Always defined after construction.
    ///
    /// # Panics
    /// Never in practice: A is set at construction and only ever advanced.
    #[must_use]
    pub fn last_accepted(&self) -> Arc<Block> {
        self.last_accepted
            .load_full()
            .expect("LastAccepted is set at construction")
    }

    /// Reports whether the frontier ordering invariant
    /// `height(S) <= height(E) <= height(A)` currently holds (specs/11 §10
    /// invariant 1).
    #[must_use]
    pub fn heights_ordered(&self) -> bool {
        let s = self.last_settled().height();
        let e = self.last_executed().map_or(s, |b| b.height());
        let a = self.last_accepted().height();
        s <= e && e <= a
    }

    // -- monotonic advances ------------------------------------------------

    /// Advances `LastAccepted` to `block` (increasing height only) and inserts
    /// it into the consensus-critical map. A stale/equal advance is ignored.
    pub fn advance_accepted(&self, block: &Arc<Block>) {
        if Self::advances(&self.last_accepted, block) {
            self.last_accepted.store(Some(Arc::clone(block)));
            self.consensus_critical
                .write()
                .insert(block.hash(), Arc::clone(block));
        }
    }

    /// Advances `LastExecuted` to `block` (increasing height only). A stale/equal
    /// advance is ignored.
    pub fn advance_executed(&self, block: &Arc<Block>) {
        if Self::advances(&self.last_executed, block) {
            self.last_executed.store(Some(Arc::clone(block)));
        }
    }

    /// Advances `LastSettled` to `block` (increasing height only), evicting every
    /// block strictly **below** the new S from the consensus-critical map (those
    /// ancestors are no longer settle-candidates and their pinned revisions can
    /// be released). The settled block itself is retained as the window floor. A
    /// stale/equal advance is ignored.
    pub fn advance_settled(&self, block: &Arc<Block>) {
        if Self::advances(&self.last_settled, block) {
            self.last_settled.store(Some(Arc::clone(block)));
            // Update the `sae` `last_settled_height` gauge (Go sets it on settle
            // in `AcceptBlock`). `Relaxed` is fine: a monitoring gauge has no
            // ordering relationship with the consensus state.
            self.last_settled_height_gauge
                .store(block.height(), Ordering::Relaxed);
            let floor = block.height();
            self.consensus_critical
                .write()
                .retain(|_, b| b.height() >= floor);
        }
    }

    /// Whether advancing `ptr` to `block` is a forward (strictly
    /// higher-height) move. A `None` pointer always advances.
    fn advances(ptr: &ArcSwapOption<Block>, block: &Arc<Block>) -> bool {
        ptr.load_full()
            .is_none_or(|cur| block.height() > cur.height())
    }

    // -- consensus-critical map --------------------------------------------

    /// The consensus-critical block with the given `hash`, if it is currently in
    /// the `[S, A]` window. Lock-free for readers is not guaranteed here (the map
    /// is `RwLock`-guarded); only the three pointers are lock-free.
    #[must_use]
    pub fn consensus_critical_block(&self, hash: B256) -> Option<Arc<Block>> {
        self.consensus_critical.read().get(&hash).map(Arc::clone)
    }

    /// The number of blocks currently in the consensus-critical (`[S, A]`)
    /// window.
    #[must_use]
    pub fn consensus_critical_len(&self) -> usize {
        self.consensus_critical.read().len()
    }

    /// The `sae` `last_settled_height` gauge value: the height of the latest
    /// settled block (set at construction + on every settle advance; specs/18
    /// §2.11). The backing store for the prometheus gauge once metrics plumbing
    /// reaches the SAE crates (M8).
    #[must_use]
    pub fn last_settled_height(&self) -> u64 {
        self.last_settled_height_gauge.load(Ordering::Relaxed)
    }
}
