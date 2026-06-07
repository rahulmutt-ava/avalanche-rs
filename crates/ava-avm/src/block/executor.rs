// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The AVM (X-Chain) block executor — Verify / Accept / Reject + atomic commit
//! (M5.16, specs 09 §7 + §6, 07 §2.3).
//!
//! Port of `vms/avm/block/executor/manager.go` / `block_context.go`, simplified:
//! the X-Chain has only `StandardBlock` (no oracle/proposal blocks), so the
//! diff cache holds a single `on_accept` diff per processed block.
//!
//! ## Verify pipeline
//!
//! For each tx in the block, in order:
//!
//! 1. `SyntacticVerifier` — stateless structural checks.
//! 2. `SemanticVerifier` — stateful checks (input UTXO existence, asset id
//!    match, `verify_fx_usage`).  The verifier reads from the **shared `Diff`**,
//!    so if tx[0] already consumed a UTXO, tx[1]'s semantic verify sees it as
//!    absent and returns a `Database(NotFound)` error — this is the double-spend
//!    detection path (00 §3.2, specs 09 §6.2).
//! 3. `Executor::execute` — applies the tx to the same `Diff` and collects
//!    atomic requests.
//!
//! The completed `Diff` and the merged `ExecutorOutputs::atomic_requests` are
//! cached per block id (`blk_id_to_state`).
//!
//! ## Accept
//!
//! On accept:
//!
//! 1. `diff.apply(&mut state)` — flush the overlay onto the persisted base.
//! 2. Record each tx's bytes (`state.add_tx`).
//! 3. Record the block bytes and height index (`state.add_block`).
//! 4. Advance `last_accepted` and `timestamp` singletons.
//! 5. If the block's atomic requests are non-empty: call
//!    `shared_memory.apply(requests, &[state.commit_batch_ops()?])` so the
//!    state write and the cross-chain atomic write happen in **one** underlying
//!    DB write (Go `sharedMemory.Apply(requests, batch)`, 27 §2.2 ATOMIC-1).
//!    If the requests are empty, fall through to `state.commit()` directly.
//! 6. Refresh `base_view = state.snapshot()`.
//!
//! ## Reject
//!
//! Discard the cached diff.
//!
//! ## `Versions` implementation
//!
//! `BlockManager` implements [`crate::state::versions::Versions`]: a cached
//! processing block's on-accept diff is the parent view for its children; when
//! `parent_id == last_accepted` the frozen `base_view` snapshot is used.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ava_database::Database;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Requests, SharedMemory};

use crate::block::Block;
use crate::error::{Error, Result};
use crate::fx::dispatch::Dispatch;
// `ReadOnlyChain` is needed for the `&Diff` → `&dyn ReadOnlyChain` coercion at
// `SemanticVerifier::new`, even though no call site names it directly.
use crate::state::chain::{Chain, ReadOnlyChain};
use crate::state::diff::Diff;
use crate::state::state::State;
use crate::state::versions::Versions;
use crate::txs::executor::backend::Backend;
use crate::txs::executor::semantic::SemanticVerifier;
use crate::txs::executor::syntactic::SyntacticVerifier;
use crate::txs::executor::{Executor, ExecutorOutputs};

/// The cached per-block state set by [`BlockManager::verify`].
struct BlockState {
    /// The on-accept diff for this block (the verified UTXO / tx mutations).
    on_accept: Arc<Diff>,
    /// The merged atomic requests for all txs in the block (empty for non-atomic
    /// blocks like pure `BaseTx` / `CreateAssetTx` / `OperationTx` blocks).
    atomic_requests: BTreeMap<Id, Requests>,
    /// The block's Unix-second timestamp (from [`Block::timestamp`]).
    timestamp: u64,
}

