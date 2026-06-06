// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Safe wrapper over the [`firewood`] embedded trie database (specs 04 §4, R3).
//!
//! Firewood is the on-disk state database for the EVM/SAE path. Unlike the
//! [`crate`]-native [`MerkleDb`](crate::MerkleDb) (a byte-exact Go `x/merkledb`
//! port), this module is a thin, **safe** wrapper over the external `firewood`
//! crate: we do not reimplement its trie, we adapt its API to the shapes the
//! rest of avalanche-rs expects (`ava-types` [`Id`] roots, `SyncDb`).
//!
//! # Hashing modes (compile-time)
//!
//! Firewood's hash function is a *global compile-time* switch
//! ([`firewood_storage::NodeHashAlgorithm`]): with the crate `firewood-ethhash`
//! feature off, firewood hashes with SHA-256 ("MerkleDB" mode, spec 04 §4.1);
//! with it on, firewood hashes with Keccak-256 over an Ethereum MPT/RLP layout
//! ("Ethereum" mode), yielding the EVM state root. We always configure the
//! runtime [`NodeHashAlgorithm`] to
//! [`NodeHashAlgorithm::compile_option`], so it can never disagree with the
//! compiled feature set (firewood rejects a mismatch at open time).
//!
//! - [`sha`] — the [`SyncDb`](crate::sync::SyncDb) implementation over a SHA-256
//!   firewood instance (M1.20).
//! - [`ethhash`] — the Ethereum-state-root view (M1.21, behind `firewood-ethhash`).
//!
//! # Safety / async
//!
//! `#![forbid(unsafe_code)]` still holds for *our* crate: all `unsafe` lives
//! inside the `firewood`/`firewood-storage` crates, which is their concern
//! (04 §4.4). Firewood's API is synchronous and may block on disk I/O, so async
//! call sites must run these calls under `tokio::task::spawn_blocking` or a
//! dedicated thread (04 §1.2); this wrapper deliberately stays synchronous.

use std::path::Path;

use firewood::api::{Db as _, DbView as _, HashKey, Proposal as _};
use firewood::db::{Db, DbConfig, Proposal};
use firewood::manager::RevisionManagerConfig;
use firewood_storage::NodeHashAlgorithm;

use ava_types::id::Id;

pub mod sha;

/// Default number of historical revisions firewood retains.
///
/// This bounds the reorg/state-sync window: `db.revision(root)` can read any of
/// the last `DEFAULT_MAX_REVISIONS` committed roots. Mirrors the Go node's
/// firewood configuration (it keeps a comparable window to serve range/change
/// proofs and to roll back across a reorg).
pub const DEFAULT_MAX_REVISIONS: usize = 256;

/// Errors surfaced by the firewood wrapper.
#[derive(Debug, thiserror::Error)]
pub enum FirewoodError {
    /// An error from the underlying firewood database.
    #[error("firewood error: {0}")]
    Firewood(#[from] firewood::api::Error),

    /// A requested root/revision is not in firewood's retained history.
    #[error("revision not found for the requested root")]
    RevisionNotFound,
}

/// Result alias for the firewood wrapper.
pub type FirewoodResult<T> = core::result::Result<T, FirewoodError>;

/// A batch operation applied to a firewood proposal.
///
/// Mirrors `firewood::api::BatchOp` but owns its bytes so callers can build a
/// batch without lifetime gymnastics. `Put` upserts, `Delete` removes a single
/// key, `DeleteRange` removes every key under a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOp {
    /// Insert or update `key` to `value`.
    Put {
        /// The key to write.
        key: Vec<u8>,
        /// The value to associate with `key`.
        value: Vec<u8>,
    },
    /// Delete a single `key`.
    Delete {
        /// The key to remove.
        key: Vec<u8>,
    },
    /// Delete every key sharing `prefix`.
    DeleteRange {
        /// The prefix whose keys are removed.
        prefix: Vec<u8>,
    },
}

impl BatchOp {
    /// Converts to firewood's owned-bytes batch op.
    fn into_firewood(self) -> firewood::api::BatchOp<Vec<u8>, Vec<u8>> {
        match self {
            BatchOp::Put { key, value } => firewood::api::BatchOp::Put { key, value },
            BatchOp::Delete { key } => firewood::api::BatchOp::Delete { key },
            BatchOp::DeleteRange { prefix } => firewood::api::BatchOp::DeleteRange { prefix },
        }
    }
}

/// Converts a firewood [`HashKey`] (32-byte trie hash) to an [`Id`].
fn id_from_hash(hash: &HashKey) -> Id {
    // `HashKey` (firewood's `TrieHash`) derefs to `&[u8; 32]`; `Id` is also 32
    // bytes, so this is a total, infallible conversion.
    Id::from(**hash)
}

/// The root [`Id`] for an empty firewood trie in the active hashing mode.
///
/// In SHA-256 mode this is [`Id::EMPTY`] (firewood reports `None`); in ethhash
/// mode it is the well-known Ethereum empty-trie root
/// (`0x56e81f17…`), which firewood returns from `default_root_hash()`.
#[must_use]
pub fn empty_root() -> Id {
    use firewood::api::HashKeyExt as _;
    HashKey::default_root_hash()
        .as_ref()
        .map_or(Id::EMPTY, id_from_hash)
}

/// A safe handle to a firewood database.
///
/// Wraps [`firewood::db::Db`]. The database is disk-backed; the `dir` passed to
/// [`FirewoodDb::open`] is created if missing. The hashing mode is fixed at
/// compile time (see the module docs).
pub struct FirewoodDb {
    db: Db,
}

