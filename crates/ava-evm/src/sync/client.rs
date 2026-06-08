// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The state-sync **client** (G8, spec 10 §10 / §17.9): reconstruct a Firewood
//! trie from leaf (range-proof) responses, verify each proof against the target
//! root, and confirm the reconstructed root equals the target.
//!
//! Works uniformly over any [`LeafServer`] — the EVM-state server
//! ([`crate::sync::server::EvmStateSyncServer`]) or a standalone atomic-trie
//! source ([`crate::sync::server::RangeProofSource`]) — so the same client drives
//! both the EVM-state and the atomic-trie sync paths.
//!
//! # Reconstruction contract
//!
//! 1. Request leaves in ascending key order, starting from the open lower bound.
//! 2. **Verify** each served `FrozenRangeProof` against the target root (the
//!    cryptographic anchor: a tampered leaf changes the reconstructed hash and
//!    fails `verify_range_proof`).
//! 3. Apply the verified leaves into a fresh Firewood-ethhash `Db` and commit.
//! 4. After the last range, assert the committed root equals the target root
//!    (the Go `CommitRangeProof` / `VerifyAndCommitRangeProof` end state). A
//!    mismatch is a hard error (the server served a different trie than claimed).

use std::path::Path;
use std::sync::Arc;

use ava_evm_reth::{
    Account, Address, B256, EMPTY_ROOT_HASH, ProviderError, ProviderResult, StorageValue,
};
use firewood::api::{Db as _, DbView as _, HashKey, Proposal as _};
use firewood::db::{BatchOp, Db, DbConfig};
use firewood::manager::RevisionManagerConfig;
use firewood_storage::NodeHashAlgorithm;

use crate::error::{Error, Result};
use crate::state::{FirewoodOps, account_key, decode_rlp_account, decode_rlp_u256, storage_key};
use crate::sync::server::{LeafServer, verify_proof_bytes};
use crate::sync::{LeafsRequest, MAX_KEY_VALUES_LIMIT, MaybeBytes};

/// Number of historical revisions the client's reconstructed trie retains.
const MAX_REVISIONS: usize = 256;

/// The per-request leaf cap the client asks for (the protocol max). The server
/// caps at the same value (coreth `MaxKeyValuesLimit`).
const REQUEST_KEY_LIMIT: u32 = MAX_KEY_VALUES_LIMIT;

/// A generous per-response byte budget (advisory; Firewood truncates by key
/// count, the proof verifies regardless).
const REQUEST_BYTES_LIMIT: u32 = 4 * 1024 * 1024;

/// Reconstructs a Firewood-ethhash trie from leaf (range-proof) responses and
/// verifies its root against a target (spec 10 §10). Owns a fresh on-disk
/// Firewood `Db`; after [`StateSyncClient::sync_from`] succeeds the trie holds
/// exactly the target revision's key/value set.
pub struct StateSyncClient {
    /// The fresh Firewood-ethhash trie being reconstructed.
    db: Db,
    /// The target root the reconstruction must reproduce.
    target: B256,
}

impl StateSyncClient {
    /// Opens (creating) a fresh Firewood-ethhash trie at `dir` to reconstruct the
    /// state at `target`.
    ///
    /// # Errors
    /// Returns [`Error`] if Firewood fails to open the path or rejects the config.
    pub fn open(dir: impl AsRef<Path>, target: B256) -> Result<StateSyncClient> {
        let manager = RevisionManagerConfig::builder()
            .max_revisions(MAX_REVISIONS)
            .build();
        let cfg = DbConfig::builder()
            .node_hash_algorithm(NodeHashAlgorithm::compile_option())
            .manager(manager)
            .build();
        let db = Db::new(dir.as_ref(), cfg).map_err(|e| Error::Provider(map_fw_err(e)))?;
        Ok(StateSyncClient { db, target })
    }

    /// The current committed root of the reconstructed trie (the ethhash
    /// empty-trie root before any leaves are applied).
    #[must_use]
    pub fn root(&self) -> B256 {
        self.db
            .root_hash()
            .map_or(EMPTY_ROOT_HASH, |h| B256::from_slice(h.as_ref()))
    }

