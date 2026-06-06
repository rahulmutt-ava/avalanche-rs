// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The state-sync driver ([`Syncer`]) and proof server ([`ProofServer`]).
//!
//! Faithful port of Go `database/merkle/sync/{syncer,network_server}.go`,
//! adapted to async Rust (spec 19 §4.2):
//! - `target_root` is an [`ArcSwap`] (lock-free reads on the hot path) instead
//!   of Go's `RWMutex`-guarded field; [`Syncer::update_sync_target`] swaps it.
//! - The work loop is a `tokio` task dispatching up to `simultaneous_work_limit`
//!   concurrent fetch+verify tasks; a [`Notify`] replaces Go's `sync.Cond`.
//! - Proof verification (CPU-bound, independent per range) runs on a [`rayon`]
//!   pool — a safe parallelism win (verification only checks a recomputed root
//!   against an expected root; overview §6.1, §9).
//!
//! [`ProofServer`] answers `RangeProofRequest`/`ChangeProofRequest` from a
//! [`SyncDb`] at a historical root, capped by `key_limit`/`bytes_limit` (the
//! response is `< bytes_limit` or the client rejects it with
//! [`SyncError::TooManyBytes`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use parking_lot::Mutex;
use tokio::sync::Notify;

use ava_types::id::Id;

use crate::proof::{ChangeProof, RangeProof};
use crate::sync::db::SyncableTrie;
use crate::sync::error::{SyncError, SyncResult};
use crate::sync::proto::{
    self, ChangeProofRequest, ProofRequest, ProofRequestKind, ProofResponse, ProofResponseKind,
    RangeProofRequest,
};
use crate::sync::workheap::{Priority, WorkHeap, WorkItem};
use crate::sync::{MAX_KEY_VALUES_LIMIT, SyncDb};

/// Default per-request key cap a client asks for (Go `defaultRequestKeyLimit`).
pub const DEFAULT_REQUEST_KEY_LIMIT: u32 = MAX_KEY_VALUES_LIMIT as u32;
/// Default per-request byte cap a client asks for (Go
/// `defaultRequestByteSizeLimit` = 256 KiB).
pub const DEFAULT_REQUEST_BYTE_LIMIT: u32 = 256 * 1024;

/// Maximum response size a server will emit (Go `maxByteSizeLimit`, capped well
/// under the default p2p message size).
pub const MAX_BYTE_SIZE_LIMIT: u32 = 2 * 1024 * 1024 - 4 * 1024;

// ---------------------------------------------------------------------------
// Proof server (network_server.go)
// ---------------------------------------------------------------------------

/// Answers range/change-proof requests from a [`SyncableTrie`]. The Go
/// `ProofHandler` p2p handler; here the transport is abstracted by
/// [`SyncClient`].
pub struct ProofServer {
    db: Arc<SyncableTrie>,
}

impl ProofServer {
    /// A server backed by `db`.
    #[must_use]
    pub fn new(db: Arc<SyncableTrie>) -> ProofServer {
        ProofServer { db }
    }

    /// Handles a serialized [`ProofRequest`], returning the serialized
    /// [`ProofResponse`].
    ///
    /// # Errors
    /// Returns a [`SyncError`] if the request is malformed or no proof fits the
    /// byte limit.
    pub fn handle(&self, request_bytes: &[u8]) -> SyncResult<Vec<u8>> {
        let req: ProofRequest = proto::decode(request_bytes)?;
        match req.request {
            Some(ProofRequestKind::RangeProof(r)) => self.handle_range(&r),
            Some(ProofRequestKind::ChangeProof(c)) => self.handle_change(&c),
            None => Err(SyncError::Decode("empty proof request".to_string())),
        }
    }

