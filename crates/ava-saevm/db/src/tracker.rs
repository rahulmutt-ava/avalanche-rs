// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The SAE state-DB / Firewood-revision [`Tracker`] (specs/11 §7.1).
//!
//! # Design: a ref-count layer over an encapsulated revision manager
//!
//! [`ava_evm::FirewoodStateProvider`] exposes **no** public
//! `track`/`untrack`/`RevisionManager` API — revision lifecycle (the retained
//! window, eviction) is encapsulated inside it. To map Go's
//! `triedb.Reference`/`Dereference` semantics onto Firewood's `RevisionManager`
//! (the deviation called out in specs/11 §7.1), [`Tracker`] keeps its **own**
//! ref-count map ([`BTreeMap<B256, u64>`]) recording which roots consensus still
//! needs. Commits go through [`ava_evm::FirewoodStateProvider::commit`]; reads
//! through [`ava_evm::FirewoodStateProvider::history_by_state_root`]. When a
//! root's count hits zero and it is durably on disk, the in-memory reference may
//! drop — the provider's own retained window then governs eviction.
//!
//! # CC-ORDER (specs/27 §2.4) — the binding ordering invariant
//!
//! Execution state lives in Firewood while consensus pointers live in the KV
//! store, so block-Accept cannot be one physical batch. The fix is a **commit
//! order**: commit execution/state **durably first**, then advance the
//! consensus/last-accepted pointer. The recovery procedure tolerates "state
//! committed but pointer not advanced" (redo the cheap pointer write) but
//! **forbids** "pointer advanced but state missing".
//!
//! [`Tracker::maybe_commit`] performs the *durable Firewood commit*. The
//! executor calls it **before** the consensus pointer advances (specs/11 §6.1
//! step 10: `maybe_commit` → `track` → `mark_executed` in D→M→I→X order), so the
//! ordering holds: by the time any external signal advances a frontier past
//! `height`, the execution root for the relevant commit boundary is already
//! durable. This module enforces the durability half (the commit); the executor
//! enforces the call order.
//!
//! # No reorgs (specs/11 §10 invariant 9)
//!
//! Acceptance is final, so the snapshot layer may be flattened freely. On
//! [`Tracker::close`] we commit the last root unconditionally (flatten to the
//! last durable revision).

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;

use ava_evm::FirewoodStateProvider;
use ava_evm_reth::B256;

/// The default trie-commit interval (Go `triedb` commit cadence): commit the
/// settled root every `4096` blocks when not archival.
pub const DEFAULT_COMMIT_INTERVAL: u64 = 4096;

/// A read view at one retained Firewood revision — the public alias for
/// [`ava_evm::FirewoodStateView`] (implements reth's `StateProvider`).
///
/// Obtained via [`Tracker::state_db`]; the natural fit per specs/11 §7.1
/// (`state_db(root) = provider.history_by_state_root(root)`).
pub type StateDb = ava_evm::FirewoodStateView;

/// Tracker errors. Sentinels are preserved as variants (specs/11 §11 error
/// model) and the underlying `ava-evm` error is folded in via `#[from]`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The Firewood commit (or open) failed in the EVM state layer.
    #[error(transparent)]
    Evm(#[from] ava_evm::Error),

    /// Opening a read view at `root` failed — the revision is not retained
    /// (outside Firewood's window) or the database errored.
    #[error("no retained revision for state root {0}")]
    NoRevision(B256),

    /// The commit interval was configured to zero, which is invalid (it would
    /// make `height % interval` a division-by-zero).
    #[error("commit interval must be non-zero")]
    ZeroCommitInterval,
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Tracker configuration (specs/11 §7.1).
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// The trie-commit interval. The settled root is committed when
    /// `height % commit_interval == 0`. Ignored when `archival` is set. Must be
    /// non-zero.
    pub commit_interval: u64,
    /// Archival mode: commit the **execution** root on **every** block (full
    /// history retained), rather than only the settled root on interval
    /// boundaries.
    pub archival: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            commit_interval: DEFAULT_COMMIT_INTERVAL,
            archival: false,
        }
    }
}

impl Config {
    /// Archival config: commit every block.
    #[must_use]
    pub fn archival() -> Self {
        Self {
            commit_interval: DEFAULT_COMMIT_INTERVAL,
            archival: true,
        }
    }

    /// Interval config with the given (non-zero) commit interval; not archival.
    #[must_use]
    pub fn interval(commit_interval: u64) -> Self {
        Self {
            commit_interval,
            archival: false,
        }
    }
}

/// The SAE state-DB / Firewood-revision tracker (specs/11 §7.1).
///
/// Holds an [`Arc<FirewoodStateProvider>`] (the execution trie) and a private
/// ref-count layer over retained roots. See the module docs for the design,
/// CC-ORDER, and no-reorg-flatten contracts.
pub struct Tracker {
    /// The Firewood-backed EVM state provider (specs/04 §4.2).
    state: Arc<FirewoodStateProvider>,
    /// Commit policy.
    config: Config,
    /// Our own ref-count over retained roots, mapping Go's
    /// `triedb.Reference`/`Dereference`. Bounded by the consensus-critical
    /// window (`LastExecuted..LastSettled`). Roots at count zero are dropped.
    refs: Mutex<BTreeMap<B256, u64>>,
}

