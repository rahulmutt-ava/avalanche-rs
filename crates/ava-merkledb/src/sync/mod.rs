// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! State-sync protocol (`database/merkle/sync`, `proto/sync`).
//!
//! This is the VM-internal "out-of-band" mechanism a state-syncable VM uses to
//! pull a trie at a target root (specs/04 §3.7, 19 §4, 15 §3.10). It is a
//! separate protocol from the consensus engine: payloads are `proto/sync`
//! frames carried over the p2p SDK `Client`, and proof verification is the
//! byte-exact [`crate::proof`] machinery (M1.17/M1.18).
//!
//! Pieces:
//! - [`db::SyncDb`] — the trait a syncable trie implements (server + commit
//!   side), verbatim from spec 04 §3.7, plus [`db::SyncableTrie`], the
//!   in-memory `ava-merkledb` implementation.
//! - [`workheap::WorkHeap`] / [`workheap::WorkItem`] / [`workheap::Priority`] —
//!   the range work-splitting priority queue (spec 19 §4.1/§4.2), a faithful
//!   port of Go `workheap.go`.
//! - [`syncer::Syncer`] — the parallel sync driver (`ArcSwap` target root,
//!   tokio `Notify` in place of Go's `sync.Cond`, a bounded tokio task set, and
//!   a rayon verify pool) plus the [`syncer::ProofServer`] that answers
//!   range/change requests capped by `key_limit`/`bytes_limit`.
//! - [`proto`] — `proto/sync` frame <-> Rust type conversions (generated types
//!   reached via `tonic::include_proto!("sync")`, `bytes` -> [`bytes::Bytes`]).
//!
//! Everything here is gated behind the crate `sync` feature (it pulls in
//! `prost`/`tonic`/`tokio` + the `protoc` codegen step).

pub mod db;
mod error;
pub mod proto;
pub mod syncer;
pub mod workheap;

pub use db::{SyncDb, SyncableTrie};
pub use error::{SyncError, SyncResult};
pub use proto::{ProofRequest, ProofResponse, change_proof_request, range_proof_request};
pub use syncer::{ProofServer, SyncClient, Syncer, SyncerConfig};
pub use workheap::{Priority, WorkHeap, WorkItem};

/// Maximum number of key/value pairs a single proof may carry, regardless of a
/// larger requested `key_limit` (Go `MaxKeyValuesLimit`).
pub const MAX_KEY_VALUES_LIMIT: usize = 2048;
