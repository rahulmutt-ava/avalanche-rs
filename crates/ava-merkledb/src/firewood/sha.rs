// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`SyncDb`] over a SHA-256 ("MerkleDB"-mode) firewood database (M1.20, spec
//! 04 §3.7 + §4).
//!
//! This wires the firewood backend into the *same* state-sync protocol that
//! drives the in-memory [`SyncableTrie`](crate::sync::SyncableTrie), proving the
//! [`SyncDb`] abstraction is backend-agnostic (04 §3.7). The associated proof
//! types are firewood's own [`FrozenRangeProof`]/[`FrozenChangeProof`]; the
//! server/verify/commit methods delegate to firewood, which retains a bounded
//! revision window (see [`FirewoodDb::open`](super::FirewoodDb::open)) covering
//! the reorg/sync window.
//!
//! Firewood's API is synchronous; the [`Syncer`](crate::sync::Syncer) call sites
//! must run these methods under `spawn_blocking`/a dedicated thread (04 §1.2).

use std::num::NonZeroUsize;

use firewood::api::{
    Db as _, DbView as _, FrozenChangeProof, FrozenRangeProof, HashKey, Proposal as _,
};

use ava_types::id::Id;

use crate::firewood::{BatchOp, FirewoodDb};
use crate::sync::{SyncDb, SyncError, SyncResult};

/// Converts an [`Id`] root to a firewood [`HashKey`], mapping [`Id::EMPTY`] to
/// the active mode's empty-trie hash.
fn hash_key(root: Id) -> SyncResult<HashKey> {
    use firewood::api::HashKeyExt as _;
    if root == Id::EMPTY {
        // An empty root: in SHA mode firewood has no committed empty revision to
        // name, so callers should treat this via the empty-trie default.
        return HashKey::default_root_hash().ok_or(SyncError::InsufficientHistory);
    }
    HashKey::try_from(root.as_bytes().as_slice()).map_err(|_| SyncError::InvalidRootHash)
}

/// Optional [`NonZeroUsize`] from a `max_len` cap (`0` means "no cap").
fn opt_limit(max_len: usize) -> Option<NonZeroUsize> {
    NonZeroUsize::new(max_len)
}

impl SyncDb for FirewoodDb {
    type RangeProof = FrozenRangeProof;
    type ChangeProof = FrozenChangeProof;

    fn merkle_root(&self) -> SyncResult<Id> {
        Ok(self.root())
    }

    fn change_proof(
        &self,
        start_root: Id,
        end_root: Id,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        max_len: usize,
    ) -> SyncResult<Self::ChangeProof> {
        let start_hash = hash_key(start_root)?;
        let end_hash = hash_key(end_root)?;
        self.db()
            .change_proof(start_hash, end_hash, start, end, opt_limit(max_len))
            .map_err(map_proof_err)
    }

    fn range_proof(
        &self,
        root: Id,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        max_len: usize,
    ) -> SyncResult<Self::RangeProof> {
        let hash = hash_key(root)?;
        let revision = self
            .db()
            .revision(hash)
            .map_err(|_| SyncError::InsufficientHistory)?;
        revision
            .range_proof(start, end, opt_limit(max_len))
            .map_err(map_proof_err)
    }

    fn verify_change_proof(
        &self,
        p: &Self::ChangeProof,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        expected_end_root: Id,
    ) -> SyncResult<()> {
        let end_hash = hash_key(expected_end_root)?;
        // `verify_change_proof` returns a proposal on success and errors on any
        // structural/root mismatch; we only need the accept/reject outcome here.
        self.db()
            .verify_change_proof(p, end_hash, start, end, None)
            .map(|_proposal| ())
            .map_err(|_| SyncError::InvalidChangeProof)
    }

    fn commit_range_proof(
        &self,
        _start: Option<&[u8]>,
        _end: Option<&[u8]>,
        p: Self::RangeProof,
    ) -> SyncResult<()> {
        // Apply the (already-verified) range proof's key/value pairs as a batch
        // of puts and commit, advancing the tip.
        let ops: Vec<BatchOp> = p
            .key_values()
            .iter()
            .map(|(k, v)| BatchOp::Put {
                key: k.to_vec(),
                value: v.to_vec(),
            })
            .collect();
        self.propose(ops)
            .map_err(|_| SyncError::InvalidRangeProof)?
            .commit()
            .map_err(|_| SyncError::InvalidRangeProof)
    }

    fn commit_change_proof(&self, p: Self::ChangeProof) -> SyncResult<()> {
        // Re-verify against the current tip and commit the resulting proposal.
        let end_hash = self.db().root_hash().ok_or(SyncError::NoEndRoot)?;
        let proposal = self
            .db()
            .verify_change_proof(&p, end_hash, None, None, None)
            .map_err(|_| SyncError::InvalidChangeProof)?;
        proposal.commit().map_err(|_| SyncError::InvalidChangeProof)
    }

    fn clear(&self) -> SyncResult<()> {
        // Delete every key (empty prefix = whole keyspace) and commit.
        self.propose(vec![BatchOp::DeleteRange { prefix: Vec::new() }])
            .map_err(|e| SyncError::Merkle(crate::error::Error::Database(e.to_string())))?
            .commit()
            .map_err(|e| SyncError::Merkle(crate::error::Error::Database(e.to_string())))
    }
}

/// Maps firewood proof errors to the sync error model, distinguishing a
/// missing-history root from a generic failure.
fn map_proof_err(err: firewood::api::Error) -> SyncError {
    match err {
        firewood::api::Error::RevisionNotFound { .. }
        | firewood::api::Error::StartRevisionNotFound { .. } => SyncError::InsufficientHistory,
        firewood::api::Error::EndRevisionNotFound { .. } => SyncError::NoEndRoot,
        _ => SyncError::InvalidRangeProof,
    }
}