impl Tracker {
    /// Builds a tracker over `state` with the given commit policy.
    #[must_use]
    pub fn new(state: Arc<FirewoodStateProvider>, config: Config) -> Self {
        Self {
            state,
            config,
            refs: Mutex::new(BTreeMap::new()),
        }
    }

    /// Increment the retain-count for `root` (consensus still needs it). Mirrors
    /// Go `triedb.Reference`.
    pub fn track(&self, root: B256) {
        let mut refs = self.refs.lock();
        let count = refs.entry(root).or_insert(0);
        *count = count.saturating_add(1);
    }

    /// Decrement the retain-count for `root`. When it reaches zero the entry is
    /// removed — the in-memory reference drops and the provider's own retained
    /// window governs eviction. Mirrors Go `triedb.Dereference`. Untracking an
    /// untracked root is a no-op (saturating).
    pub fn untrack(&self, root: B256) {
        let mut refs = self.refs.lock();
        if let Some(count) = refs.get_mut(&root) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                refs.remove(&root);
            }
        }
    }

    /// The number of distinct roots currently retained (held by ≥1 reference).
    /// Bounds the consensus-critical window; primarily a test/observability hook.
    #[must_use]
    pub fn retained_count(&self) -> usize {
        self.refs.lock().len()
    }

    /// Commit policy (specs/11 §7.1), performing the durable Firewood commit
    /// half of CC-ORDER (specs/27 §2.4):
    ///
    /// - **archival** → commit `execution_root` (every block);
    /// - `height % commit_interval == 0` → commit `settled_root`;
    /// - **else** → nothing (pipelined; the root stays readable in-memory via
    ///   [`Tracker::state_db`] while it is retained).
    ///
    /// The caller (the executor) MUST invoke this **before** advancing the
    /// consensus pointer for `height`, so a crash can never leave "pointer
    /// advanced but state missing".
    ///
    /// # Errors
    /// [`Error::ZeroCommitInterval`] if `commit_interval == 0`; [`Error::Evm`]
    /// if the Firewood commit fails.
    pub fn maybe_commit(
        &self,
        settled_root: B256,
        execution_root: B256,
        height: u64,
    ) -> Result<()> {
        if self.config.archival {
            // Durably commit the execution root every block.
            self.state.commit(execution_root)?;
            return Ok(());
        }

        if self.config.commit_interval == 0 {
            return Err(Error::ZeroCommitInterval);
        }

        // `is_multiple_of` is panic-free (handles a zero divisor without a
        // div-by-zero) — the SAE arithmetic-discipline-clean boundary check.
        if height.is_multiple_of(self.config.commit_interval) {
            // On an interval boundary, durably commit the SETTLED root.
            self.state.commit(settled_root)?;
        }
        // else: pipelined — leave the proposed root in-memory. It is still
        // readable while retained; commit lands on a later boundary.
        Ok(())
    }

    /// Open a read view at any **retained** revision (specs/11 §7.1):
    /// `state_db(root) = provider.history_by_state_root(root)`.
    ///
    /// # Errors
    /// [`Error::NoRevision`] when `root` is not in Firewood's retained window.
    pub fn state_db(&self, root: B256) -> Result<StateDb> {
        self.state
            .history_by_state_root(root)
            .map_err(|_| Error::NoRevision(root))
    }

    /// The highest height whose **post-execution state root was committed** —
    /// the recovery start point (specs/11 §1.4, §7.1; specs/27 §5.4).
    ///
    /// - **archival** → every block is committed ⇒ return `head` unchanged.
    /// - **interval** → round `head` **down** to the last commit-interval
    ///   boundary (the cadence at which the settled root is durable).
    #[must_use]
    pub fn last_height_with_execution_root_committed(&self, head: u64) -> u64 {
        if self.config.archival {
            return head;
        }
        // Round `head` DOWN to the nearest multiple of the interval.
        // `checked_rem` returns `None` only for a zero divisor (invalid config);
        // we then conservatively return `head` (re-execute from there). The
        // subtraction is saturating. Both are SAE arithmetic-discipline-clean.
        match head.checked_rem(self.config.commit_interval) {
            Some(remainder) => head.saturating_sub(remainder),
            None => head,
        }
    }

    /// Flatten the snapshot to `last_root` on close. SAE has **no reorgs**
    /// (specs/11 §10 invariant 9), so the last accepted/executed root is final
    /// and we commit it unconditionally.
    ///
    /// Committing a root that is already the durable tip (or has no stashed
    /// proposal) is treated as a no-op rather than an error, since flattening to
    /// an already-flat state is idempotent.
    ///
    /// # Errors
    /// [`Error::Evm`] only for a genuine Firewood commit failure (not a missing
    /// proposal / already-committed tip).
    pub fn close(&self, last_root: B256) -> Result<()> {
        // Already the durable tip ⇒ nothing to flatten.
        if self.state.root() == last_root {
            return Ok(());
        }
        // A genuine commit succeeds; a `MissingProposal` means `last_root` was
        // already committed or never proposed — either way flattening to an
        // already-flat state is an idempotent no-op, not a failure.
        match self.state.commit(last_root) {
            Ok(()) | Err(ava_evm::Error::MissingProposal(_)) => Ok(()),
            Err(e) => Err(Error::Evm(e)),
        }
    }
}
