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
//!   over a minimal in-memory trie builder (M1.14)
//!
//! View/TrieView, history, DB-backed node stores, proofs and state-sync are
//! later M1 tasks and are intentionally NOT in this crate yet.
//!
//! `Maybe<T>` (the "something / nothing" type from the spec) is defined locally
//! in [`maybe`] rather than in `ava-types` to keep this crate self-contained.

#![forbid(unsafe_code)]

pub mod codec;
pub mod error;
pub mod hashing;
pub mod key;
pub mod maybe;
pub mod node;

pub use error::{Error, Result};
pub use key::{BranchFactor, Key};
pub use maybe::Maybe;
