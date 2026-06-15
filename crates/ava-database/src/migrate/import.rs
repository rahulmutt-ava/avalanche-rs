// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The R2 import *facade* (04 §11, 26 §6) — a higher-level entry point over the
//! byte-exact [`migrate`](crate::migrate::migrate) engine.
//!
//! Where [`migrate`](crate::migrate::migrate) is the raw copy driver over a
//! [`GoDbSource`] → [`DynDatabase`], this module is the operator-facing surface:
//!
//! 1. **Detect** the Go backend from the on-disk schema-version folder name
//!    (26 §6 / 04 §11.3): `v1.4.5/` ⇒ goleveldb, `pebble/` ⇒ Pebble. The folder
//!    name *is* the schema version (26 §6); there is no separate in-file version
//!    byte. Anything else is refused — never opened in place (04 §11.2).
//! 2. **Open** the right [`GoDbSource`] for the detected backend.
//! 3. **Create** a fresh RocksDB directory named [`current_db_dir_name`]
//!    (`CURRENT_DATABASE` = `v1.4.5`) under the destination root — the Rust
//!    node's single on-disk backend (04 §2.1) under the Go-compatible folder
//!    convention (26 §6).
//! 4. **Stream** every pair into it via [`migrate`](crate::migrate::migrate),
//!    byte-for-byte (no key/value transformation — the whole 04 §10 catalog
//!    rides along untouched).
//! 5. **Verify** the copy at the requested [`VerifyLevel`].
//!
//! ## Why this lives next to (not inside) `migrate`
//!
//! [`migrate`](crate::migrate::migrate) stays a pure, target-agnostic driver
//! (`MemDb` in tests, `RocksDb` in production) with **no** RocksDB or filesystem
//! coupling. The facade is the part that *names and creates* the RocksDB dir, so
//! the RocksDB-touching entry points ([`import_go_dir`],
//! [`import_source_into_rocksdb`]) are gated behind the `rocksdb` feature, while
//! the feature-free pieces ([`detect_backend`], [`current_db_dir_name`],
//! [`GoBackend`], [`ImportError`], [`ImportReport`]) are always available so the
//! node-side refusal logic (`ava-node`) can use them without pulling RocksDB.

use std::path::{Path, PathBuf};

#[cfg(doc)]
use crate::migrate::GoDbSource;
use crate::migrate::leveldb::GOLEVELDB_DIR_NAME;
#[cfg(doc)]
use crate::migrate::migrate;
use crate::migrate::pebble::PEBBLE_DIR_NAME;
use crate::migrate::verify::VerifyLevel;
#[cfg(doc)]
use crate::traits::DynDatabase;

/// The on-disk RocksDB directory name the Rust node opens: `CURRENT_DATABASE`
/// (`v1.4.5`, 26 §6). The import tool creates the destination dir under this
/// name so the migrated DB is exactly what the node will open on first start.
///
/// The folder name *is* the schema version (26 §6); RocksDB sharing the
/// `v1.4.5` name with goleveldb is intentional — both are "schema v1.4.5", just
/// different engine file formats inside.
#[must_use]
pub fn current_db_dir_name() -> &'static str {
    ava_version::CURRENT_DATABASE
}

/// The Go base-DB backend behind a data directory (04 §11.3 / 26 §6), detected
/// by its schema-version folder name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoBackend {
    /// goleveldb (`<data-dir>/v1.4.5/`), the pre-v1.10.15 default.
    Goleveldb,
    /// Pebble (`<data-dir>/pebble/`), the v1.10.15+ default.
    Pebble,
}

impl GoBackend {
    /// The schema-version subdirectory name this backend lives under.
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            GoBackend::Goleveldb => GOLEVELDB_DIR_NAME,
            GoBackend::Pebble => PEBBLE_DIR_NAME,
        }
    }
}

