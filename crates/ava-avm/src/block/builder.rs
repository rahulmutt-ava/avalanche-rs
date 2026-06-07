// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain (AVM) block builder (specs 09 §7.1).
//!
//! Mirrors the P-Chain builder (`crates/ava-platformvm/src/block/builder/mod.rs`)
//! but simpler: the X-Chain has only `StandardBlock`, no reward/proposal/
//! advance-time machinery, no stakers. The single entry point is [`build_block`]:
//! a free function that takes a candidate tx slice in mempool (FIFO) order,
//! verifies each against a running [`Diff`], packs those that pass into a
//! `StandardBlock`, and returns an error if there is nothing to build.
//!
//! ## Packing pipeline (port of Go `buildBlock` / `packDecisionTxs`)
//!
//! 1. Lay a fresh `Diff` over `parent_state`.
//! 2. Drain candidate txs IN ORDER; for each:
//!    - Run `SyntacticVerifier` (stateless structural checks).
//!    - Run `SemanticVerifier` against the running `Diff` (detects double-spends
//!      against already-packed txs — the diff already records their consumed UTXOs).
//!    - Run `Executor::execute` (mutates the diff; packed state accumulates).
//!    - On any verification failure, **record** the tx id + reason in `dropped`
//!      and emit a `tracing::warn!`; then continue.
//! 3. Stop packing once cumulative serialized tx bytes would exceed
//!    [`TARGET_BLOCK_SIZE`] (128 KiB); always pack at least the first tx.
//! 4. If no txs packed, return [`Error::NoPendingBlocks`].
//! 5. Build `StandardBlock { parent_id, height = parent_height + 1,
//!    time = max(parent_time, now), txs }` and initialize it via the codec.
//! 6. Return [`BuildBlockOutput`] — the packed block plus the list of dropped
//!    (tx_id, reason) pairs (mirrors Go's logged-drops + dropped-LRU recording).

use std::sync::Arc;
use std::time::SystemTime;

use ava_codec::manager::Manager;
use ava_types::id::Id;

use crate::block::Block;
use crate::block::standard_block::StandardBlock;
use crate::error::{Error, Result};
use crate::fx::dispatch::Dispatch;
use crate::state::chain::Chain;
use crate::state::diff::Diff;
use crate::txs::Tx;
use crate::txs::executor::backend::Backend;
use crate::txs::executor::semantic::SemanticVerifier;
use crate::txs::executor::syntactic::SyntacticVerifier;
use crate::txs::executor::{Executor, ExecutorOutputs};

/// `targetBlockSize` — the soft cap (in bytes of serialized txs) a standard
/// block packs before stopping (Go `builder.targetBlockSize`, 128 KiB).
///
/// Mirrors `crates/ava-platformvm/src/block/builder/mod.rs` `TARGET_BLOCK_SIZE`.
pub const TARGET_BLOCK_SIZE: usize = 128 * 1024;

/// The result of a successful [`build_block`] call.
///
/// Carries both the assembled block and the list of txs that were **dropped**
/// during the packing pipeline (failed syntactic/semantic verify or execution),
/// together with the reason for each drop. The Go X-Chain builder logs each
/// dropped tx and records it in a "dropped" LRU; this struct makes that
/// information observable to callers instead of silently discarding it.
#[derive(Debug)]
pub struct BuildBlockOutput {
    /// The assembled `StandardBlock`.
    pub block: Block,
    /// Txs that were rejected during packing, in the order they were encountered.
    ///
    /// Each entry is `(tx_id, reason)` — the tx id that was dropped and the
    /// first verification/execution error that caused the drop.
    pub dropped: Vec<(Id, Error)>,
}

/// Parameters for [`build_block`], bundled to avoid the too-many-arguments lint.
pub struct BuildBlockParams<'a> {
    /// The codec used to marshal the block.
    pub codec: &'a Manager,
    /// The parent block's id.
    pub parent_id: Id,
    /// The parent block's height (`new block height = parent_height + 1`).
    pub parent_height: u64,
    /// The parent block's wall-clock time (Unix seconds, as `SystemTime`).
    pub parent_time: SystemTime,
    /// The current wall-clock time from the VM's clock.
    pub now: SystemTime,
    /// The parent state as an `Arc<dyn Chain>` (required by `Diff::new_on`).
    ///
    /// Callers supply `BlockManager::get_state(parent_id)` or `state.snapshot()`.
    pub parent_state: Arc<dyn Chain>,
    /// The tx executor backend (chain ids, fees, fx count).
    pub backend: &'a Backend,
    /// The fx dispatch table.
    pub dispatch: &'a Dispatch,
    /// Candidate txs in FIFO mempool order to pack.
    pub candidate_txs: Vec<Tx>,
}

