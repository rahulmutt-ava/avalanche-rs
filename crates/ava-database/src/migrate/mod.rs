// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The R2 offline Go-data-dir import tool (04 §11, closes overview §11.2 risk
//! R2). Gated behind the `migrate` feature; wired into the CLI in M12.
//!
//! ## The problem (04 §11.1)
//!
//! A Go avalanchego node persists its base DB as either **goleveldb**
//! (`db/v1.4.5/`) or **Pebble** (`pebble/`). The Rust node's only on-disk
//! backend is **RocksDB** (04 §2.1). The engines share neither file format nor
//! SSTable layout, so a Rust node **cannot open a Go data dir in place**. But
//! everything layered *inside* the KV pairs is byte-identical (the whole 04 §10
//! catalog: prefixdb SHA-256 namespaces, linkeddb node codec, archivedb
//! `^height` keys, blockdb file format, merkledb node/proof bytes). So migration
//! reduces to: **copy every `(key, value)` pair into RocksDB, verbatim.** No
//! transformation of key or value bytes is ever required.
//!
//! ## The decision (04 §11.2, BINDING)
//!
//! Ship an **offline, one-shot import tool** that reads the Go DB and bulk-loads
//! RocksDB, run before first Rust-node start. In-place open and runtime
//! translation are rejected; the network-bootstrap alternative (04 §11.5) is the
//! supported "no tool" path. See `crates/ava-database/docs/migration.md`.
//!
//! ## This module
//!
//! - [`import`] — the higher-level facade (M9.16): detect the Go backend by its
//!   schema-version folder name (26 §6), open the source, create a fresh RocksDB
//!   dir named `CURRENT_DATABASE`, stream via [`migrate`], then verify. The
//!   detection/refusal primitives ([`import::detect_backend`],
//!   [`import::GoBackend`], [`import::ImportError`]) are feature-free so the
//!   node-side refusal (`ava-node`) can use them without RocksDB.
//! - [`GoDbSource`] — a backend-agnostic source: every Go pair, in lexicographic
//!   key order, **verbatim** (no byte transformation).
//! - [`migrate`] — the byte-exact copy driver: batched bulk-ingest with a 64 MiB
//!   flush window and a [`MIGRATION_CURSOR_KEY`] resume checkpoint.
//! - [`leveldb`] — the goleveldb readers ([`leveldb::RocksDbCompatSource`] fast
//!   path + [`leveldb::RustyLevelDbSource`] fallback, best-effort).
//! - [`pebble`] — the Pebble Go-sidecar reader ([`pebble::PebbleSidecarSource`]);
//!   **in-place Pebble open is NOT supported**.
//! - [`verify`] — post-migration verification with a **pluggable** root
//!   re-derivation ([`verify::RootVerifier`]) so this module never depends on
//!   `ava-merkledb` (concrete wiring lands with the CLI in M12).

pub mod import;
pub mod leveldb;
pub mod pebble;
pub mod verify;

#[cfg(doc)]
use crate::traits::Database;
use crate::traits::DynDatabase;

/// The reserved key under which [`migrate`] records the last-written source key,
/// enabling `--resume` to skip already-copied pairs (04 §11.4).
///
/// The byte string is chosen to sort *after* any plausible Go on-disk key — it
/// is not part of the §10 catalog, so it cannot collide with a migrated pair.
/// The CLI strips it (or leaves it; it is harmless) after a verified migration.
pub const MIGRATION_CURSOR_KEY: &[u8] = b"\xff\xff\xff\xffava-rs-migration-cursor";

/// The bulk-ingest flush window: accumulate up to ~64 MiB of buffered ops before
/// flushing a batch and recording a resume checkpoint (04 §11.4).
pub const FLUSH_WINDOW_BYTES: usize = 64 * 1024 * 1024;

/// A single migrated `(key, value)` pair, owned and untransformed.
pub type Pair = (Vec<u8>, Vec<u8>);

/// A boxed, ordered iterator over every [`Pair`] in a Go data dir.
pub type PairIter = Box<dyn Iterator<Item = Pair>>;

