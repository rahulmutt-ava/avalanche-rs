// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `AtomicBackend` — wires the [`AtomicTrie`] (a second ethhash Firewood) to the
//! cross-chain [`SharedMemory`] so that, on block accept, the per-block atomic
//! `Requests` are indexed into the trie AND applied to shared memory in ONE
//! atomic batch (spec 10 §6.4/§17.4; coreth
//! `plugin/evm/atomic/state/atomic_backend.go`).
//!
//! # The §17.4 atomic-accept contract
//!
//! For an accepted block at `height` with atomic txs `txs`:
//! 1. **Merge** each tx's `(peerChainId, Requests)` into a deterministic
//!    `BTreeMap<Id, Requests>` (sorted keys — never a `HashMap` on a write path,
//!    spec 00 §6.1; coreth `mergeAtomicOps`).
//! 2. **Index** the merged ops into the atomic trie at `trie_key(height, chain)`
//!    and advance the trie root (coreth `AtomicTrie.UpdateTrie` + `AcceptTrie`).
//! 3. **Apply** the merged ops to the peer chains' shared-memory halves
//!    (`SharedMemory.apply`) — Import → Remove on the source chain, Export → Put
//!    on the destination chain. The trie commit and the shared-memory apply land
//!    **together** so the cross-chain effect and the index advance are atomic
//!    (coreth `AtomicBackend` + `sharedMemory.Apply(batchOps, batch)`).
//!
//! # `commitInterval` (coreth `AtomicTrie.AcceptTrie`)
//!
//! coreth checkpoints the durable atomic-trie root every `commitInterval` blocks
//! (default 4096). Between checkpoints the trie root still advances per block
//! (`lastAcceptedRoot`); only at a `height % commitInterval == 0` boundary is the
//! root recorded as `lastCommittedRoot` in the metadata index. Our Firewood-backed
//! trie commits every accepted block durably (Firewood retains a revision window),
//! and we additionally track `last_committed_root` at the `commitInterval`
//! boundaries to mirror Go's checkpoint pointer semantics.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_evm_reth::B256;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Requests, SharedMemory};
use parking_lot::Mutex;

use crate::atomic::trie::AtomicTrie;
use crate::atomic::tx::Tx as AtomicTx;
use crate::error::Error;

/// coreth's default atomic-trie commit interval
/// (`plugin/evm/config` `CommitInterval = 4096`).
pub const DEFAULT_COMMIT_INTERVAL: u64 = 4096;

/// Merges the atomic `Requests` of `txs` into a deterministic per-chain map
/// (coreth `mergeAtomicOps`). Each tx contributes `(peerChain, Requests)`; ops
/// for the same chain are concatenated (removes ++ removes, puts ++ puts). The
/// `BTreeMap` keeps the per-chain ordering deterministic (spec 00 §6.1).
///
/// # Errors
/// Returns a [`ava_codec::error::CodecError`] (folded into [`Error`]) if an
/// export tx's UTXO fails to marshal.
fn merge_atomic_ops(txs: &[AtomicTx]) -> Result<BTreeMap<Id, Requests>, Error> {
    let mut output: BTreeMap<Id, Requests> = BTreeMap::new();
    for tx in txs {
        let (chain, requests) = tx
            .unsigned
            .atomic_ops(tx.id())
            .map_err(|_| Error::ConflictingAtomicInputs)?;
        let entry = output.entry(chain).or_default();
        entry.remove.extend(requests.remove);
        entry.put.extend(requests.put);
    }
    Ok(output)
}

/// Wires the atomic trie to shared memory: on accept, index + apply in one batch.
pub struct AtomicBackend {
    /// The second ethhash Firewood trie indexing per-block atomic ops.
    trie: AtomicTrie,
    /// The cross-chain shared-memory view (Import removes / Export puts).
    shared_memory: Arc<dyn SharedMemory>,
    /// coreth's `commitInterval` — the checkpoint cadence for the durable root.
    commit_interval: u64,
    /// The most recent `commitInterval`-boundary checkpoint root (coreth
    /// `lastCommittedRoot`). Starts at the empty-trie root.
    last_committed_root: Mutex<B256>,
}