/// `buildBlock` — the AVM block builder (specs 09 §7.1, Go `vm.buildBlock`).
///
/// Verifies each candidate tx against a running [`Diff`] over `params.parent_state`,
/// packs those that pass under the [`TARGET_BLOCK_SIZE`] byte cap, and assembles
/// a `StandardBlock`.
///
/// The block `time` field is `max(parent_time, now)` in Unix seconds (monotonic
/// clamping, specs 09 §7.1).
///
/// Dropped txs (those that fail syntactic/semantic verify or execution) are
/// returned in [`BuildBlockOutput::dropped`] alongside the packed block, and a
/// `tracing::warn!` is emitted per drop — mirroring Go's logged-drops + mempool
/// dropped-LRU recording.
///
/// # Errors
/// - [`Error::NoPendingBlocks`] — no txs passed verification (nothing to build).
/// - [`Error::Codec`] — block initialization (marshaling) failed.
/// - [`Error::MissingParentState`] — diff construction failed.
pub fn build_block(params: BuildBlockParams<'_>) -> Result<BuildBlockOutput> {
    let BuildBlockParams {
        codec,
        parent_id,
        parent_height,
        parent_time,
        now,
        parent_state,
        backend,
        dispatch,
        candidate_txs,
    } = params;

    // Lay a fresh diff over the parent state for tx verification.
    let mut diff = Diff::new_on(parent_state)?;

    let mut packed: Vec<Tx> = Vec::new();
    let mut dropped: Vec<(Id, Error)> = Vec::new();
    let mut cumulative_bytes: usize = 0;

    for tx in candidate_txs {
        // Stop packing once the byte cap is reached (always pack at least one).
        let tx_bytes = tx.bytes().len();
        let next_bytes = cumulative_bytes.saturating_add(tx_bytes);
        if !packed.is_empty() && next_bytes > TARGET_BLOCK_SIZE {
            break;
        }

        let tx_id = tx.id();

        // 1. Syntactic verify (stateless).
        if let Err(e) = SyntacticVerifier::new(backend, &tx).verify() {
            tracing::warn!(tx_id = %tx_id, reason = %e, "build_block: dropping tx (syntactic verify failed)");
            dropped.push((tx_id, e));
            continue;
        }

        // 2. Semantic verify (stateful — reads from the running diff, so
        //    double-spends against already-packed txs are detected here).
        if let Err(e) =
            SemanticVerifier::new(backend, &diff, &tx, dispatch, backend.fee_asset_id).verify()
        {
            tracing::warn!(tx_id = %tx_id, reason = %e, "build_block: dropping tx (semantic verify failed)");
            dropped.push((tx_id, e));
            continue;
        }

        // 3. Execute — mutates the diff; state accumulates for the next tx.
        // The atomic-memory requests from ExecutorOutputs are intentionally
        // discarded here; they are recomputed at verify/accept time.
        let ExecutorOutputs { .. } = match Executor::execute(&tx.unsigned, tx_id, &mut diff) {
            Ok(out) => out,
            Err(e) => {
                tracing::warn!(tx_id = %tx_id, reason = %e, "build_block: dropping tx (execute failed)");
                dropped.push((tx_id, e));
                continue;
            }
        };

        cumulative_bytes = next_bytes;
        packed.push(tx);
    }

    if packed.is_empty() {
        return Err(Error::NoPendingBlocks);
    }

    let height = parent_height.saturating_add(1);
    let time_secs = unix_secs(now.max(parent_time));

    let block = StandardBlock::new_block(codec, parent_id, height, time_secs, packed)
        .map_err(Error::Codec)?;

    Ok(BuildBlockOutput { block, dropped })
}

/// Seconds since the Unix epoch for `t` (saturating; `0` for pre-epoch).
fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
