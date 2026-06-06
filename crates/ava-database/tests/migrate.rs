// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the R2 Go-data-dir import tool (`migrate` module,
//! 04 §11). Gated behind the `migrate` feature:
//!
//! ```sh
//! cargo test -p ava-database --features migrate --test migrate
//! ```
//!
//! The ingest target is [`MemDb`] (an in-memory [`Database`]): the driver is
//! generic over `&dyn Database`, so the byte-exactness / resume / verify logic
//! is identical for the `RocksDb` target (covered by the conformance suite — see
//! `crates/ava-database/docs/migration.md`). Using `MemDb` keeps the test
//! independent of the heavy RocksDB C++ FFI toolchain.
//!
//! The `unused_crate_dependencies` allow is unconditional: the package's other
//! deps are linked into every test binary but unused here (a known
//! false-positive of that lint for integration tests).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(feature = "migrate")]
mod unit {
    use std::sync::Arc;

    use ava_database::migrate::verify::{RootVerifier, VerifyError, VerifyLevel, verify};
    use ava_database::migrate::{GoDbSource, MIGRATION_CURSOR_KEY, migrate};
    use ava_database::{DynDatabase, MemDb};

    /// A `GoDbSource` stub yielding fixed `(key, value)` pairs in lexicographic
    /// key order — no real on-disk engine, so the test exercises the driver
    /// without leveldb/pebble I/O.
    struct StubSource {
        pairs: Vec<(Vec<u8>, Vec<u8>)>,
    }