/// Configuration injected into a [`BlockManager`] at construction.
pub struct BlockManagerConfig {
    /// The tx executor backend (network/chain ids, fee schedule, fx count,
    /// bootstrapped flag).
    pub backend: Backend,
    /// The fx dispatch table (secp256k1fx / nftfx / propertyfx).
    pub dispatch: Dispatch,
    /// The cross-chain shared-memory handle for atomic accept.
    pub shared_memory: Arc<dyn SharedMemory>,
}

/// `block/executor.manager` — owns the persisted [`State`], the per-block diff
/// cache, and the verify / accept / reject logic for X-Chain blocks.
pub struct BlockManager<D: Database> {
    /// The persisted state base (the bottom of the diff stack).
    state: State<D>,
    /// A frozen snapshot of `state` used as the diff parent for blocks whose
    /// parent is the last-accepted block.  Refreshed after each accept.
    base_view: Arc<dyn Chain>,
    /// The tx-executor backend (fees, chain ids, fx count, bootstrapped flag).
    backend: Backend,
    /// The fx dispatch table threaded through `SemanticVerifier`.
    dispatch: Dispatch,
    /// The cross-chain shared-memory handle for atomic accept (export/import txs).
    shared_memory: Arc<dyn SharedMemory>,
    /// Per-block verified state cache, keyed by block id (Go `blkIDToState`).
    blk_id_to_state: BTreeMap<Id, BlockState>,
    /// The id of the most-recently accepted block.
    last_accepted: Id,
}

