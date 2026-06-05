// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-database` — the synchronous key/value storage tier.
//!
//! Tier T1 (storage). Owning spec: `specs/04-storage-and-databases.md` §1-§2.
//!
//! This crate ports avalanchego's `database` package: the `Database` trait
//! family ([`traits`]), the two sentinel errors ([`error`]), the byte-exact
//! free-function helpers ([`helpers`]) and the [`BatchOps`] recorder
//! ([`batch`]). KV backends (memdb, rocksdb, …) build on top of these.
//!
//! The trait family is **synchronous** (04 §1.2): backends are blocking
//! C/Rust libraries, so blocking DB work runs on `spawn_blocking` (or a
//! dedicated DB thread pool) at the *call site*, never inside the trait.

#![forbid(unsafe_code)]

pub mod batch;
pub mod error;
pub mod helpers;
pub mod traits;

#[cfg(feature = "testutil")]
pub mod dbtest;

pub mod corruptabledb;
pub mod linkeddb;
pub mod memdb;
pub mod meterdb;
pub mod prefixdb;
pub mod versiondb;

pub use batch::{BatchOp, BatchOps};
pub use corruptabledb::CorruptableDb;
pub use error::{Error, Result};
pub use linkeddb::LinkedDb;
pub use memdb::MemDb;
pub use meterdb::MeterDb;
pub use prefixdb::{PrefixDb, join_prefixes, make_prefix};
pub use traits::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, IteratorError,
    KeyValueDeleter, KeyValueReader, KeyValueWriter, WriteDelete,
};
pub use versiondb::VersionDb;
