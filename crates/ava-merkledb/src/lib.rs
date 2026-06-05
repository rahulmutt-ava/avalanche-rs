// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-merkledb` — the path-based Merkle radix trie.
//!
//! Tier T1 (storage). Owning spec: `specs/04-storage-and-databases.md` §3.
//! A faithful, **byte-exact** reimplementation of Go `x/merkledb`'s
//! node/key/hash scheme (not a Firewood wrapper — see spec §3.1). Root hashes
//! and the on-disk node codec are protocol-critical and MUST match Go
//! bit-for-bit.
//!
//! Implemented across milestone M1 (`plan/M1-storage.md`):
//!
//! - [`key`] — `Key`/`Path` bit-path over [`key::BranchFactor`] (M1.12)
//! - [`node`] / [`codec`] — node model + on-disk `encode_db_node` codec (M1.13)
//! - [`hashing`] — `Hasher` trait + SHA-256 `DefaultHasher` + root computation
//!   over the shared in-memory [`trie`] (M1.14)
//! - [`db`] / [`view`] / [`history`] — DB-backed [`MerkleDb`], immutable
//!   [`View`]/`TrieView` proposals + the bounded change-set history (M1.15)
//!
//! Proofs and state-sync are later M1 tasks and are intentionally NOT in this
//! crate yet.
//!
//! `Maybe<T>` (the "something / nothing" type from the spec) is defined locally
//! in [`maybe`] rather than in `ava-types` to keep this crate self-contained.

#![forbid(unsafe_code)]

pub mod codec;
pub mod db;
pub mod error;
pub mod hashing;
pub mod history;
pub mod key;
pub mod maybe;
pub mod node;
mod node_store;
mod trie;
pub mod view;

pub use codec::{decode_db_node, encode_db_node};
pub use db::MerkleDb;
pub use error::{Error, Result};
pub use hashing::{DefaultHasher, HASH_LENGTH, Hasher, merkle_root};
pub use history::{ChangeSummary, History, KeyChange};
pub use key::{BranchFactor, Key};
pub use maybe::Maybe;
pub use node::{Child, DbNode, Node};
pub use view::{BatchOp, View};
