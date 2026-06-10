// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Restart recovery: rebuild the three SAE frontiers (A/E/S) from disk after a
//! crash + restart, with **no trust in in-memory state** (specs/11 §1.4,
//! specs/27 §3 crash-point C6 / §5.4).
//!
//! Faithful port of `vms/saevm/sae/recovery.go`
//! (`recovery.{lastCommittedBlock,executeAllAccepted,consensusCriticalBlocks}`)
//! and the recovery block of `vms/saevm/sae/vm.go::New`.
//!
//! # The procedure (specs/11 §1.4)
//!
//! On startup the VM reconstructs all three frontiers from disk:
//!
//! 1. **`last_committed_block`** — the highest height whose post-execution state
//!    root was durably committed to the trie DB
//!    (`saedb::last_height_with_execution_root_committed`, driven by the commit
//!    interval / archival policy). That block's execution artefacts are restored
//!    from disk; below it, no in-memory state is needed.
//! 2. **`execute_all_accepted`** (rebuilds **E**) — re-enqueue every
//!    accepted-but-not-executed canonical block (from the last committed height
//!    up to the head) and re-execute each from disk, then take the tip as
//!    `LastExecuted`. Re-execution from the last committed root reproduces the
//!    **exact same** post-state roots (execution is pure — no wall-clock, no
//!    map-order; specs/11 §6.1), so the trie commit cadence lands on the same
//!    heights.
//! 3. **`consensus_critical_blocks`** (rebuilds **S**) — walk back from the
//!    executed tip over the accepted blocks (from `LastExecuted` back through
//!    `LastSettled`), marking settled every block whose execution gas-time is
//!    `<= BlockTime − Tau`. The walked window `[S, A]` becomes the
//!    consensus-critical set.
//!
//! Finally `LastAccepted == LastExecuted == head` and the preference is the head
//! (Go `vm.go::New`: `vm.last.accepted.Store(head)` / `vm.preference.Store(head)`).
//!
//! # The persistence seam
//!
//! `ava-saevm-core` carries no Firewood / saedb dependency, so the durable disk
//! is abstracted behind [`RecoverySource`]: the synchronous floor, the head
//! height, the last-committed height (the `saedb` rounding), the canonical eth
//! block at a height, and the height-indexed committed [`ExecutionResults`]. The
//! real implementation (the cchain harness, M7.23) reads rawdb canonical hashes
//! and the `saedb` execution-results table; the recovery tests model it with an
//! in-memory table. This is the same object-safe-seam pattern the VM lifecycle
//! uses for the hook builder and executor (M7.18).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use ava_evm_reth::{B256, RethBlock, SealedBlock};
use ava_saevm_blocks::{Block, last_to_settle_at};
use ava_saevm_params::TAU;
use ava_saevm_types::ExecutionResults;

use crate::block_handle::SaeBlock;
use crate::frontier::Frontier;

/// A failure of the [`recover`] procedure.
#[derive(Debug, thiserror::Error)]
pub enum RecoverError {
    /// The canonical chain had a gap: no block body is durable at this height,
    /// yet the head is higher. A consistent disk never produces this — it
    /// signals corruption (the accepted bodies are written before the pointer
    /// advances; specs/27 §2.4 D-step).
    #[error("missing canonical block at height {0}")]
    MissingCanonicalBlock(u64),

    /// The committed execution results are missing for a height that must have
    /// been executed (the `last_committed_block` restore step).
    #[error("missing execution results at height {0}")]
    MissingExecutionResults(u64),

    /// Reconstructing a canonical block from its persisted body failed (a
    /// parent-hash / height-incrementing inconsistency in the durable chain).
    #[error("rebuilding canonical block at height {height}: {source}")]
    Rebuild {
        /// The height whose reconstruction failed.
        height: u64,
        /// The underlying block-lifecycle error.
        source: ava_saevm_blocks::Error,
    },

    /// Restoring a block's execution artefacts / settled state failed.
    #[error("restoring block at height {height}: {source}")]
    Restore {
        /// The height whose restore failed.
        height: u64,
        /// The underlying block-lifecycle error.
        source: ava_saevm_blocks::Error,
    },
}