impl FirewoodDb {
    /// Opens (creating if missing) a firewood database at `dir`, retaining
    /// [`DEFAULT_MAX_REVISIONS`] historical revisions.
    ///
    /// # Errors
    /// Returns [`FirewoodError::Firewood`] if firewood fails to open the path or
    /// the configuration is rejected (e.g. a hash-mode mismatch on an existing
    /// database).
    pub fn open(dir: impl AsRef<Path>) -> FirewoodResult<FirewoodDb> {
        FirewoodDb::open_with_revisions(dir, DEFAULT_MAX_REVISIONS)
    }

    /// Opens a firewood database at `dir` retaining `max_revisions` revisions.
    ///
    /// # Errors
    /// Returns [`FirewoodError::Firewood`] on any firewood open/config failure.
    pub fn open_with_revisions(
        dir: impl AsRef<Path>,
        max_revisions: usize,
    ) -> FirewoodResult<FirewoodDb> {
        let manager = RevisionManagerConfig::builder()
            .max_revisions(max_revisions.max(1))
            .build();
        let cfg = DbConfig::builder()
            // Always bind the runtime mode to the compiled feature so firewood
            // never rejects us for a hash-algorithm mismatch.
            .node_hash_algorithm(NodeHashAlgorithm::compile_option())
            .manager(manager)
            .build();
        let db = Db::new(dir.as_ref(), cfg)?;
        Ok(FirewoodDb { db })
    }

    /// The current committed root of the database (the empty-trie root if no
    /// data has been committed).
    #[must_use]
    pub fn root(&self) -> Id {
        self.db.root_hash().as_ref().map_or(Id::EMPTY, id_from_hash)
    }

    /// Borrows the underlying firewood database (crate-internal; used by the
    /// [`SyncDb`](crate::sync::SyncDb) impl in [`sha`]).
    pub(crate) fn db(&self) -> &Db {
        &self.db
    }

    /// Builds a proposal over `ops` *without* committing it.
    ///
    /// The returned [`FirewoodProposal`] exposes the post-application root via
    /// [`FirewoodProposal::root`] — this is the root consensus votes on *before*
    /// the proposal is committed (04 §4.2). Call [`FirewoodProposal::commit`] to
    /// advance the tip, or drop it to discard.
    ///
    /// # Errors
    /// Returns [`FirewoodError::Firewood`] if firewood cannot build the proposal.
    pub fn propose(&self, ops: Vec<BatchOp>) -> FirewoodResult<FirewoodProposal<'_>> {
        let batch: Vec<firewood::api::BatchOp<Vec<u8>, Vec<u8>>> =
            ops.into_iter().map(BatchOp::into_firewood).collect();
        let proposal = self.db.propose(batch)?;
        Ok(FirewoodProposal { proposal })
    }

    /// Reads `key` from the latest committed revision.
    ///
    /// # Errors
    /// Returns [`FirewoodError`] on a firewood read error.
    pub fn get(&self, key: &[u8]) -> FirewoodResult<Option<Vec<u8>>> {
        match self.db.root_hash() {
            Some(root) => self.get_at(&id_from_hash(&root), key),
            None => Ok(None),
        }
    }

    /// Reads `key` as of the committed revision identified by `root`.
    ///
    /// # Errors
    /// Returns [`FirewoodError::RevisionNotFound`] if `root` is no longer in the
    /// retained revision window, or [`FirewoodError::Firewood`] on a read error.
    pub fn get_at(&self, root: &Id, key: &[u8]) -> FirewoodResult<Option<Vec<u8>>> {
        let hash = HashKey::try_from(root.as_bytes().as_slice())
            .map_err(|_| FirewoodError::RevisionNotFound)?;
        let revision = match self.db.revision(hash) {
            Ok(rev) => rev,
            Err(firewood::api::Error::RevisionNotFound { .. }) => {
                return Err(FirewoodError::RevisionNotFound);
            }
            Err(err) => return Err(FirewoodError::Firewood(err)),
        };
        let value = revision.val(key)?;
        Ok(value.map(|v| v.to_vec()))
    }
}

/// A pending firewood proposal (a not-yet-committed trie revision).
///
/// Borrows the [`FirewoodDb`] it was created from. Exposes the post-application
/// root (for consensus to vote on) and the values it would expose, then either
/// commits (advancing the tip) or is dropped (discarding the proposal).
pub struct FirewoodProposal<'db> {
    proposal: Proposal<'db>,
}

impl FirewoodProposal<'_> {
    /// The root [`Id`] this proposal would produce once committed (04 §4.2).
    #[must_use]
    pub fn root(&self) -> Id {
        self.proposal
            .root_hash()
            .as_ref()
            .map_or(Id::EMPTY, id_from_hash)
    }

    /// Reads `key` as it would appear in this (uncommitted) proposal.
    ///
    /// # Errors
    /// Returns [`FirewoodError`] on a firewood read error.
    pub fn get(&self, key: &[u8]) -> FirewoodResult<Option<Vec<u8>>> {
        let value = self.proposal.val(key)?;
        Ok(value.map(|v| v.to_vec()))
    }

    /// Commits this proposal, advancing the database tip to its root.
    ///
    /// # Errors
    /// Returns [`FirewoodError::Firewood`] if firewood rejects the commit.
    pub fn commit(self) -> FirewoodResult<()> {
        self.proposal.commit()?;
        Ok(())
    }
}