/// A backend-agnostic source of every `(key, value)` pair in a Go data dir, in
/// lexicographic key order (04 §11.4).
///
/// **The contract is byte-for-byte:** [`iter_all`](GoDbSource::iter_all) yields
/// keys and values exactly as the Go engine stored them, with **no
/// transformation**. Lexicographic order is what lets [`migrate`] drive a
/// RocksDB `SstFileWriter` for a bulk ingest on the fast path.
///
/// Implementors live in [`leveldb`] and [`pebble`].
pub trait GoDbSource {
    /// Yields every `(key, value)` pair verbatim, in ascending key order.
    fn iter_all(&self) -> anyhow::Result<PairIter>;
}

/// Copies every pair from `src` into `dst`, byte-for-byte (04 §11.4).
///
/// The driver takes the object-safe [`DynDatabase`] facade so it works with any
/// ingest target ([`RocksDb`](crate::rocksdb::RocksDb) in production;
/// [`MemDb`](crate::MemDb) in tests) behind `&dyn`. (The typed [`Database`]
/// trait carries a GAT iterator and is not dyn-compatible; `DynDatabase` is the
/// boxed facade every backend also implements — 04 §1.3.) The loop never decodes
/// a key or value, so the entire 04 §10
/// catalog rides along untouched — this is what makes a one-pass copy correct.
///
/// Behaviour:
///
/// - **Idempotent / resumable.** `resume_after = Some(cursor)` skips every key
///   `<= cursor` (the keys already migrated in a prior run). A clean re-run over
///   a complete dir with `resume_after = None` is still correct: every `put`
///   rewrites identical bytes.
/// - **Flush window.** Buffered ops are flushed to `dst` whenever they reach
///   [`FLUSH_WINDOW_BYTES`]; after each flush the last-written key is recorded
///   under [`MIGRATION_CURSOR_KEY`] so an interrupted migration can resume.
///
/// Returns the number of pairs copied (excluding the cursor checkpoint itself).
///
/// # Errors
///
/// Propagates any source-iteration or destination-write failure.
pub fn migrate(
    src: &dyn GoDbSource,
    dst: &dyn DynDatabase,
    resume_after: Option<&[u8]>,
) -> anyhow::Result<u64> {
    let mut count: u64 = 0;
    let mut batch = dst.new_batch();
    let mut last_key: Option<Vec<u8>> = None;

    for (key, value) in src.iter_all()? {
        // Idempotency / --resume: skip anything already migrated.
        if let Some(after) = resume_after
            && key.as_slice() <= after
        {
            continue;
        }

        // Byte-for-byte: prefixdb namespaces, codec values, ^height keys, merkledb
        // nodes, etc. are ALL preserved because we never touch the bytes (04 §10).
        batch.put(&key, &value)?;
        count = count.saturating_add(1);
        last_key = Some(key);

        if batch.size() >= FLUSH_WINDOW_BYTES {
            batch.write()?;
            if let Some(ref lk) = last_key {
                dst.put(MIGRATION_CURSOR_KEY, lk)?; // resume checkpoint
            }
            batch = dst.new_batch();
        }
    }

    batch.write()?;
    if let Some(ref lk) = last_key {
        dst.put(MIGRATION_CURSOR_KEY, lk)?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memdb::MemDb;
    use crate::traits::KeyValueReader;

    struct VecSource(Vec<(Vec<u8>, Vec<u8>)>);

    impl GoDbSource for VecSource {
        fn iter_all(&self) -> anyhow::Result<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>>> {
            let mut pairs = self.0.clone();
            pairs.sort();
            Ok(Box::new(pairs.into_iter()))
        }
    }

    #[test]
    fn migrate_copies_every_pair_verbatim() {
        let pairs = vec![
            (b"a".to_vec(), b"1".to_vec()),
            (b"b".to_vec(), b"2".to_vec()),
            (Vec::new(), b"empty-key".to_vec()),
        ];
        let src = VecSource(pairs.clone());
        let dst = MemDb::new();

        let n = migrate(&src, &dst, None).expect("migrate");
        assert_eq!(n, 3);
        for (k, v) in &pairs {
            assert_eq!(&KeyValueReader::get(&dst, k).expect("get"), v);
        }
    }

    #[test]
    fn empty_source_writes_no_cursor() {
        let src = VecSource(Vec::new());
        let dst = MemDb::new();
        let n = migrate(&src, &dst, None).expect("migrate");
        assert_eq!(n, 0);
        // No pairs => no cursor checkpoint.
        assert!(matches!(
            KeyValueReader::get(&dst, MIGRATION_CURSOR_KEY),
            Err(crate::error::Error::NotFound)
        ));
    }
}