    /// Drives the leaf protocol against `server`: requests ascending key ranges,
    /// verifies each range proof against the target root, applies the leaves, and
    /// finally asserts the reconstructed root equals the target.
    ///
    /// # Errors
    /// Returns [`Error`] if a served proof fails to verify against the target, a
    /// Firewood apply/commit fails, or the reconstructed root does not match.
    pub fn sync_from(&mut self, server: &dyn LeafServer) -> Result<()> {
        let mut all_ops: FirewoodOps = Vec::new();
        let mut start = MaybeBytes::nothing();

        loop {
            let req = LeafsRequest {
                root: self.target,
                start: start.clone(),
                end: MaybeBytes::nothing(),
                key_limit: REQUEST_KEY_LIMIT,
                bytes_limit: REQUEST_BYTES_LIMIT,
            };
            let resp = server.handle_leafs(&req)?;

            // Cryptographically verify the served proof against the target root
            // for the requested bounds (anchors the leaves; tamper -> error).
            verify_proof_bytes(&resp.proof, self.target, start.as_bytes(), None)?;

            let returned = resp.keys.len();
            for (key, value) in resp.keys.iter().zip(resp.vals.iter()) {
                all_ops.push(BatchOp::Put {
                    key: key.clone(),
                    value: value.clone(),
                });
            }

            // The range is complete when the server returned fewer than the cap
            // (no truncation) or nothing at all.
            if returned < (REQUEST_KEY_LIMIT.min(MAX_KEY_VALUES_LIMIT) as usize) || returned == 0 {
                break;
            }
            // Otherwise continue strictly after the last returned key.
            match resp.keys.last() {
                Some(last) => {
                    let mut next = last.clone();
                    next.push(0); // smallest key strictly greater than `last`
                    start = MaybeBytes::some(next);
                }
                None => break,
            }
        }

        // Apply the verified leaves and commit the reconstructed trie.
        let proposal = self
            .db
            .propose(all_ops)
            .map_err(|e| Error::Provider(map_fw_err(e)))?;
        proposal
            .commit()
            .map_err(|e| Error::Provider(map_fw_err(e)))?;

        // The reconstructed root MUST equal the target (the Go end state of
        // `VerifyAndCommitRangeProof`).
        let got = self.root();
        if got != self.target {
            return Err(Error::Provider(ProviderError::Database(
                ava_evm_reth::DatabaseError::Other(format!(
                    "state-sync root mismatch: reconstructed {got}, target {}",
                    self.target
                )),
            )));
        }
        Ok(())
    }

    /// A read view pinned at the reconstructed (target) revision.
    ///
    /// # Errors
    /// Returns [`Error`] if the reconstructed revision is not retained.
    pub fn view(&self) -> Result<SyncedView> {
        let hash = HashKey::try_from(self.target.as_slice())
            .map_err(|_| Error::Provider(ProviderError::StateForHashNotFound(self.target)))?;
        let rev = self
            .db
            .revision(hash)
            .map_err(|_| Error::Provider(ProviderError::StateForHashNotFound(self.target)))?;
        Ok(SyncedView { rev })
    }
}

/// A read view over the reconstructed trie at the target revision. Exposes raw
/// key reads (used by the atomic-trie `ApplyToSharedMemory` walk) and decoded
/// EVM-account/storage reads (used to confirm reconstruction).
pub struct SyncedView {
    /// The pinned reconstructed revision.
    rev: Arc<<Db as firewood::api::Db>::Historical>,
}

impl SyncedView {
    /// The raw value at `key` in the reconstructed trie, or `None`.
    ///
    /// # Errors
    /// Returns [`Error`] on a Firewood read failure.
    pub fn raw_get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self
            .rev
            .val(key)
            .map_err(|e| Error::Provider(map_fw_err(e)))?
            .map(|v| v.to_vec()))
    }

    /// All `(key, value)` entries in the reconstructed trie, ascending. Collected
    /// eagerly (the trie is the just-synced revision; sizes are bounded by the
    /// sync pivot).
    ///
    /// # Errors
    /// Returns [`Error`] on a Firewood iteration failure.
    pub fn iter_entries(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut out = Vec::new();
        let iter = self
            .rev
            .iter()
            .map_err(|e| Error::Provider(map_fw_err(e)))?;
        for item in iter {
            let (k, v) = item.map_err(|e| {
                Error::Provider(ProviderError::Database(ava_evm_reth::DatabaseError::Other(
                    e.to_string(),
                )))
            })?;
            out.push((k.to_vec(), v.to_vec()));
        }
        Ok(out)
    }

    /// The decoded EVM account at `addr` in the reconstructed trie, or `None`.
    ///
    /// # Errors
    /// Returns a [`ProviderError`] on a read or RLP-decode failure.
    pub fn basic_account(&self, addr: &Address) -> ProviderResult<Option<Account>> {
        match self.read(&account_key(addr))? {
            Some(rlp) => Ok(Some(decode_rlp_account(&rlp)?)),
            None => Ok(None),
        }
    }

    /// The decoded EVM storage slot value at `(addr, slot)`, or `None`.
    ///
    /// # Errors
    /// Returns a [`ProviderError`] on a read or RLP-decode failure.
    pub fn storage(&self, addr: &Address, slot: &B256) -> ProviderResult<Option<StorageValue>> {
        match self.read(&storage_key(addr, slot))? {
            Some(rlp) => Ok(Some(decode_rlp_u256(&rlp)?)),
            None => Ok(None),
        }
    }

    /// Reads a raw value, mapping Firewood errors to `ProviderError`.
    fn read(&self, key: &[u8]) -> ProviderResult<Option<Vec<u8>>> {
        Ok(self.rev.val(key).map_err(map_fw_err)?.map(|v| v.to_vec()))
    }
}

/// Maps a [`firewood::api::Error`] to a reth [`ProviderError`].
fn map_fw_err(err: firewood::api::Error) -> ProviderError {
    ProviderError::Database(ava_evm_reth::DatabaseError::Other(err.to_string()))
}