    fn handle_range(&self, req: &RangeProofRequest) -> SyncResult<Vec<u8>> {
        validate_range_request(req)?;
        let root = proto::root_from_bytes(&req.root_hash)?;
        let start = proto::proto_to_opt(&req.start_key);
        let end = proto::proto_to_opt(&req.end_key);

        let mut key_limit = (req.key_limit as usize).min(MAX_KEY_VALUES_LIMIT);
        let bytes_limit = req.bytes_limit.min(MAX_BYTE_SIZE_LIMIT) as usize;

        while key_limit > 0 {
            let proof = match self
                .db
                .range_proof(root, start.as_deref(), end.as_deref(), key_limit)
            {
                Ok(p) => p,
                // A server lacking the root drops the request (Go returns nil).
                Err(SyncError::InsufficientHistory) => return Err(SyncError::InsufficientHistory),
                Err(e) => return Err(e),
            };
            let inner = proof.encode_proto();
            let resp = ProofResponse {
                response: Some(ProofResponseKind::RangeProof(inner.into())),
            };
            let bytes = proto::encode(&resp);
            if bytes.len() < bytes_limit {
                return Ok(bytes);
            }
            key_limit /= 2;
        }
        Err(SyncError::MinProofSizeTooLarge)
    }

    fn handle_change(&self, req: &ChangeProofRequest) -> SyncResult<Vec<u8>> {
        validate_change_request(req)?;
        let start_root = proto::root_from_bytes(&req.start_root_hash)?;
        let end_root = proto::root_from_bytes(&req.end_root_hash)?;
        let start = proto::proto_to_opt(&req.start_key);
        let end = proto::proto_to_opt(&req.end_key);

        let mut key_limit = (req.key_limit as usize).min(MAX_KEY_VALUES_LIMIT);
        let bytes_limit = req.bytes_limit.min(MAX_BYTE_SIZE_LIMIT) as usize;

        while key_limit > 0 {
            match self.db.change_proof(
                start_root,
                end_root,
                start.as_deref(),
                end.as_deref(),
                key_limit,
            ) {
                Ok(proof) => {
                    let inner = proof.encode_proto();
                    let resp = ProofResponse {
                        response: Some(ProofResponseKind::ChangeProof(inner.into())),
                    };
                    let bytes = proto::encode(&resp);
                    if bytes.len() < bytes_limit {
                        return Ok(bytes);
                    }
                    key_limit /= 2;
                }
                // No end root in history: can't fall back -> propagate.
                Err(SyncError::NoEndRoot) => return Err(SyncError::NoEndRoot),
                // Insufficient history for a change proof -> fall back to a range
                // proof at the end root (Go `handleChangeProofRequest`).
                Err(SyncError::InsufficientHistory) => {
                    return self.handle_range(&RangeProofRequest {
                        root_hash: req.end_root_hash.clone(),
                        start_key: req.start_key.clone(),
                        end_key: req.end_key.clone(),
                        key_limit: req.key_limit,
                        bytes_limit: req.bytes_limit,
                    });
                }
                Err(e) => return Err(e),
            }
        }
        Err(SyncError::MinProofSizeTooLarge)
    }
}

/// Returns `Ok` iff `req` is well-formed (Go `validateRangeProofRequest`).
fn validate_range_request(req: &RangeProofRequest) -> SyncResult<()> {
    if req.bytes_limit == 0 {
        return Err(SyncError::InvalidBytesLimit);
    }
    if req.key_limit == 0 {
        return Err(SyncError::InvalidKeyLimit);
    }
    if req.root_hash.len() != 32 {
        return Err(SyncError::InvalidRootHash);
    }
    if req.root_hash.as_ref() == Id::EMPTY.as_bytes() {
        return Err(SyncError::EmptyProof);
    }
    if let (Some(s), Some(e)) = (&req.start_key, &req.end_key)
        && s.value > e.value
    {
        return Err(SyncError::InvalidBounds);
    }
    Ok(())
}

