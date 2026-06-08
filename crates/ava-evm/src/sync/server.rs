// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The leaf (range-proof) **server** side of C-Chain state sync (G8, spec 10
//! §10 / §17.9). Answers a [`LeafsRequest`] from a Firewood **historical
//! revision** with a wire-exact `FrozenRangeProof` (the Go `firewood/syncer`
//! reference), for both the EVM-state trie ([`EvmStateSyncServer`]) and the
//! atomic-trie / any standalone Firewood instance ([`RangeProofSource`]).
//!
//! This is a pure read path: it pins the revision identified by the request's
//! `root`, asks Firewood for a `range_proof(start, end, limit)`, and returns the
//! proof bytes + the proven leaves. The transport (p2p SDK, specs/05) and proto
//! envelope (`proto/sync/sync.proto`) live at the 12-node wiring layer; this
//! module is the protocol logic the handler delegates to.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use ava_evm_reth::{B256, ProviderError};
use firewood::api::{Db as _, DbView as _, FrozenRangeProof, HashKey};
use firewood::db::{Db, DbConfig};
use firewood::manager::RevisionManagerConfig;
use firewood::verify_range_proof;
use firewood_storage::NodeHashAlgorithm;

use crate::error::{Error, Result};
use crate::state::FirewoodStateProvider;
use crate::sync::{LeafsRequest, LeafsResponse, MAX_KEY_VALUES_LIMIT};

/// Number of historical revisions a standalone source retains — mirrors the
/// state provider / atomic trie window (spec 04 §4.2).
const MAX_REVISIONS: usize = 256;

/// Anything that can answer a [`LeafsRequest`] with a Firewood range proof. Both
/// the EVM-state server and a standalone Firewood source implement it, so the
/// client ([`crate::sync::client::StateSyncClient`]) drives either uniformly.
pub trait LeafServer {
    /// Serve the leaves in `[req.start, req.end]` at `req.root` as a wire-exact
    /// Firewood range proof.
    ///
    /// # Errors
    /// Returns [`Error`] if the requested root is not a retained revision or
    /// Firewood fails to build the proof.
    fn handle_leafs(&self, req: &LeafsRequest) -> Result<LeafsResponse>;
}

/// The protocol-capped key limit for a leaf request (coreth `MaxKeyValuesLimit`).
fn capped_limit(req: &LeafsRequest) -> Option<NonZeroUsize> {
    NonZeroUsize::new(req.key_limit.min(MAX_KEY_VALUES_LIMIT) as usize)
}

/// Assembles a [`LeafsResponse`] from already-serialized proof bytes + leaves.
fn assemble(proof: Vec<u8>, keys: Vec<Vec<u8>>, vals: Vec<Vec<u8>>) -> LeafsResponse {
    LeafsResponse { proof, keys, vals }
}

/// Verifies wire-exact Firewood range-proof `bytes` against `root` for the
/// requested `[start, end]` bounds — the inverse of [`build_response`], used by
/// the client and by tests to confirm byte-format round-trip + cryptographic
/// soundness.
///
/// # Errors
/// Returns [`Error`] if the bytes are not a valid `FrozenRangeProof`, or the
/// proof fails to verify against `root`.
pub fn verify_proof_bytes(
    bytes: &[u8],
    root: B256,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
) -> Result<()> {
    let proof = FrozenRangeProof::from_slice(bytes)
        .map_err(|e| Error::Provider(ProviderError::Database(into_db_other(e.to_string()))))?;
    let hash = HashKey::try_from(root.as_slice())
        .map_err(|_| Error::Provider(ProviderError::StateForHashNotFound(root)))?;
    verify_range_proof(
        start.map(<[u8]>::to_vec),
        end.map(<[u8]>::to_vec),
        &hash,
        &proof,
    )
    .map_err(map_fw_err)
}

/// Serves EVM account/storage leaves from the Firewood **state** trie at a
/// historical revision (spec 10 §10, §17.9). Holds the live
/// [`FirewoodStateProvider`] (the state-of-record) and answers leaf requests by
/// pinning the revision named in the request `root`.
pub struct EvmStateSyncServer {
    /// The Firewood-ethhash state provider (owns the state `Db`).
    state: Arc<FirewoodStateProvider>,
}