impl<D: Database + 'static> BlockManager<D> {
    /// Builds a manager over `state` with the given `config`.
    ///
    /// `state.get_last_accepted()` seeds the manager's last-accepted block id.
    #[must_use]
    pub fn new(state: State<D>, config: BlockManagerConfig) -> Self {
        let last_accepted = state.get_last_accepted();
        let base_view = state.snapshot();
        Self {
            state,
            base_view,
            backend: config.backend,
            dispatch: config.dispatch,
            shared_memory: config.shared_memory,
            blk_id_to_state: BTreeMap::new(),
            last_accepted,
        }
    }

    /// The id of the most-recently accepted block.
    #[must_use]
    pub fn last_accepted(&self) -> Id {
        self.last_accepted
    }

    /// Read-only access to the persisted state (for tests / callers).
    #[must_use]
    pub fn state(&self) -> &State<D> {
        &self.state
    }

    /// `Verify(block)` — runs the syntactic + semantic + executor pipeline for
    /// every tx in `block` over a fresh [`Diff`] layered on the parent state,
    /// then caches the resulting diff and atomic requests per block id.
    ///
    /// Double-spend detection is automatic: the executor mutates the same `Diff`
    /// that the next tx's semantic verifier reads, so the second tx's
    /// `SemanticVerifier` sees the UTXO absent and returns an error.
    ///
    /// # Errors
    /// Returns an [`Error`] if:
    /// - the parent state cannot be resolved (`MissingParentState`),
    /// - any tx fails syntactic or semantic verification, or
    /// - the executor fails (overflow, codec error).
    pub fn verify(&mut self, block: &Block) -> Result<()> {
        let parent_id = block.parent_id();

        // Resolve the parent diff parent (either a cached processing block's
        // on-accept diff or the base snapshot if parent == last-accepted).
        let parent_view = self.get_state(parent_id).ok_or(Error::MissingParentState)?;

        let mut diff = Diff::new_on(parent_view)?;
        let mut merged_atomic: BTreeMap<Id, Requests> = BTreeMap::new();

        for tx in block.txs() {
            // 1. Syntactic.
            SyntacticVerifier::new(&self.backend, tx).verify()?;

            // 2. Semantic — reads from `diff` (already-consumed UTXOs are
            //    tombstoned there, so double spends surface as NotFound).
            SemanticVerifier::new(
                &self.backend,
                &diff,
                tx,
                &self.dispatch,
                self.backend.fee_asset_id,
            )
            .verify()?;

            // 3. Execute — mutates `diff` and collects atomic requests.
            let ExecutorOutputs {
                atomic_requests, ..
            } = Executor::execute(&tx.unsigned, tx.id(), &mut diff)?;

            // Merge atomic requests: if two txs in one block both produce
            // atomic ops for the same peer chain, merge them (export → put,
            // import → remove).
            for (chain_id, reqs) in atomic_requests {
                let entry = merged_atomic.entry(chain_id).or_default();
                entry.put.extend(reqs.put);
                entry.remove.extend(reqs.remove);
            }
        }

        let timestamp = block.timestamp();
        self.blk_id_to_state.insert(
            block.id(),
            BlockState {
                on_accept: Arc::new(diff),
                atomic_requests: merged_atomic,
                timestamp,
            },
        );
        Ok(())
    }

    /// `Accept(block)` — applies the cached diff onto the persisted [`State`],
    /// records block + tx bytes, advances the last-accepted / timestamp
    /// singletons, and — if the block has atomic requests — calls
    /// `shared_memory.apply(requests, &[commit_batch])` so the state write and
    /// the atomic-memory write are **one** underlying DB write.
    ///
    /// # Errors
    /// Returns [`Error::BlockNotVerified`] if the block was never verified (no
    /// cached diff), or an [`Error`] if a state write fails.
    pub fn accept(&mut self, block: &Block) -> Result<()> {
        let blk_id = block.id();
        let cached = self
            .blk_id_to_state
            .remove(&blk_id)
            .ok_or(Error::BlockNotVerified)?;

        // Apply the diff down to the persisted state.
        cached.on_accept.apply(&mut self.state);

        // Record each tx's bytes.
        for tx in block.txs() {
            self.state.add_tx(tx.id(), tx.bytes().to_vec());
        }

        // Record the block bytes and height index.
        self.state
            .add_block(blk_id, block.height(), block.bytes().to_vec());

        // Advance last-accepted and timestamp singletons.
        self.state.set_last_accepted(blk_id);
        // Intentional silent clamp to UNIX_EPOCH on the (practically impossible)
        // u64-seconds overflow — mirrors `state::decode_timestamp`.
        let ts = UNIX_EPOCH
            .checked_add(Duration::from_secs(cached.timestamp))
            .unwrap_or(UNIX_EPOCH);
        self.state.set_timestamp(ts);
        self.last_accepted = blk_id;

        // Atomic co-commit: if the block has cross-chain requests, hand the
        // not-yet-written state batch to SharedMemory::apply so both writes
        // are atomic (Go `sharedMemory.Apply(requests, batch)`, 27 §2.2).
        if !cached.atomic_requests.is_empty() {
            let batch_ops = self.state.commit_batch_ops()?;
            self.shared_memory
                .apply(cached.atomic_requests, &[batch_ops])
                .map_err(Error::Fx)?;
            // The versiondb overlay is now committed via the batch; discard.
            self.state.abort();
        } else {
            self.state.commit()?;
        }

        // Refresh the frozen base view so the next block layers over the new base.
        self.base_view = self.state.snapshot();
        Ok(())
    }

    /// `Reject(block)` — discards the block's cached diff.
    pub fn reject(&mut self, block: &Block) {
        self.blk_id_to_state.remove(&block.id());
    }

    /// The `Chain` view to use as parent for a new diff over `parent_id`.
    ///
    /// - If `parent_id` is a cached processing block: returns its on-accept diff.
    /// - If `parent_id == last_accepted`: returns the frozen base view snapshot.
    /// - Otherwise: returns `None` (caller will surface `MissingParentState`).
    fn parent_view(&self, parent_id: Id) -> Option<Arc<dyn Chain>> {
        if let Some(s) = self.blk_id_to_state.get(&parent_id) {
            return Some(Arc::clone(&s.on_accept) as Arc<dyn Chain>);
        }
        if parent_id == self.last_accepted {
            return Some(Arc::clone(&self.base_view));
        }
        None
    }
}

impl<D: Database + 'static> Versions for BlockManager<D> {
    fn get_state(&self, blk_id: Id) -> Option<Arc<dyn Chain>> {
        self.parent_view(blk_id)
    }
}