/// Returns `Ok` iff `req` is well-formed (Go `validateChangeProofRequest`).
fn validate_change_request(req: &ChangeProofRequest) -> SyncResult<()> {
    if req.bytes_limit == 0 {
        return Err(SyncError::InvalidBytesLimit);
    }
    if req.key_limit == 0 {
        return Err(SyncError::InvalidKeyLimit);
    }
    if req.start_root_hash.len() != 32 || req.end_root_hash.len() != 32 {
        return Err(SyncError::InvalidRootHash);
    }
    if req.end_root_hash.as_ref() == Id::EMPTY.as_bytes() {
        return Err(SyncError::EmptyProof);
    }
    if let (Some(s), Some(e)) = (&req.start_key, &req.end_key)
        && s.value > e.value
    {
        return Err(SyncError::InvalidBounds);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Client transport
// ---------------------------------------------------------------------------

/// A boxed, `Send` future of a sync response — the return type of
/// [`SyncClient::app_request`] (avoids an `async-trait` dependency).
pub type ResponseFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = SyncResult<Vec<u8>>> + Send + 'a>>;

/// The transport a [`Syncer`] uses to send a serialized [`ProofRequest`] and
/// receive a serialized [`ProofResponse`] (the p2p SDK `Client` boundary). An
/// in-process [`LocalClient`] is provided for tests; a real impl forwards over
/// `network/p2p` (M2).
pub trait SyncClient: Send + Sync {
    /// Sends `request_bytes`, returning the response bytes.
    fn app_request(&self, request_bytes: Vec<u8>) -> ResponseFuture<'_>;
}

/// In-process [`SyncClient`] that calls a local [`ProofServer`] directly. Used
/// for the roundtrip tests and any single-process sync.
pub struct LocalClient {
    server: Arc<ProofServer>,
}

impl LocalClient {
    /// A client wired straight to `server`.
    #[must_use]
    pub fn new(server: Arc<ProofServer>) -> LocalClient {
        LocalClient { server }
    }
}

impl SyncClient for LocalClient {
    fn app_request(&self, request_bytes: Vec<u8>) -> ResponseFuture<'_> {
        let server = Arc::clone(&self.server);
        Box::pin(async move { server.handle(&request_bytes) })
    }
}

// ---------------------------------------------------------------------------
// Sync driver (syncer.go)
// ---------------------------------------------------------------------------

/// Static configuration for a [`Syncer`].
#[derive(Clone, Copy, Debug)]
pub struct SyncerConfig {
    /// Max number of work items processed concurrently (Go
    /// `SimultaneousWorkLimit`).
    pub simultaneous_work_limit: usize,
    /// Per-request key cap.
    pub key_limit: u32,
    /// Per-request byte cap.
    pub byte_limit: u32,
}

impl Default for SyncerConfig {
    fn default() -> SyncerConfig {
        SyncerConfig {
            simultaneous_work_limit: 4,
            key_limit: DEFAULT_REQUEST_KEY_LIMIT,
            byte_limit: DEFAULT_REQUEST_BYTE_LIMIT,
        }
    }
}

/// Drives state sync of a local [`SyncableTrie`] toward a target root over a
/// [`SyncClient`]. See the module docs for the async/parallelism mapping.
pub struct Syncer {
    db: Arc<SyncableTrie>,
    client: Arc<dyn SyncClient>,
    target_root: ArcSwap<Id>,
    config: SyncerConfig,
    unprocessed: Mutex<WorkHeap>,
    processed: Mutex<WorkHeap>,
    processing: AtomicUsize,
    work_available: Notify,
}

impl Syncer {
    /// Builds a syncer that drives `db` toward `target_root` over `client`.
    #[must_use]
    pub fn new(
        db: Arc<SyncableTrie>,
        client: Arc<dyn SyncClient>,
        target_root: Id,
        config: SyncerConfig,
    ) -> Arc<Syncer> {
        Arc::new(Syncer {
            db,
            client,
            target_root: ArcSwap::from_pointee(target_root),
            config,
            unprocessed: Mutex::new(WorkHeap::new()),
            processed: Mutex::new(WorkHeap::new()),
            processing: AtomicUsize::new(0),
            work_available: Notify::new(),
        })
    }

    /// The current target root.
    #[must_use]
    pub fn target_root(&self) -> Id {
        **self.target_root.load()
    }

    /// Advances the target root (Go `UpdateSyncTarget`): moves all processed
    /// ranges back into the unprocessed heap at high priority so they're
    /// re-validated as change proofs against the new root.
    pub fn update_sync_target(&self, new_root: Id) {
        if self.target_root() == new_root {
            return;
        }
        self.target_root.store(Arc::new(new_root));

        let mut processed = self.processed.lock();
        let mut unprocessed = self.unprocessed.lock();
        let mut moved = false;
        while let Some(mut item) = processed.get_work() {
            item.priority = Priority::High;
            unprocessed.insert(item);
            moved = true;
        }
        drop(unprocessed);
        drop(processed);
        if moved {
            self.work_available.notify_one();
        }
    }

    /// Runs the sync to completion. On success the local root equals the target
    /// root (checked at the end, Go `ErrFinishedWithUnexpectedRoot`).
    ///
    /// # Errors
    /// Returns a [`SyncError`] on a fatal fetch/verify failure or if the final
    /// root doesn't match the target.
    pub async fn sync(self: &Arc<Syncer>) -> SyncResult<()> {
        // Seed the whole keyspace at low priority.
        self.unprocessed
            .lock()
            .insert(WorkItem::whole_keyspace(Priority::Low));

        loop {
            // Drain available work up to the concurrency limit.
            let mut tasks = Vec::new();
            loop {
                if self.processing.load(Ordering::SeqCst) >= self.config.simultaneous_work_limit {
                    break;
                }
                let work = self.unprocessed.lock().get_work();
                let Some(work) = work else { break };
                self.processing.fetch_add(1, Ordering::SeqCst);
                let me = Arc::clone(self);
                tasks.push(tokio::spawn(async move { me.do_work(work).await }));
            }

            // If nothing is in flight and nothing is queued, we're done.
            if tasks.is_empty() && self.processing.load(Ordering::SeqCst) == 0 {
                let empty = self.unprocessed.lock().is_empty();
                if empty {
                    break;
                }
            }

            // Await the dispatched batch; surface the first fatal error.
            for t in tasks {
                match t.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return Err(e),
                    Err(_) => return Err(SyncError::Closed),
                }
            }
        }

        let root = self.db.merkle_root()?;
        let target = self.target_root();
        if root != target {
            return Err(SyncError::FinishedWithUnexpectedRoot {
                expected: target,
                got: root,
            });
        }
        Ok(())
    }

    /// Fetches + verifies + commits one work item, then enqueues any follow-up.
    async fn do_work(self: &Arc<Syncer>, work: WorkItem) -> SyncResult<()> {
        let res = self.fetch_and_apply(&work).await;
        self.processing.fetch_sub(1, Ordering::SeqCst);
        self.work_available.notify_one();
        res
    }

    async fn fetch_and_apply(self: &Arc<Syncer>, work: &WorkItem) -> SyncResult<()> {
        let target = self.target_root();
        let start = work.start.as_deref();
        let end = work.end.as_deref();

        let req = if work.local_root == Id::EMPTY {
            proto::range_proof_request(
                target,
                start,
                end,
                self.config.key_limit,
                self.config.byte_limit,
            )
        } else {
            proto::change_proof_request(
                work.local_root,
                target,
                start,
                end,
                self.config.key_limit,
                self.config.byte_limit,
            )
        };

        let resp_bytes = self.client.app_request(proto::encode(&req)).await?;
        let resp: ProofResponse = proto::decode(&resp_bytes)?;

        match resp.response {
            Some(ProofResponseKind::RangeProof(inner)) => {
                let proof: RangeProof = proto::range_proof_from_bytes(&inner)?;
                // Verify against the target on the rayon pool (CPU-bound, pure).
                let hasher = crate::hashing::DefaultHasher;
                let (s, e, bf) = (
                    work.start.clone(),
                    work.end.clone(),
                    self.db.branch_factor(),
                );
                let proof_for_verify = proof.clone();
                let verify = rayon::scope(|_| {
                    proof_for_verify.verify(s.as_deref(), e.as_deref(), target, bf, &hasher)
                });
                if verify.is_err() {
                    return Err(SyncError::InvalidRangeProof);
                }
                self.db.commit_range_proof(start, end, proof)?;
            }
            Some(ProofResponseKind::ChangeProof(inner)) => {
                let proof: ChangeProof = proto::change_proof_from_bytes(&inner)?;
                self.db
                    .verify_change_proof(&proof, start, end, target)
                    .map_err(|_| SyncError::InvalidChangeProof)?;
                self.db.commit_change_proof(proof)?;
            }
            None => return Err(SyncError::Decode("empty proof response".to_string())),
        }

        self.complete_work_item(work, target);
        Ok(())
    }

    /// Records a completed range, re-queuing as change-proof work if the target
    /// advanced mid-flight (Go `completeWorkItem`).
    fn complete_work_item(&self, work: &WorkItem, root: Id) {
        let stale = self.target_root() != root;
        let completed = WorkItem::new(root, work.start.clone(), work.end.clone(), work.priority);
        if stale {
            let mut item = completed;
            item.priority = Priority::High;
            self.unprocessed.lock().insert(item);
            self.work_available.notify_one();
        } else {
            self.processed.lock().merge_insert(completed);
        }
    }
}