    impl GoDbSource for StubSource {
        fn iter_all(&self) -> anyhow::Result<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)>>> {
            let mut pairs = self.pairs.clone();
            pairs.sort();
            Ok(Box::new(pairs.into_iter()))
        }
    }

    /// Returns the realistic seed pairs: a prefixdb-namespaced key (32-byte
    /// `SHA256(prefix) ‖ key`), an archivedb `^height` key (`uvarint(len) ‖ key
    /// ‖ BE(!height)`), and a merkledb-node-style key prefix — all derived from
    /// existing crate code so the bytes mirror what a Go node writes (04 §10).
    fn seed_pairs() -> Vec<(Vec<u8>, Vec<u8>)> {
        // prefixdb: on-disk key is SHA256(prefix) ‖ key.
        let mut prefixed = ava_database::make_prefix(b"chainID");
        prefixed.extend_from_slice(b"account-balance");

        // archivedb: uvarint(len(key)) ‖ key ‖ BigEndian(!height).
        // Reproduce the encoding inline to avoid a dep on ava-archivedb here.
        let user_key = b"utxo";
        let height: u64 = 42;
        let mut archive_key = Vec::new();
        archive_key.push(user_key.len() as u8); // uvarint of 4 is a single byte
        archive_key.extend_from_slice(user_key);
        archive_key.extend_from_slice(&(!height).to_be_bytes());

        // merkledb node key: a typical token-packed path prefix + node bytes.
        let mut merkle_key = vec![0x00u8]; // root-ish path prefix
        merkle_key.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);

        vec![
            (prefixed, b"\x00\x00\x00\x00\x00\x00\x03\xe8".to_vec()),
            (archive_key, b"\x00utxo-value".to_vec()),
            (merkle_key, b"node-bytes".to_vec()),
            (b"singleton".to_vec(), b"last-accepted-block-id".to_vec()),
            (Vec::new(), b"empty-key-value".to_vec()),
        ]
    }

    /// 04 §11.4: the loop never decodes a key or value, so every pair reads back
    /// byte-identical from the destination DB.
    #[test]
    fn migrate_preserves_bytes() {
        let pairs = seed_pairs();
        let src = StubSource {
            pairs: pairs.clone(),
        };
        let dst = MemDb::new();

        let count = migrate(&src, &dst, None).unwrap();
        assert_eq!(count, pairs.len() as u64);

        for (key, value) in &pairs {
            let got = dst.get(key).unwrap();
            assert_eq!(&got, value, "value mismatch for key {key:02x?}");
        }

        // The cursor checkpoint was written and points at the last key.
        let cursor = dst.get(MIGRATION_CURSOR_KEY).unwrap();
        let last = src.iter_all().unwrap().last().unwrap().0;
        assert_eq!(cursor, last);
    }

    /// 04 §11.4: a re-run with `--resume` past `MIGRATION_CURSOR_KEY` copies
    /// nothing (the destination already holds every pair).
    #[test]
    fn migrate_resumable() {
        let pairs = seed_pairs();
        let src = StubSource {
            pairs: pairs.clone(),
        };
        let dst = MemDb::new();

        // First full pass.
        let first = migrate(&src, &dst, None).unwrap();
        assert_eq!(first, pairs.len() as u64);

        // Resume past the recorded cursor: a no-op (every key <= cursor).
        let cursor = dst.get(MIGRATION_CURSOR_KEY).unwrap();
        let second = migrate(&src, &dst, Some(&cursor)).unwrap();
        assert_eq!(second, 0, "resume re-run must copy nothing");
    }

    /// A pluggable [`RootVerifier`] that recomputes a toy "root" by XOR-folding
    /// every byte of every value in the DB. The test injects it so `verify`
    /// stays decoupled from `ava-merkledb` (04 §11.4; concrete merkledb wiring
    /// lands with the CLI in M12).
    struct XorRootVerifier {
        /// The expected fold of an uncorrupted copy.
        expected: u8,
    }

    impl RootVerifier for XorRootVerifier {
        fn recompute_root(&self, dst: &dyn DynDatabase) -> anyhow::Result<Vec<u8>> {
            let mut fold = 0u8;
            let mut it = dst.new_iterator_with_start_and_prefix(&[], &[]);
            while it.next() {
                // Skip the migration metadata cursor — it is not migrated data.
                if it.key() == Some(MIGRATION_CURSOR_KEY) {
                    continue;
                }
                if let Some(v) = it.value() {
                    for b in v {
                        fold ^= *b;
                    }
                }
            }
            it.error()?;
            Ok(vec![fold])
        }

        fn expected_root(&self, _dst: &dyn DynDatabase) -> anyhow::Result<Vec<u8>> {
            Ok(vec![self.expected])
        }
    }

    fn xor_fold(pairs: &[(Vec<u8>, Vec<u8>)]) -> u8 {
        let mut fold = 0u8;
        for (_, v) in pairs {
            for b in v {
                fold ^= *b;
            }
        }
        fold
    }

    /// 04 §11.4: `verify(Roots)` re-derives the root via the pluggable verifier
    /// and fails when the destination copy is corrupted.
    #[test]
    fn verify_roots_detects_mismatch() {
        let pairs = seed_pairs();
        let expected = xor_fold(&pairs);

        // Clean copy: verify passes.
        let src = StubSource {
            pairs: pairs.clone(),
        };
        let good = MemDb::new();
        migrate(&src, &good, None).unwrap();
        let verifier: Arc<dyn RootVerifier> = Arc::new(XorRootVerifier { expected });
        verify(&good, VerifyLevel::Roots, std::slice::from_ref(&verifier)).unwrap();

        // Corrupted copy: flip a value byte, verify must fail with a root
        // mismatch.
        let corrupt = MemDb::new();
        migrate(&src, &corrupt, None).unwrap();
        corrupt.put(b"singleton", b"TAMPERED-block-id").unwrap();
        let err = verify(&corrupt, VerifyLevel::Roots, &[verifier]).unwrap_err();
        assert!(
            matches!(err, VerifyError::RootMismatch { .. }),
            "expected RootMismatch, got {err:?}"
        );
    }

    /// `VerifyLevel::None` is a no-op even on a corrupted copy (no verifier is
    /// consulted).
    #[test]
    fn verify_none_is_noop() {
        let dst = MemDb::new();
        dst.put(b"k", b"v").unwrap();
        verify(&dst, VerifyLevel::None, &[]).unwrap();
    }
}