/// Import / detection failures (04 §11, 26 §6). A per-crate `thiserror` enum so
/// callers (the node-side refusal in `ava-node`, the M12 CLI) can match the
/// failure mode.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The data directory is neither a goleveldb (`v1.4.5/`) nor a Pebble
    /// (`pebble/`) schema-version folder — a foreign/unknown backend. Per
    /// 04 §11.2 / 26 §6 the node **refuses** rather than opening it in place
    /// (which would corrupt it).
    #[error(
        "unsupported data directory {path}: no `{}` (goleveldb) or `{}` (pebble) \
         schema-version folder found. The Rust node cannot open a Go data dir in \
         place; run the offline import tool first (04 §11) or bootstrap fresh \
         from the network (04 §11.5).",
        GOLEVELDB_DIR_NAME,
        PEBBLE_DIR_NAME
    )]
    UnsupportedDir {
        /// The directory that could not be classified.
        path: PathBuf,
    },

    /// The source directory does not exist or is not a directory.
    #[error("source data directory {path} does not exist or is not a directory")]
    MissingSource {
        /// The offending path.
        path: PathBuf,
    },

    /// A post-migration [`verify`](crate::migrate::verify::verify) check failed.
    #[error("post-import verification failed: {0}")]
    Verify(#[from] crate::migrate::verify::VerifyError),

    /// An underlying source-open / copy / RocksDB / I/O failure.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result alias for the import facade.
pub type Result<T> = std::result::Result<T, ImportError>;

/// Detects the Go base-DB backend behind `data_dir` from its schema-version
/// folder layout (26 §6 / 04 §11.3): a `v1.4.5/` subdir ⇒ goleveldb, a
/// `pebble/` subdir ⇒ Pebble.
///
/// This is the load-bearing **migration-trigger / refusal** primitive: the
/// schema-version folder name is how a foreign/older dir is detected so the node
/// imports rather than opens in place (26 §6). It does **not** open the engine
/// or read any KV bytes.
///
/// # Errors
///
/// - [`ImportError::MissingSource`] if `data_dir` is absent or not a directory.
/// - [`ImportError::UnsupportedDir`] if neither schema-version folder is present
///   (a foreign/unknown backend — refuse, never open in place).
pub fn detect_backend(data_dir: &Path) -> Result<GoBackend> {
    if !data_dir.is_dir() {
        return Err(ImportError::MissingSource {
            path: data_dir.to_path_buf(),
        });
    }

    // goleveldb is checked first: its folder name (`v1.4.5`) is also what a
    // RocksDB dir we wrote uses, but detection runs only against a *source* Go
    // dir, so a present `v1.4.5/` here means goleveldb. `--db-type` (M12 CLI)
    // overrides; this is the auto-detect default (04 §11.3).
    if data_dir.join(GOLEVELDB_DIR_NAME).is_dir() {
        return Ok(GoBackend::Goleveldb);
    }
    if data_dir.join(PEBBLE_DIR_NAME).is_dir() {
        return Ok(GoBackend::Pebble);
    }
    Err(ImportError::UnsupportedDir {
        path: data_dir.to_path_buf(),
    })
}

/// Knobs for an import run (04 §11.4). Defaults: verify the load-bearing
/// surfaces ([`VerifyLevel::Roots`]), no resume, sidecar resolved from `PATH`.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Post-migration verification tier (04 §11.4). Default [`VerifyLevel::Roots`].
    pub verify: VerifyLevel,
    /// `--resume`: skip pairs already copied past this cursor (04 §11.4). The
    /// recorded [`MIGRATION_CURSOR_KEY`](crate::migrate::MIGRATION_CURSOR_KEY)
    /// from an interrupted run goes here.
    pub resume_after: Option<Vec<u8>>,
    /// Explicit Pebble export-sidecar binary path (`--sidecar`), overriding the
    /// `PATH` lookup of [`SIDECAR_BIN`](crate::migrate::pebble::SIDECAR_BIN).
    pub sidecar: Option<PathBuf>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            verify: VerifyLevel::Roots,
            resume_after: None,
            sidecar: None,
        }
    }
}

/// The outcome of a successful import (04 §11.4), for operator logging.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// The detected/declared source backend.
    pub backend: GoBackend,
    /// The created RocksDB directory (named [`current_db_dir_name`]).
    pub dst_dir: PathBuf,
    /// The number of `(key, value)` pairs copied (excluding the migration
    /// cursor checkpoint).
    pub pairs_copied: u64,
    /// The verification tier that was run.
    pub verify: VerifyLevel,
}

/// Opens the right [`GoDbSource`] for `backend` over `src_dir` and streams every
/// pair into a fresh RocksDB directory named [`current_db_dir_name`] under
/// `dst_root` (the full 04 §11 import: detect ⇒ open ⇒ create ⇒ copy ⇒ verify).
///
/// `src_dir` is the **node data directory** (the parent of the schema-version
/// folder); the source engine is opened at `src_dir/<backend dir>`.
///
/// Auto-detection lives in [`detect_backend`]; pass its result here (the M12 CLI
/// also lets `--db-type` override it). Verification uses the structural flat-KV
/// check only; merkleized [`RootVerifier`](crate::migrate::verify::RootVerifier)s
/// are injected by the CLI in M12 (this facade keeps `ava-database` free of any
/// `ava-merkledb`/Firewood dependency — 04 §11.4).
///
/// # Errors
///
/// Propagates source-open, copy, RocksDB-open, filesystem, and verification
/// failures as [`ImportError`].
#[cfg(feature = "rocksdb")]
pub fn import_go_dir(
    src_dir: &Path,
    dst_root: &Path,
    backend: GoBackend,
    opts: &ImportOptions,
) -> Result<ImportReport> {
    use crate::migrate::GoDbSource;
    use crate::migrate::leveldb::RocksDbCompatSource;
    use crate::migrate::pebble::PebbleSidecarSource;

    let engine_dir = src_dir.join(backend.dir_name());
    if !engine_dir.is_dir() {
        return Err(ImportError::MissingSource { path: engine_dir });
    }

    // Open the backend-specific source. The leveldb fast path opens the dir with
    // the RocksDB backend in leveldb-compat mode; Pebble streams via the Go
    // export sidecar (spawn wired in M12 — documented stub today).
    let source: Box<dyn GoDbSource> = match backend {
        GoBackend::Goleveldb => Box::new(RocksDbCompatSource::open(&engine_dir)?),
        GoBackend::Pebble => match &opts.sidecar {
            Some(bin) => Box::new(PebbleSidecarSource::with_sidecar(&engine_dir, bin)),
            None => Box::new(PebbleSidecarSource::new(&engine_dir)),
        },
    };

    import_source_into_rocksdb(source.as_ref(), dst_root, backend, opts)
}

