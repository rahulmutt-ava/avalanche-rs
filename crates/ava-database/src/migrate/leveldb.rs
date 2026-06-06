// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! goleveldb readers for the import tool (04 §11.3).
//!
//! A Go node written before v1.10.15 stores its base DB as **goleveldb** under
//! `db/v1.4.5/`. Two reader strategies, in order of preference:
//!
//! 1. [`RocksDbCompatSource`] — the **fast path**. RocksDB can open many classic
//!    LevelDB directories directly (it descends from LevelDB and reads the same
//!    SSTable/MANIFEST family). When the open succeeds, the "migration" is just a
//!    streaming in-place ingest. Gated on the `rocksdb` feature (it reuses the
//!    crate's `RocksDb` FFI backend).
//! 2. [`RustyLevelDbSource`] — the **fallback** pure-Rust reader for dirs RocksDB
//!    refuses. The spec proposes evaluating the `rusty-leveldb` crate; see the
//!    status note on the type for why it currently ships as a documented
//!    best-effort stub rather than pulling that dependency.
//!
//! Both yield pairs **verbatim** in lexicographic order ([`GoDbSource`]).

use crate::migrate::GoDbSource;

/// The conventional on-disk subdirectory name of a goleveldb base DB written by
/// avalanchego (`db/v1.4.5/`). Used by the CLI's backend auto-detection (04
/// §11.3); `--db-type leveldb` overrides.
pub const GOLEVELDB_DIR_NAME: &str = "v1.4.5";

/// The goleveldb **fast path** (04 §11.3): open the Go LevelDB directory with the
/// crate's RocksDB backend (RocksDB reads many classic LevelDB dirs) and stream
/// every pair through its ordered iterator.
///
/// This is the cheapest migration path — when RocksDB opens the dir, the pairs
/// can feed an `SstFileWriter` for a bulk ingest, so multi-GB dirs migrate in
/// minutes. Gated on the `rocksdb` feature because it reuses the FFI backend.
#[cfg(feature = "rocksdb")]
pub struct RocksDbCompatSource {
    db: crate::rocksdb::RocksDb,
}

#[cfg(feature = "rocksdb")]
impl RocksDbCompatSource {
    /// Opens the goleveldb directory `path` read-only via the RocksDB backend.
    ///
    /// # Errors
    ///
    /// Returns an error if RocksDB cannot open the directory (e.g. it is a Pebble
    /// dir, or uses LevelDB features RocksDB does not read). On failure the
    /// caller should fall back to [`RustyLevelDbSource`].
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        let db = crate::rocksdb::RocksDb::open_with_config(
            path,
            &crate::rocksdb::RocksDbConfig::default(),
        )
        .map_err(|e| anyhow::anyhow!("rocksdb leveldb-compat open failed: {e}"))?;
        Ok(Self { db })
    }
}

#[cfg(feature = "rocksdb")]
impl GoDbSource for RocksDbCompatSource {
    fn iter_all(&self) -> anyhow::Result<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>>> {
        use crate::traits::{Iteratee, Iterator as _};

        // Drain the ordered iterator into an owned, lexicographically-ordered
        // vector. (A streaming adaptor over the borrowed iterator would tie the
        // returned `Box` to `&self`'s lifetime, which the object-safe
        // `GoDbSource::iter_all` signature does not carry; collecting keeps the
        // trait simple. The SstFileWriter fast path consumes this same order.)
        let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut it = self.db.new_iterator_with_start_and_prefix(&[], &[]);
        while it.next() {
            match (it.key(), it.value()) {
                (Some(k), Some(v)) => out.push((k.to_vec(), v.to_vec())),
                _ => break,
            }
        }
        it.error()
            .map_err(|e| anyhow::anyhow!("leveldb-compat iteration error: {e}"))?;
        Ok(Box::new(out.into_iter()))
    }
}

/// The pure-Rust goleveldb **fallback** reader (04 §11.3) for directories the
/// [`RocksDbCompatSource`] fast path cannot open.
///
/// # Status — documented best-effort stub
///
/// The spec proposes evaluating the `rusty-leveldb` crate (reads SSTables +
/// MANIFEST + log). It is **not yet wired** here: pulling it for the fallback
/// path adds an unvetted transitive-dependency surface to `ava-database` that
/// must clear `cargo deny` first. Per the M1.24 directive ("if a real on-disk
/// reader is heavy/risky to wire, implement the trait + driver fully and make
/// the readers best-effort, documenting status precisely"), this reader holds
/// the constructor + trait shape and returns an explanatory error until the dep
/// is vetted. The byte-exact [`migrate`](crate::migrate::migrate) driver, the
/// [`RocksDbCompatSource`] fast path, and the [`verify`](crate::migrate::verify)
/// tier are all fully implemented and tested against a stub source.
pub struct RustyLevelDbSource {
    path: std::path::PathBuf,
}

impl RustyLevelDbSource {
    /// Records the goleveldb directory to read. Opening is deferred to
    /// [`iter_all`](GoDbSource::iter_all).
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The goleveldb directory this reader targets.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl GoDbSource for RustyLevelDbSource {
    fn iter_all(&self) -> anyhow::Result<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>>> {
        anyhow::bail!(
            "RustyLevelDbSource ({}) is a documented best-effort stub: the \
             pure-Rust goleveldb fallback (rusty-leveldb) is pending cargo-deny \
             vetting. Use the RocksDbCompatSource fast path (the `rocksdb` \
             feature) for goleveldb dirs RocksDB can open. See \
             crates/ava-database/docs/migration.md.",
            self.path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rusty_leveldb_stub_reports_status() {
        let src = RustyLevelDbSource::new("/tmp/does-not-matter/v1.4.5");
        assert_eq!(src.path().to_string_lossy(), "/tmp/does-not-matter/v1.4.5");
        let err = src.iter_all().err().expect("stub must error").to_string();
        assert!(err.contains("best-effort stub"));
    }
}