impl EvmStateSyncServer {
    /// Builds a server over the live state provider.
    #[must_use]
    pub fn new(state: Arc<FirewoodStateProvider>) -> Self {
        EvmStateSyncServer { state }
    }

    /// Verifies wire-exact range-proof bytes against `root` — re-exported on the
    /// server type for ergonomic test/handler access (delegates to the free
    /// [`verify_proof_bytes`]).
    ///
    /// # Errors
    /// See [`verify_proof_bytes`].
    pub fn verify_proof_bytes(
        bytes: &[u8],
        root: B256,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<()> {
        verify_proof_bytes(bytes, root, start, end)
    }
}

impl LeafServer for EvmStateSyncServer {
    fn handle_leafs(&self, req: &LeafsRequest) -> Result<LeafsResponse> {
        // Pin the historical revision named by the request root and serve a
        // capped range proof over it (the byte-exact wire form).
        let view = self.state.history_by_state_root(req.root)?;
        let (proof, keys, vals) =
            view.range_proof_bytes(req.start.as_bytes(), req.end.as_bytes(), capped_limit(req))?;
        Ok(assemble(proof, keys, vals))
    }
}

/// A standalone Firewood **range-proof source** — a read-only handle over an
/// on-disk Firewood instance (the atomic trie's data dir, or any second
/// instance). Used to serve atomic-trie leaves the same way EVM state is served
/// (spec 10 §10 "atomic trie state → synced the same way").
///
/// It opens its OWN Firewood handle at the given directory, so the caller must
/// release any exclusive writer over that dir first (Firewood is single-writer).
pub struct RangeProofSource {
    /// The opened Firewood instance (ethhash mode, matching the source trie).
    db: Db,
}

impl RangeProofSource {
    /// Opens a read-only range-proof source over the Firewood instance at `dir`
    /// (e.g. the atomic trie's data directory).
    ///
    /// # Errors
    /// Returns [`Error`] if Firewood fails to open the path or rejects the
    /// configuration (e.g. a hash-mode mismatch).
    pub fn open(dir: impl AsRef<Path>) -> Result<RangeProofSource> {
        let manager = RevisionManagerConfig::builder()
            .max_revisions(MAX_REVISIONS)
            .build();
        let cfg = DbConfig::builder()
            .node_hash_algorithm(NodeHashAlgorithm::compile_option())
            .manager(manager)
            .build();
        let db = Db::new(dir.as_ref(), cfg).map_err(|e| Error::Provider(map_fw_err_provider(e)))?;
        Ok(RangeProofSource { db })
    }
}

impl LeafServer for RangeProofSource {
    fn handle_leafs(&self, req: &LeafsRequest) -> Result<LeafsResponse> {
        let hash = HashKey::try_from(req.root.as_slice())
            .map_err(|_| Error::Provider(ProviderError::StateForHashNotFound(req.root)))?;
        let view = self
            .db
            .revision(hash)
            .map_err(|_| Error::Provider(ProviderError::StateForHashNotFound(req.root)))?;
        let proof: FrozenRangeProof = view
            .range_proof(
                req.start.as_bytes().map(<[u8]>::to_vec),
                req.end.as_bytes().map(<[u8]>::to_vec),
                capped_limit(req),
            )
            .map_err(map_fw_err)?;
        let mut keys = Vec::with_capacity(proof.key_values().len());
        let mut vals = Vec::with_capacity(proof.key_values().len());
        for (k, v) in proof.key_values() {
            keys.push(k.to_vec());
            vals.push(v.to_vec());
        }
        let mut bytes = Vec::new();
        proof.write_to_vec(&mut bytes);
        Ok(assemble(bytes, keys, vals))
    }
}

/// Maps a [`firewood::api::Error`] to the C-Chain [`Error`] model (database
/// bucket — spec 10 §11.2).
fn map_fw_err(err: firewood::api::Error) -> Error {
    Error::Provider(ProviderError::Database(into_db_other(err.to_string())))
}

/// Same mapping for an error wrapped directly into a `ProviderError`.
fn map_fw_err_provider(err: firewood::api::Error) -> ProviderError {
    ProviderError::Database(into_db_other(err.to_string()))
}

/// Wraps a message in reth's `DatabaseError::Other` through the facade.
fn into_db_other(msg: String) -> ava_evm_reth::DatabaseError {
    ava_evm_reth::DatabaseError::Other(msg)
}