/// The durable disk a [`recover`] reads — abstracted so `ava-saevm-core` needs
/// no Firewood / saedb dependency (the cchain harness supplies the real reader
/// in M7.23; tests supply an in-memory table). Mirrors the rawdb + saedb reads
/// the Go `recovery` struct performs.
pub trait RecoverySource: Send + Sync {
    /// The synchronous (genesis / last pre-SAE) block — the floor all three
    /// frontiers can never fall below. It is already self-settled / self-executed
    /// (`Block::mark_synchronous`). Mirrors Go `recovery.lastSynchronous`.
    fn last_synchronous(&self) -> Arc<Block>;

    /// The head height: the highest accepted (canonical) block on disk. Equal to
    /// the synchronous floor's height when the chain never advanced. Mirrors the
    /// highest key of `rawdb` canonical hashes.
    fn head_height(&self) -> u64;

    /// The highest height whose post-execution state root was durably committed
    /// (`saedb::last_height_with_execution_root_committed`). Recovery re-executes
    /// from here to the head. Always `>= last_synchronous().height()`.
    fn last_committed_height(&self) -> u64;

    /// The canonical (accepted) eth block body at `height`, if durable. Mirrors
    /// Go `canonicalBlock(db, num)`.
    fn canonical_eth_block(&self, height: u64) -> Option<SealedBlock<RethBlock>>;

    /// The committed [`ExecutionResults`] for the block at `height`, if durable.
    /// Mirrors restoring from the `saedb` execution-results table.
    fn execution_results(&self, height: u64) -> Option<ExecutionResults>;
}

/// The reconstructed consensus state produced by [`recover`]: the three
/// frontiers (A/E/S) plus the rebuilt in-memory block store + canonical height
/// index the VM seeds itself with. The caller (the cchain harness, M7.23)
/// installs these into a fresh [`Vm`](crate::Vm) — recovery itself constructs no
/// VM (it has no builder/executor seam), it only rebuilds the disk-derived
/// consensus state.
pub struct Recovered {
    /// The three monotonic frontiers + the consensus-critical `[S, A]` map,
    /// rebuilt purely from disk.
    pub frontier: Frontier,
    /// Every consensus-critical block, keyed by hash — the VM's block store seed
    /// (the accepted blocks consensus may request; specs/11 §1.2).
    pub blocks: HashMap<B256, SaeBlock>,
    /// Canonical (accepted) height → block hash — the VM's height-index seed.
    pub height_index: HashMap<u64, B256>,
    /// The head (== `LastExecuted` == `LastAccepted`) block; the VM seeds its
    /// preference with this (Go `vm.preference.Store(head)`).
    pub head: Arc<Block>,
}

