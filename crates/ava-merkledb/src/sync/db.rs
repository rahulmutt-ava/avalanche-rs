// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`SyncDb`] trait (spec 04 §3.7) and [`SyncableTrie`], the in-memory
//! `ava-merkledb` implementation used by the sync client and the proof server.
//!
//! The generic Go `sync.DB[R, C]` interface becomes a Rust trait with two
//! associated proof types so the *same* [`crate::sync::syncer::Syncer`] drives
//! both `ava-merkledb` (SHA-256) and a future Firewood-ethhash backend.
//!
//! [`SyncableTrie`] keeps the materialised key/value set in a [`BTreeMap`] and
//! (re)builds the in-memory [`Trie`] on demand to compute roots and proofs. This
//! mirrors the M1.18 proof port (which takes before/after states directly rather
//! than reading a bounded history ring): the protocol logic and the byte-exact
//! verification semantics are identical; only the storage of historical
//! revisions is simplified. To answer change/range proofs *at a past root*, the
//! trie records a bounded ring of recent committed snapshots keyed by root.

use std::collections::{BTreeMap, VecDeque};

use bytes::Bytes;
use parking_lot::Mutex;

use ava_types::id::Id;

use crate::hashing::DefaultHasher;
use crate::key::{BranchFactor, Key};
use crate::maybe::Maybe;
use crate::proof::{ChangeProof, RangeProof};
use crate::sync::error::{SyncError, SyncResult};
use crate::trie::Trie;

/// Default number of recent committed snapshots retained for serving change/
/// range proofs at a past root (the Go merkledb `HistoryLength` analogue).
pub const DEFAULT_HISTORY_LENGTH: usize = 256;

/// A materialised key/value set (the live state or a historical snapshot).
type KvSet = BTreeMap<Vec<u8>, Vec<u8>>;

/// A trie backend that can serve and apply state-sync proofs (spec 04 §3.7).
///
/// `merkle_root`/`change_proof`/`range_proof`/`verify_change_proof` are the
/// server + verify side; `commit_range_proof`/`commit_change_proof`/`clear` are
/// the client-apply side.
pub trait SyncDb: Send + Sync {
    /// The range-proof type this backend produces/consumes.
    type RangeProof;
    /// The change-proof type this backend produces/consumes.
    type ChangeProof;

    /// Current root of the trie (`Id::EMPTY` if empty).
    ///
    /// # Errors
    /// Returns a [`SyncError`] if the root cannot be computed.
    fn merkle_root(&self) -> SyncResult<Id>;

    /// A change proof for the key changes in `[start, end]` between `start_root`
    /// and `end_root`, capped at `max_len` changes.
    ///
    /// # Errors
    /// Returns [`SyncError::InsufficientHistory`]/[`SyncError::NoEndRoot`] if the
    /// roots aren't in the retained history.
    fn change_proof(
        &self,
        start_root: Id,
        end_root: Id,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        max_len: usize,
    ) -> SyncResult<Self::ChangeProof>;

    /// A range proof for `[start, end]` at `root`, capped at `max_len` pairs.
    ///
    /// # Errors
    /// Returns [`SyncError::InsufficientHistory`] if `root` isn't retained.
    fn range_proof(
        &self,
        root: Id,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        max_len: usize,
    ) -> SyncResult<Self::RangeProof>;

    /// Verifies that applying `p`'s changes over `[start, end]` yields
    /// `expected_end_root`.
    ///
    /// # Errors
    /// Returns [`SyncError::InvalidChangeProof`] on any failure.
    fn verify_change_proof(
        &self,
        p: &Self::ChangeProof,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        expected_end_root: Id,
    ) -> SyncResult<()>;

    /// Applies a verified range proof's pairs to this DB (client side).
    ///
    /// # Errors
    /// Returns a [`SyncError`] if the proof is malformed.
    fn commit_range_proof(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        p: Self::RangeProof,
    ) -> SyncResult<()>;

    /// Applies a verified change proof's changes to this DB (client side).
    ///
    /// # Errors
    /// Returns a [`SyncError`] if the proof is malformed.
    fn commit_change_proof(&self, p: Self::ChangeProof) -> SyncResult<()>;

    /// Removes all key/value pairs from the trie.
    ///
    /// # Errors
    /// Returns a [`SyncError`] if clearing fails.
    fn clear(&self) -> SyncResult<()>;
}

/// In-memory `ava-merkledb` [`SyncDb`] implementation.
///
/// Holds the live key/value set plus a bounded ring of recent committed
/// snapshots (keyed by root) so it can answer range/change proofs at a past
/// root, like the Go merkledb history ring.
pub struct SyncableTrie {
    branch_factor: BranchFactor,
    history_length: usize,
    inner: Mutex<Inner>,
}

struct Inner {
    /// The live key/value set.
    kvs: KvSet,
    /// Recent committed snapshots: (root, key/value set), newest at the back.
    history: VecDeque<(Id, KvSet)>,
}

impl SyncableTrie {
    /// A fresh empty syncable trie with the default branch factor and history.
    #[must_use]
    pub fn new(branch_factor: BranchFactor) -> SyncableTrie {
        SyncableTrie::with_history(branch_factor, DEFAULT_HISTORY_LENGTH)
    }

    /// A fresh empty syncable trie retaining `history_length` past snapshots.
    #[must_use]
    pub fn with_history(branch_factor: BranchFactor, history_length: usize) -> SyncableTrie {
        let st = SyncableTrie {
            branch_factor,
            history_length: history_length.max(1),
            inner: Mutex::new(Inner {
                kvs: BTreeMap::new(),
                history: VecDeque::new(),
            }),
        };
        // Seed the history with the empty snapshot so EMPTY is always serveable.
        st.snapshot();
        st
    }