/// The RocksDB-target core of [`import_go_dir`]: creates a fresh RocksDB dir
/// named [`current_db_dir_name`] under `dst_root`, streams every pair from
/// `source` into it via [`migrate`](crate::migrate::migrate), and verifies.
///
/// Exposed so callers can inject any [`GoDbSource`] (the M12 CLI's resolved
/// reader; an in-memory test source) — the dir naming, the verbatim copy, and
/// the verify tier are identical regardless of how the source was opened.
///
/// # Errors
///
/// Propagates RocksDB-open, copy, filesystem, and verification failures.
#[cfg(feature = "rocksdb")]
pub fn import_source_into_rocksdb(
    source: &dyn crate::migrate::GoDbSource,
    dst_root: &Path,
    backend: GoBackend,
    opts: &ImportOptions,
) -> Result<ImportReport> {
    use std::sync::Arc;

    use crate::migrate::{migrate, verify::verify};
    use crate::traits::DynDatabase;

    std::fs::create_dir_all(dst_root)
        .map_err(|e| ImportError::Other(anyhow::anyhow!("create dst root {dst_root:?}: {e}")))?;
    let dst_dir = dst_root.join(current_db_dir_name());

    let db = crate::rocksdb::RocksDb::open(&dst_dir)
        .map_err(|e| ImportError::Other(anyhow::anyhow!("open RocksDB {dst_dir:?}: {e}")))?;
    let dst: Arc<dyn DynDatabase> = Arc::new(db);

    let resume = opts.resume_after.as_deref();
    let pairs_copied = migrate(source, dst.as_ref(), resume)?;

    // Roots tier (and above): structural flat-KV check. Merkleized root
    // re-derivation is injected by the M12 CLI; none here keeps the facade free
    // of merkledb/Firewood (04 §11.4).
    let verifiers: Vec<Arc<dyn crate::migrate::verify::RootVerifier>> = Vec::new();
    verify(dst.as_ref(), opts.verify, &verifiers)?;

    Ok(ImportReport {
        backend,
        dst_dir,
        pairs_copied,
        verify: opts.verify,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_db_dir_name_is_v145() {
        assert_eq!(current_db_dir_name(), "v1.4.5");
    }

    #[test]
    fn backend_dir_names() {
        assert_eq!(GoBackend::Goleveldb.dir_name(), "v1.4.5");
        assert_eq!(GoBackend::Pebble.dir_name(), "pebble");
    }

    // The folder-detection tests synthesize directories with `tempfile`, which
    // is linked only with the `rocksdb` feature in this crate; gate them so the
    // feature-free build (which still exposes `detect_backend`) stays clean.
    #[cfg(feature = "rocksdb")]
    #[test]
    fn detect_goleveldb_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("v1.4.5")).expect("mkdir");
        assert_eq!(
            detect_backend(dir.path()).expect("detect"),
            GoBackend::Goleveldb
        );
    }

    #[cfg(feature = "rocksdb")]
    #[test]
    fn detect_pebble_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("pebble")).expect("mkdir");
        assert_eq!(
            detect_backend(dir.path()).expect("detect"),
            GoBackend::Pebble
        );
    }

    #[cfg(feature = "rocksdb")]
    #[test]
    fn detect_refuses_unknown_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("v1.0.0")).expect("mkdir");
        let err = detect_backend(dir.path()).expect_err("must refuse");
        assert!(matches!(err, ImportError::UnsupportedDir { .. }));
    }

    #[test]
    fn detect_refuses_missing_dir() {
        let err = detect_backend(std::path::Path::new("/nonexistent/ava-rs/import"))
            .expect_err("must refuse");
        assert!(matches!(err, ImportError::MissingSource { .. }));
    }

    #[test]
    fn default_options_verify_roots() {
        let opts = ImportOptions::default();
        assert_eq!(opts.verify, VerifyLevel::Roots);
        assert!(opts.resume_after.is_none());
        assert!(opts.sidecar.is_none());
    }
}