/// Rebuilds the three SAE frontiers (A/E/S) from `src` after a restart (specs/11
/// §1.4). See the module docs for the procedure. Re-execution from the last
/// committed root is pure (specs/11 §6.1), so the reconstructed frontiers +
/// post-state roots are identical to those the VM held before the crash
/// (specs/11 §10 invariant 7).
///
/// `async` because step 2 awaits each re-enqueued block's execution
/// (`wait_until_executed`), matching Go's `executeAllAccepted` /
/// `last.WaitUntilExecuted(ctx)`.
///
/// # Errors
/// [`RecoverError::MissingCanonicalBlock`] on a gap in the canonical chain;
/// [`RecoverError::MissingExecutionResults`] if a must-be-executed height has no
/// committed results; [`RecoverError::Rebuild`] / [`RecoverError::Restore`] on a
/// block-lifecycle inconsistency.
pub async fn recover<S: RecoverySource + ?Sized>(src: &S) -> Result<Recovered, RecoverError> {
    let last_synchronous = src.last_synchronous();
    let sync_height = last_synchronous.height();
    let head_height = src.head_height();

    // ===== (1) last committed block =====================================
    // The highest height whose execution root is durable. It is the boundary
    // between blocks whose post-state is *restored* from the committed trie
    // (`<= last_committed_height`) and blocks that are *re-executed* from there
    // (`> last_committed_height`). Both reproduce identical roots — re-execution
    // is pure (specs/11 §6.1) — so the boundary does not affect the result, only
    // the work. The synchronous floor is always its own committed block.
    let last_committed_height = src.last_committed_height().max(sync_height);
    // A consistent disk never commits past the head it has accepted.
    debug_assert!(
        last_committed_height <= head_height,
        "last committed height {last_committed_height} exceeds head {head_height}",
    );

    // ===== (2) execute_all_accepted (rebuilds E) ========================
    // Reconstruct the FULL accepted canonical chain `[sync+1, head]` with parent
    // linkage, restoring each block's execution artefacts from disk. The tip
    // becomes `LastExecuted`.
    //
    // The whole ancestry — not just `(last_committed, head]` — must be
    // reconstructed: the settlement walk-back (step 3) chases `parent_block()`
    // pointers from the head down to `LastSettled`, and `S` can sit *below*
    // `last_committed_height` (settlement lags execution by `Tau` of gas-time).
    // If only `[last_committed, head]` were linked, the walk-back would dead-end
    // at the last committed block's `None` parent and `S` could never recover
    // past it (e.g. under an archival cadence where `last_committed == head`).
    //
    // We track every reconstructed block in a height-indexed store so the
    // settlement walk-back reuses the same `Arc<Block>` (identity) and the VM's
    // block store + canonical height index can be seeded.
    let mut by_height: HashMap<u64, Arc<Block>> = HashMap::new();
    by_height.insert(sync_height, Arc::clone(&last_synchronous));

    let mut head = Arc::clone(&last_synchronous);
    let mut next_height = sync_height.saturating_add(1);
    while next_height <= head_height {
        let block = rebuild_canonical(src, next_height, Some(Arc::clone(&head)))?;
        // Restore the committed execution output onto the block. For
        // `next_height <= last_committed_height` this is the durable trie root;
        // above it, the executor reactor (M7.26) performs the EVM re-run — in the
        // pure-replay model the committed results ARE that output, so both paths
        // restore identical artefacts. `wait_until_executed` resolves immediately
        // since `restore_executed` fires the notification.
        restore_executed(src, &block)?;
        block.wait_until_executed().await;

        by_height.insert(next_height, Arc::clone(&block));
        head = Arc::clone(&block);
        next_height = next_height.saturating_add(1);
    }

    // ===== (3) consensus_critical_blocks (rebuilds S) ===================
    // Walk back from the executed tip over the accepted blocks, marking settled
    // every ancestor whose execution gas-time is `<= BlockTime(head) − Tau` (the
    // Tau settlement instant). The walked `[S, A]` window is consensus-critical.
    let frontier = Frontier::new(Arc::clone(&last_synchronous));

    // The frontier is constructed at the synchronous floor; advance E then A to
    // the head (increasing-height advances; the genesis floor is already set).
    advance_chain_executed(&frontier, &by_height, sync_height, head.height());
    advance_chain_accepted(&frontier, &by_height, sync_height, head.height());

    // Settlement: the settle instant is `BlockTime(head) − Tau` (saturating at
    // the epoch), exactly as the live `settle` driver computes it. `head` is the
    // newest accepted block, so it dictates the settlement frontier on recovery
    // (Go `consensusCriticalBlocks`'s `extend(exec.LastExecuted())`).
    let settle_at = head
        .timestamp()
        .checked_sub(TAU)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if let Some(parent) = head.parent_block() {
        // `last_to_settle_at` returns the last ancestor provably settled by
        // `settle_at` (or the synchronous floor when execution lagged — which
        // cannot happen here since every accepted block was re-executed above).
        let (candidate, _known) =
            last_to_settle_at(settle_at, &parent).map_err(|e| RecoverError::Restore {
                height: parent.height(),
                source: e,
            })?;
        if let Some(candidate) = candidate {
            // Mark every block in `(current S, candidate]` settled in increasing
            // height (the frontier's `advance_settled` evicts below-S blocks from
            // the consensus-critical map). Reuse the live settle range walk.
            mark_settled_up_to(&frontier, &candidate)?;
        }
    }

    // ===== assemble the VM seed =========================================
    // The block store + canonical height index hold every consensus-critical
    // block (the `[S, A]` window). Blocks below S were evicted from the frontier
    // map by `advance_settled`; we mirror that by only seeding the retained set.
    let mut blocks = HashMap::new();
    let mut height_index = HashMap::new();
    let settled_height = frontier.last_settled().height();
    for (h, b) in &by_height {
        if *h >= settled_height {
            blocks.insert(b.hash(), SaeBlock::new(Arc::clone(b)));
            height_index.insert(*h, b.hash());
        }
    }

    Ok(Recovered {
        frontier,
        blocks,
        height_index,
        head,
    })
}

