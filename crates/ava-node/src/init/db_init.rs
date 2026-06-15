// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Pre-open data-directory guard (specs/26 §6, 04 §11, 27 §4) for the node DB
//! init step (mirror the migration-trigger / refusal half of Go `initDatabase`).
//!
//! The Rust node's only on-disk backend is RocksDB (04 §2.1); a Go node persists
//! its base DB as **goleveldb** (`<data-dir>/v1.4.5/`) or **Pebble**
//! (`<data-dir>/pebble/`). The two engines share no file format, so the Rust
//! node **cannot open a Go data dir in place** (04 §11.1). The schema-version
//! folder name *is* the schema version (26 §6), so it is also how a
//! foreign/older dir is detected.
//!
//! This module runs **before** [`open_backend`](super::database) opens RocksDB:
//!
//! - A **Pebble** (`pebble/`) folder, or a **`PREV_DATABASE`** (`v1.0.0/`)
//!   folder, is a foreign/older Go dir the node must never open in place — it is
//!   refused with [`Error::ForeignDataDir`] (26 §6: *"the node documents
//!   non-support and refuses to start against a foreign/older dir rather than
//!   silently corrupting it"*). The offline import tool (04 §11, M12 CLI) is the
//!   migration path; the network-bootstrap alternative (04 §11.5) is the "no
//!   tool" path.
//! - A `v1.4.5/` (`CURRENT_DATABASE`) folder is the dir the node opens: either a
//!   RocksDB dir we wrote, or an absent dir created fresh on first start. (A
//!   genuine Go *goleveldb* `v1.4.5/` is the one case RocksDB might open in a
//!   degraded compat mode; the import tool is still the supported path, and the
//!   genesis-hash check in [`init_database`](super::database::init_database)
//!   guards correctness — see the §6 note in `tests/PORTING.md`.)
//!
//! The `ungracefulShutdown` marker (27 §4) is owned by
//! [`init_database`](super::database::init_database) (it writes the marker after
//! the genesis-hash check, the graceful-shutdown path deletes it). This guard
//! runs strictly *before* the open, so it never touches the marker — the two are
//! deliberately non-overlapping.

use std::path::{Path, PathBuf};

use ava_config::node::DatabaseConfig;

use crate::error::{Error, Result};

/// The on-disk RocksDB schema-version folder the node opens (`CURRENT_DATABASE`,
/// 26 §6). Re-exported from `ava-version` so the guard and the opener agree.
pub fn current_db_dir_name() -> &'static str {
    ava_version::CURRENT_DATABASE
}

/// The prior on-disk schema-version folder (`PREV_DATABASE`, 26 §6): a dir under
/// this name was written by an older Go schema and must be migrated, not opened.
pub fn prev_db_dir_name() -> &'static str {
    ava_version::PREV_DATABASE
}

/// The Go Pebble base-DB folder name (`pebble/`, 04 §11.3): no Rust reader, so a
/// dir under this name can never be opened in place.
pub const PEBBLE_DIR_NAME: &str = "pebble";

/// Refuse to start against a foreign/older Go data directory (26 §6 / 04 §11).
///
/// Called before [`open_backend`](super::database) for every on-disk backend.
/// `memdb` is ephemeral and always allowed. For an on-disk backend rooted at
/// `db_config.path`, the guard refuses with [`Error::ForeignDataDir`] when it
/// finds a folder the Rust node cannot open in place:
///
/// - `pebble/` — a Go Pebble dir (no Rust reader at all).
/// - `<PREV_DATABASE>/` (`v1.0.0/`) — a prior Go schema version.
///
/// The `<CURRENT_DATABASE>/` (`v1.4.5/`) folder is **not** refused: it is what
/// the node opens (a RocksDB dir we wrote, or created fresh). Auto-import is
/// *not* performed here — the import is the explicit offline tool (04 §11.2);
/// this guard only enforces "refuse, never open-in-place" when import was not
/// run.
///
/// # Errors
///
/// [`Error::ForeignDataDir`] when a Pebble or `PREV_DATABASE` folder is present.
pub fn precheck_data_dir(db_config: &DatabaseConfig) -> Result<()> {
    if db_config.name == "memdb" {
        return Ok(());
    }
    precheck_path(Path::new(&db_config.path))
}

/// The path-only core of [`precheck_data_dir`], testable without a full
/// [`DatabaseConfig`]. `data_dir` is the node data directory (the parent of the
/// schema-version folder).
///
/// # Errors
///
/// [`Error::ForeignDataDir`] when a foreign/older schema-version folder is found.
pub fn precheck_path(data_dir: &Path) -> Result<()> {
    // A not-yet-created data dir is fine — the node creates a fresh RocksDB
    // `v1.4.5/` on first start.
    if !data_dir.is_dir() {
        return Ok(());
    }

    if data_dir.join(PEBBLE_DIR_NAME).is_dir() {
        return Err(foreign(data_dir, PEBBLE_DIR_NAME));
    }
    if data_dir.join(prev_db_dir_name()).is_dir() {
        return Err(foreign(data_dir, prev_db_dir_name()));
    }
    Ok(())
}

/// Builds the [`Error::ForeignDataDir`] refusal for `data_dir`/`backend`.
fn foreign(data_dir: &Path, backend: &str) -> Error {
    Error::ForeignDataDir {
        path: PathBuf::from(data_dir),
        backend: backend.to_string(),
        current: current_db_dir_name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_config(name: &str, path: &Path) -> DatabaseConfig {
        DatabaseConfig {
            name: name.to_string(),
            read_only: false,
            path: path.to_string_lossy().into_owned(),
            config: Vec::new(),
        }
    }

    /// 26 §6 / 04 §11.2: a Go Pebble dir is refused, never opened in place.
    #[test]
    fn refuses_pebble_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("pebble")).expect("mkdir");
        let err = precheck_data_dir(&db_config("pebbledb", dir.path())).expect_err("must refuse");
        assert!(
            matches!(err, Error::ForeignDataDir { ref backend, .. } if backend == "pebble"),
            "expected ForeignDataDir(pebble), got {err:?}"
        );
    }

    /// 26 §6: a `PREV_DATABASE` (`v1.0.0`) Go dir is refused.
    #[test]
    fn refuses_prev_database_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("v1.0.0")).expect("mkdir");
        let err = precheck_data_dir(&db_config("leveldb", dir.path())).expect_err("must refuse");
        assert!(
            matches!(err, Error::ForeignDataDir { ref backend, .. } if backend == "v1.0.0"),
            "expected ForeignDataDir(v1.0.0), got {err:?}"
        );
    }

    /// A fresh data dir (no schema folder yet) is allowed — first start creates
    /// the RocksDB `v1.4.5/` dir.
    #[test]
    fn allows_fresh_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        precheck_data_dir(&db_config("leveldb", dir.path())).expect("fresh dir allowed");
    }

    /// A `v1.4.5/` (CURRENT_DATABASE) dir is the dir we open — not refused.
    #[test]
    fn allows_current_database_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("v1.4.5")).expect("mkdir");
        precheck_data_dir(&db_config("leveldb", dir.path())).expect("v1.4.5 allowed");
    }

    /// `memdb` is ephemeral and always allowed.
    #[test]
    fn allows_memdb() {
        precheck_data_dir(&db_config("memdb", Path::new("/does/not/matter")))
            .expect("memdb allowed");
    }

    #[test]
    fn dir_name_constants() {
        assert_eq!(current_db_dir_name(), "v1.4.5");
        assert_eq!(prev_db_dir_name(), "v1.0.0");
    }
}