    /// Builds a trie from an initial key/value set (for tests / the server).
    #[must_use]
    pub fn from_kvs(branch_factor: BranchFactor, kvs: &[(&[u8], &[u8])]) -> SyncableTrie {
        let st = SyncableTrie::new(branch_factor);
        {
            let mut inner = st.inner.lock();
            for (k, v) in kvs {
                inner.kvs.insert(k.to_vec(), v.to_vec());
            }
        }
        st.snapshot();
        st
    }

    /// The branch factor this trie uses.
    #[must_use]
    pub fn branch_factor(&self) -> BranchFactor {
        self.branch_factor
    }

    /// The current live key/value set (sorted).
    #[must_use]
    pub fn key_values(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.inner
            .lock()
            .kvs
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Records the current live state as a new history snapshot keyed by its
    /// root, trimming to `history_length`.
    fn snapshot(&self) {
        let mut inner = self.inner.lock();
        let root = root_of(self.branch_factor, &inner.kvs);
        let kvs = inner.kvs.clone();
        // Replace an existing snapshot for this root (keep newest position).
        inner.history.retain(|(r, _)| *r != root);
        inner.history.push_back((root, kvs));
        while inner.history.len() > self.history_length {
            inner.history.pop_front();
        }
    }

    /// Returns the key/value set at `root` from history, if retained.
    fn kvs_at(&self, root: Id) -> Option<KvSet> {
        if root == Id::EMPTY {
            return Some(BTreeMap::new());
        }
        self.inner
            .lock()
            .history
            .iter()
            .find(|(r, _)| *r == root)
            .map(|(_, kvs)| kvs.clone())
    }
}

/// Computes the root of a key/value set.
fn root_of(branch_factor: BranchFactor, kvs: &KvSet) -> Id {
    let mut trie = Trie::new(branch_factor);
    let hasher = DefaultHasher;
    for (k, v) in kvs {
        trie.apply(Key::from_bytes(k), Maybe::Some(Bytes::copy_from_slice(v)));
    }
    trie.root_id(&hasher)
}

/// Borrowed view of a key/value set as `&[(&[u8], &[u8])]` for proof builders.
fn as_pairs(kvs: &KvSet) -> Vec<(&[u8], &[u8])> {
    kvs.iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect()
}

impl SyncDb for SyncableTrie {
    type RangeProof = RangeProof;
    type ChangeProof = ChangeProof;

    fn merkle_root(&self) -> SyncResult<Id> {
        let inner = self.inner.lock();
        Ok(root_of(self.branch_factor, &inner.kvs))
    }

    fn change_proof(
        &self,
        start_root: Id,
        end_root: Id,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        max_len: usize,
    ) -> SyncResult<Self::ChangeProof> {
        let hasher = DefaultHasher;
        let before = self
            .kvs_at(start_root)
            .ok_or(SyncError::InsufficientHistory)?;
        let after = self.kvs_at(end_root).ok_or(SyncError::NoEndRoot)?;
        let before_pairs = as_pairs(&before);
        let after_pairs = as_pairs(&after);
        ChangeProof::prove(
            self.branch_factor,
            &hasher,
            &before_pairs,
            &after_pairs,
            start,
            end,
            max_len,
        )
        .map_err(SyncError::from)
    }

    fn range_proof(
        &self,
        root: Id,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        max_len: usize,
    ) -> SyncResult<Self::RangeProof> {
        let hasher = DefaultHasher;
        let kvs = self.kvs_at(root).ok_or(SyncError::InsufficientHistory)?;
        let pairs = as_pairs(&kvs);
        RangeProof::prove(self.branch_factor, &hasher, &pairs, start, end, max_len)
            .map_err(SyncError::from)
    }

    fn verify_change_proof(
        &self,
        p: &Self::ChangeProof,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        expected_end_root: Id,
    ) -> SyncResult<()> {
        let hasher = DefaultHasher;
        let inner = self.inner.lock();
        let start_kvs = as_pairs(&inner.kvs);
        p.verify(
            &start_kvs,
            start,
            end,
            expected_end_root,
            self.branch_factor,
            &hasher,
        )
        .map_err(|_| SyncError::InvalidChangeProof)
    }

    fn commit_range_proof(
        &self,
        _start: Option<&[u8]>,
        _end: Option<&[u8]>,
        p: Self::RangeProof,
    ) -> SyncResult<()> {
        {
            let mut inner = self.inner.lock();
            for kv in &p.key_values {
                inner.kvs.insert(kv.key.clone(), kv.value.clone());
            }
        }
        self.snapshot();
        Ok(())
    }

    fn commit_change_proof(&self, p: Self::ChangeProof) -> SyncResult<()> {
        {
            let mut inner = self.inner.lock();
            for kc in &p.key_changes {
                match &kc.value {
                    Maybe::Some(v) => {
                        inner.kvs.insert(kc.key.clone(), v.to_vec());
                    }
                    Maybe::Nothing => {
                        inner.kvs.remove(&kc.key);
                    }
                }
            }
        }
        self.snapshot();
        Ok(())
    }

    fn clear(&self) -> SyncResult<()> {
        {
            let mut inner = self.inner.lock();
            inner.kvs.clear();
        }
        self.snapshot();
        Ok(())
    }
}