/// Reconstructs the canonical block at `height` from its persisted eth body,
/// attaching `parent` linkage. Mirrors Go `recovery.newCanonicalBlock`.
fn rebuild_canonical<S: RecoverySource + ?Sized>(
    src: &S,
    height: u64,
    parent: Option<Arc<Block>>,
) -> Result<Arc<Block>, RecoverError> {
    let eth = src
        .canonical_eth_block(height)
        .ok_or(RecoverError::MissingCanonicalBlock(height))?;
    // The last-settled pointer is reconstructed by the settlement walk-back
    // (step 3) via `mark_settled`; recovery seeds ancestry with `None` here and
    // lets the walk re-derive S, matching Go (`newCanonicalBlock(..., nil)`).
    let block =
        Block::new(eth, parent, None).map_err(|source| RecoverError::Rebuild { height, source })?;
    Ok(Arc::new(block))
}

/// Restores `block` to the executed state from its committed [`ExecutionResults`]
/// (mirrors Go `Block.RestoreExecutionArtefacts`). Fires the executed
/// notification so a concurrent `wait_until_executed` resolves.
fn restore_executed<S: RecoverySource + ?Sized>(
    src: &S,
    block: &Arc<Block>,
) -> Result<(), RecoverError> {
    let height = block.height();
    let results = src
        .execution_results(height)
        .ok_or(RecoverError::MissingExecutionResults(height))?;
    block
        .restore_execution_artefacts(results)
        .map_err(|source| RecoverError::Restore { height, source })
}

/// Advances `LastExecuted` to every block in `[from+1, to]` in increasing
/// height (the frontier ignores stale advances, so passing the whole chain is
/// safe). The synchronous floor at `from` is already E at construction.
fn advance_chain_executed(
    frontier: &Frontier,
    by_height: &HashMap<u64, Arc<Block>>,
    from: u64,
    to: u64,
) {
    let mut h = from.saturating_add(1);
    while h <= to {
        if let Some(b) = by_height.get(&h) {
            frontier.advance_executed(b);
        }
        h = h.saturating_add(1);
    }
}

/// Advances `LastAccepted` to every block in `[from+1, to]` in increasing height
/// (inserting each into the consensus-critical map). The synchronous floor at
/// `from` is already A at construction.
fn advance_chain_accepted(
    frontier: &Frontier,
    by_height: &HashMap<u64, Arc<Block>>,
    from: u64,
    to: u64,
) {
    let mut h = from.saturating_add(1);
    while h <= to {
        if let Some(b) = by_height.get(&h) {
            frontier.advance_accepted(b);
        }
        h = h.saturating_add(1);
    }
}

/// Marks every block in `(current S, candidate]` settled in increasing height,
/// advancing the frontier's `LastSettled` pointer and evicting the below-S
/// blocks from the consensus-critical map (Go `extend` + the `bMap` build). The
/// walk uses parent-pointer ancestry from `candidate` down to the current S.
fn mark_settled_up_to(frontier: &Frontier, candidate: &Arc<Block>) -> Result<(), RecoverError> {
    let current = frontier.last_settled();
    if candidate.height() <= current.height() {
        return Ok(());
    }
    let range = ava_saevm_blocks::Range::between(Some(current), Some(Arc::clone(candidate)));
    for block in range.iter() {
        // Already-settled blocks (e.g. the synchronous floor) are skipped.
        if block.settled() {
            continue;
        }
        block
            .mark_settled(None)
            .map_err(|source| RecoverError::Restore {
                height: block.height(),
                source,
            })?;
        frontier.advance_settled(block);
    }
    Ok(())
}
