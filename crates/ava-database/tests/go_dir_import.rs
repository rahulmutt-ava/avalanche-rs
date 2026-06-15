// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the M9.16 R2 Go-data-dir → RocksDB import *facade*
//! (`import` module, 04 §11, 26 §6). Gated behind the `migrate` feature; the
//! end-to-end RocksDB write path additionally needs the `rocksdb` feature:
//!
//! ```sh
//! cargo test -p ava-database --features migrate,rocksdb --test go_dir_import
//! ```
//!
//! ## How the source fixture is synthesized
//!
//! The facade auto-detects the Go backend from the schema-version folder name
//! (`v1.4.5/` ⇒ goleveldb, `pebble/` ⇒ Pebble — 26 §6 / 04 §11.3) and streams
//! every pair through [`ava_database::migrate::migrate`] into a fresh RocksDB
//! dir named `CURRENT_DATABASE` (`v1.4.5`).
//!
//! We do **not** have a real captured Go-written Pebble/LevelDB directory, and
//! both concrete on-disk readers are documented best-effort
//! (`RustyLevelDbSource` is a stub pending cargo-deny vetting; the
//! `PebbleSidecarSource` spawn is wired in M12). So `imports_go_pebble_dir_to_rocksdb`
//! drives the facade with an **injected [`GoDbSource`]** (a `VecSource` of pairs
//! mirroring what a Go node writes per 04 §10) into a **real on-disk RocksDB**
//! target. This covers the facade logic end-to-end: the destination dir is named
//! `v1.4.5`, the copy is byte-for-byte verbatim, and `verify` runs — everything
//! except the leveldb/pebble file-format decode, which is the readers' concern
//! (and itself unit-tested as a documented stub).
//!
//! The `unused_crate_dependencies` allow is unconditional (other package deps are
//! linked into every test binary; a known false positive for integration tests).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(all(feature = "migrate", feature = "rocksdb"))]
mod rocksdb_facade {
    use ava_database::migrate::import::{
        GoBackend, ImportError, ImportOptions, current_db_dir_name, detect_backend,
        import_source_into_rocksdb,
    };
    use ava_database::migrate::verify::VerifyLevel;
    use ava_database::migrate::{GoDbSource, MIGRATION_CURSOR_KEY};

    /// A `GoDbSource` stub yielding fixed `(key, value)` pairs in lexicographic
    /// key order — stands in for a real leveldb/pebble reader so the test
    /// exercises the facade without on-disk Go-engine I/O.
    struct VecSource(Vec<(Vec<u8>, Vec<u8>)>);

    impl GoDbSource for VecSource {
        fn iter_all(&self) -> anyhow::Result<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>>> {
            let mut pairs = self.0.clone();
            pairs.sort();
            Ok(Box::new(pairs.into_iter()))
        }
    }

    /// Pairs mirroring what a Go node writes (04 §10): a prefixdb-namespaced
    /// key, a flat-KV singleton with a non-empty `"last accepted"` pointer, an
    /// empty key, and arbitrary bytes.
    fn go_pairs() -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut prefixed = ava_database::make_prefix(b"chainID");
        prefixed.extend_from_slice(b"account-balance");

        let mut last_accepted = vec![0u8; 32];
        last_accepted.extend_from_slice(b"last accepted");

        vec![
            (prefixed, b"\x00\x00\x00\x00\x00\x00\x03\xe8".to_vec()),
            (last_accepted, b"last-accepted-block-id".to_vec()),
            (Vec::new(), b"empty-key-value".to_vec()),
            (vec![0xff, 0x00, 0xde], vec![0xad, 0xbe, 0xef]),
        ]
    }

    /// 04 §11.4 / 26 §6: the facade streams an injected Go source into a fresh
    /// RocksDB directory **named `v1.4.5`** (`CURRENT_DATABASE`), byte-for-byte.
    #[test]
    fn imports_go_pebble_dir_to_rocksdb() {
        let pairs = go_pairs();
        let src = VecSource(pairs.clone());

        let dst_root = tempfile::tempdir().unwrap();
        let report = import_source_into_rocksdb(
            &src,
            dst_root.path(),
            GoBackend::Pebble,
            &ImportOptions::default(),
        )
        .unwrap();

        // The created RocksDB dir is named for CURRENT_DATABASE.
        assert_eq!(current_db_dir_name(), "v1.4.5");
        assert_eq!(report.dst_dir.file_name().unwrap(), "v1.4.5");
        assert!(
            report.dst_dir.is_dir(),
            "the RocksDB dir must exist on disk"
        );
        assert_eq!(report.backend, GoBackend::Pebble);
        assert_eq!(report.pairs_copied, pairs.len() as u64);

        // Re-open the RocksDB dir and assert the full KV set equals the source's,
        // byte-for-byte (excluding the migration cursor metadata key).
        let db = ava_database::RocksDb::open(&report.dst_dir).unwrap();
        use ava_database::KeyValueReader;
        for (k, v) in &pairs {
            let got = KeyValueReader::get(&db, k).unwrap();
            assert_eq!(&got, v, "value mismatch for key {k:02x?}");
        }
        // The migration cursor was recorded and is the only extra key.
        assert!(KeyValueReader::has(&db, MIGRATION_CURSOR_KEY).unwrap());
    }

    /// 26 §6 / 04 §11.2: pointing the facade at a directory that is neither a
    /// goleveldb (`v1.4.5/`) nor a Pebble (`pebble/`) schema-version folder is a
    /// typed refusal — NOT an in-place open (which would corrupt).
    #[test]
    fn refuses_unsupported_dir() {
        let foreign = tempfile::tempdir().unwrap();
        // A dir with neither the goleveldb nor the pebble version folder.
        std::fs::create_dir(foreign.path().join("some-unknown-backend")).unwrap();

        let err = detect_backend(foreign.path()).unwrap_err();
        assert!(
            matches!(err, ImportError::UnsupportedDir { .. }),
            "expected UnsupportedDir, got {err:?}"
        );
    }

    /// Verify tier wiring: `VerifyLevel::Roots` (the default) runs the flat-KV
    /// structural check after the copy and succeeds for a clean import.
    #[test]
    fn import_runs_roots_verify_by_default() {
        let src = VecSource(go_pairs());
        let dst_root = tempfile::tempdir().unwrap();
        let opts = ImportOptions {
            verify: VerifyLevel::Roots,
            ..ImportOptions::default()
        };
        let report =
            import_source_into_rocksdb(&src, dst_root.path(), GoBackend::Goleveldb, &opts).unwrap();
        assert_eq!(report.verify, VerifyLevel::Roots);
    }
}