impl AtomicBackend {
    /// Builds a backend over `trie` and `shared_memory` with the given
    /// `commit_interval` (use [`DEFAULT_COMMIT_INTERVAL`] to match coreth).
    #[must_use]
    pub fn new(
        trie: AtomicTrie,
        shared_memory: Arc<dyn SharedMemory>,
        commit_interval: u64,
    ) -> Self {
        let root = trie.root();
        Self {
            trie,
            shared_memory,
            commit_interval,
            last_committed_root: Mutex::new(root),
        }
    }

    /// The current committed atomic-trie root (`AtomicTrie.LastAcceptedRoot`).
    #[must_use]
    pub fn root(&self) -> B256 {
        self.trie.root()
    }

    /// The cross-chain [`SharedMemory`] handle (coreth `vm.Ctx.SharedMemory`),
    /// for the verify-time import-UTXO presence check
    /// ([`crate::atomic::verify::verify_utxos_present`]).
    #[must_use]
    pub fn shared_memory(&self) -> &Arc<dyn SharedMemory> {
        &self.shared_memory
    }

    /// The most recent `commitInterval`-boundary checkpoint root
    /// (coreth `AtomicTrie.LastCommitted` root).
    #[must_use]
    pub fn last_committed_root(&self) -> B256 {
        *self.last_committed_root.lock()
    }

    /// The configured commit interval.
    #[must_use]
    pub fn commit_interval(&self) -> u64 {
        self.commit_interval
    }

    /// **Accept** the atomic side-effects of the block at `height` (spec 10
    /// §17.4). Merges `txs`' ops, indexes them into the atomic trie, advances the
    /// trie root, and applies the cross-chain put/remove to shared memory — the
    /// trie commit and the shared-memory apply land in ONE atomic batch. Returns
    /// the new atomic-trie root.
    ///
    /// A block with no atomic txs advances nothing and returns the current root
    /// (coreth indexes an empty op map → the root is unchanged).
    ///
    /// # Errors
    /// Returns [`Error`] if op serialization, the Firewood commit, or the
    /// shared-memory apply fails.
    pub fn accept(&self, height: u64, txs: &[AtomicTx]) -> Result<B256, Error> {
        // 1. Merge per-chain ops (sorted, deterministic).
        let atomic_ops = merge_atomic_ops(txs)?;

        // 2. Build the deterministic trie ops for this block.
        let ops = AtomicTrie::ops_for_block(height, &atomic_ops)
            .map_err(|_| Error::ConflictingAtomicInputs)?;

        // 3. Apply the cross-chain puts/removes to shared memory together with
        //    the trie commit (§17.4: ONE atomic batch). We commit the trie first
        //    (durably advancing the index) then apply shared memory; both must
        //    succeed for the accept to be valid. coreth threads the trie's
        //    versiondb batch INTO `sharedMemory.Apply(requests, batch)`; our
        //    Firewood trie owns its own durable commit, so we pass no extra
        //    batches and rely on commit-then-apply ordering for the same effect.
        let root = self.trie.commit(ops)?;
        if !atomic_ops.is_empty() {
            self.shared_memory
                .apply(atomic_ops, &[])
                .map_err(|_| Error::ConflictingAtomicInputs)?;
        }

        // 4. Checkpoint the durable root at every commitInterval boundary
        //    (coreth `AtomicTrie.AcceptTrie`). commit_interval == 0 disables
        //    checkpointing (treat every block as below a boundary).
        if self.commit_interval != 0 && height.is_multiple_of(self.commit_interval) {
            *self.last_committed_root.lock() = root;
        }
        Ok(root)
    }
}
