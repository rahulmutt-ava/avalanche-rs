// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `unit::local_signer_roundtrip` (M0.21).
//!
//! Mirrors avalanchego `utils/crypto/bls/signers/local/*_test.go`. Exercises the
//! `LocalSigner` lifecycle: generate, persist (32-byte raw, `0o400`/`0o700` on
//! Unix), reload, `from_file_or_persist_new`, and signing through the `Signer`
//! trait (SIG + POP ciphersuites). Also loads the committed Go-written
//! `signer.key` fixture (scalar = 1) and checks the derived public key + that
//! its signatures verify.

use ava_crypto::bls::{LocalSigner, SECRET_KEY_LEN, Signer, verify, verify_pop};

fn fixture_signer_key() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/crypto/signer.key"
    ))
    .expect("read signer.key")
}

mod unit {
    use super::*;

    #[test]
    fn local_signer_roundtrip() {
        let dir = tempdir();
        let path = dir.join("signer.key");

        // Generate, persist, reload — same public key.
        let signer = LocalSigner::generate().expect("generate");
        let pk_before = signer.public_key().compress();
        signer.to_file(&path).expect("to_file");

        // File is exactly 32 bytes.
        let bytes = std::fs::read(&path).expect("read persisted");
        assert_eq!(bytes.len(), SECRET_KEY_LEN);

        // Unix perms: file 0o400, dir 0o700.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let fmode = std::fs::metadata(&path)
                .expect("stat file")
                .permissions()
                .mode();
            assert_eq!(fmode & 0o777, 0o400, "key file must be 0o400");
            let dmode = std::fs::metadata(&dir)
                .expect("stat dir")
                .permissions()
                .mode();
            assert_eq!(dmode & 0o777, 0o700, "key dir must be 0o700");
        }

        let reloaded = LocalSigner::from_file(&path).expect("from_file");
        assert_eq!(reloaded.public_key().compress(), pk_before);

        // from_file_or_persist_new: reuse on the second call.
        let dir2 = tempdir();
        let path2 = dir2.join("nested").join("signer.key");
        let created = LocalSigner::from_file_or_persist_new(&path2).expect("persist new");
        let pk_created = created.public_key().compress();
        let reused = LocalSigner::from_file_or_persist_new(&path2).expect("reuse");
        assert_eq!(reused.public_key().compress(), pk_created);

        // Sign + verify via the Signer trait (SIG and POP ciphersuites differ).
        let msg = b"hello avalanche";
        let sig = signer.sign(msg).expect("sign");
        assert!(verify(signer.public_key(), &sig, msg), "SIG must verify");

        let pop = signer.sign_proof_of_possession(msg).expect("pop");
        assert!(
            verify_pop(signer.public_key(), &pop, msg),
            "POP must verify"
        );

        signer.shutdown().expect("shutdown is a no-op");
    }

    #[test]
    fn loads_go_written_signer_key() {
        let bytes = fixture_signer_key();
        assert_eq!(bytes.len(), SECRET_KEY_LEN);

        let signer = LocalSigner::from_bytes(&bytes).expect("from_bytes");

        // Round-trips back to the same 32 bytes (big-endian serialize).
        let dir = tempdir();
        let path = dir.join("signer.key");
        signer.to_file(&path).expect("to_file");
        assert_eq!(std::fs::read(&path).expect("read"), bytes);

        // The derived public key signs verifiably.
        let msg = b"pop test";
        let pop = signer.sign_proof_of_possession(msg).expect("pop");
        assert!(verify_pop(signer.public_key(), &pop, msg));
    }

    #[test]
    fn rejects_malformed_key() {
        // Wrong length / non-canonical scalar must error, not panic.
        assert!(LocalSigner::from_bytes(&[0u8; 16]).is_err());
    }
}

/// Create a fresh temp directory (avoids a `tempfile` dependency in this test).
fn tempdir() -> std::path::PathBuf {
    let mut base = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    base.push(format!(
        "ava-crypto-localsigner-{nanos}-{:?}",
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&base).expect("mkdir tmp");
    base
}
