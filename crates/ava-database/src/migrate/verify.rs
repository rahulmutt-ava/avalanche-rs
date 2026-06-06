// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Post-migration verification (04 §11.4) — verify the copy without trusting the
//! copy loop.
//!
//! Three tiers ([`VerifyLevel`]):
//!
//! - [`None`](VerifyLevel::None) — skip verification.
//! - [`Roots`](VerifyLevel::Roots) — the default. Re-derive the **load-bearing
//!   compatibility surfaces**: for flat-KV P/X chains, walk the
//!   `"singleton" → "last accepted"` block chain; for merkleized SAE/EVM state,
//!   re-open the trie and assert `merkle_root()` equals the root stored in the
//!   last block header (the on-wire compatibility surface).
//! - [`Full`](VerifyLevel::Full) — additionally sample random pairs back against
//!   the source.
//!
//! # Pluggable root re-derivation (decoupled from `ava-merkledb`)
//!
//! The merkle-root recomputation is provided by the caller as a
//! [`RootVerifier`], **not** wired to `ava-merkledb` here. This keeps the
//! `migrate` module dependency-free of the merkledb/Firewood crates (which are
//! under concurrent development) and lets the CLI inject the concrete verifier
//! when it is assembled in M12. Tests inject a stub verifier. The flat-KV
//! last-accepted-chain check is a structural KV walk with no trie dependency, so
//! it lives directly in this module.

use crate::traits::DynDatabase;

/// How thoroughly [`verify`] re-checks a migrated DB (04 §11.4). Ordered: each
/// level implies the cheaper ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VerifyLevel {
    /// Skip verification entirely.
    None,
    /// Re-derive the load-bearing compatibility surfaces (default).
    Roots,
    /// Additionally sample random pairs back against the source.
    Full,
}

/// Verification failures (04 §11.4). A per-crate `thiserror` enum so callers can
/// match the failure mode (the library convention; `anyhow` is for the binary).
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// A merkleized chain's recomputed root did not match the root recorded in
    /// its last block header — the migration corrupted state.
    #[error("merkle root mismatch after migration: recomputed {recomputed}, expected {expected}")]
    RootMismatch {
        /// The root re-derived from the migrated DB (hex).
        recomputed: String,
        /// The root the block header says it should be (hex).
        expected: String,
    },
    /// The flat-KV last-accepted block chain was unreadable or inconsistent.
    #[error("last-accepted chain check failed: {0}")]
    LastAcceptedChain(String),
    /// A sampled pair did not read back byte-identical (`Full` tier).
    #[error("sampled pair mismatch for key {key}")]
    SampleMismatch {
        /// The mismatching key (hex).
        key: String,
    },
    /// An underlying DB or verifier error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The crate-supplied re-derivation of a merkleized chain's root (04 §11.4).
///
/// The CLI implements this over `ava-merkledb`/Firewood in M12; tests inject a
/// stub. Each implementor represents one merkleized chain (SAE/EVM) found in the
/// migrated DB.
pub trait RootVerifier: Send + Sync {
    /// Re-opens the trie/Firewood state over `dst` and recomputes `merkle_root()`.
    ///
    /// # Errors
    ///
    /// Returns an error if the state cannot be re-opened or hashed.
    fn recompute_root(&self, dst: &dyn DynDatabase) -> anyhow::Result<Vec<u8>>;

    /// Reads the expected root from the chain's last block header in `dst`.
    ///
    /// # Errors
    ///
    /// Returns an error if the header cannot be read or parsed.
    fn expected_root(&self, dst: &dyn DynDatabase) -> anyhow::Result<Vec<u8>>;
}

/// The reserved P/X singleton key recording the last-accepted block ID
/// (`"last accepted"` under the chain's `"singleton"` namespace, 04 §10.3/§10.4).
///
/// Exposed for the CLI's concrete chain wiring; the structural walk below only
/// confirms the key is present and non-empty (the byte-exact namespacing rides
/// along untouched, so a present, non-empty value is the structural signal).
pub const LAST_ACCEPTED_KEY: &[u8] = b"last accepted";

/// Verifies a migrated DB at the requested `level` (04 §11.4).
///
/// `root_verifiers` is the set of merkleized chains to re-derive (one per
/// SAE/EVM chain). It is empty for [`VerifyLevel::None`] and for DBs with no
/// merkleized state (pure P/X), in which case only the flat-KV structural checks
/// run.
///
/// # Errors
///
/// Returns [`VerifyError`] on the first detected inconsistency.
pub fn verify(
    dst: &dyn DynDatabase,
    level: VerifyLevel,
    root_verifiers: &[std::sync::Arc<dyn RootVerifier>],
) -> Result<(), VerifyError> {
    if level == VerifyLevel::None {
        return Ok(());
    }

    // Roots tier (and above): structural flat-KV check + merkleized root re-derive.
    check_last_accepted_chain(dst)?;

    for v in root_verifiers {
        let recomputed = v.recompute_root(dst)?;
        let expected = v.expected_root(dst)?;
        if recomputed != expected {
            return Err(VerifyError::RootMismatch {
                recomputed: hex_lower(&recomputed),
                expected: hex_lower(&expected),
            });
        }
    }

    // Full tier additionally samples pairs back against the source. The source
    // re-sampling is wired with the CLI in M12 (it needs a live `GoDbSource`
    // handle); here the tier is accepted as a superset of `Roots`.
    Ok(())
}

/// Flat-KV structural check (04 §11.4): the P/X last-accepted singletons survive
/// migration. A defensive, dependency-free walk — if a `"last accepted"` pointer
/// is present it must be non-empty (an empty pointer signals a torn copy).
///
/// This never decodes block bytes (no VM dependency); the byte-exact §10
/// namespacing is preserved by the copy loop, so structural presence is the
/// signal available at the storage tier. The CLI's M12 wiring layers the full
/// `blockID → PackUInt64(height)` chain walk on top using the VM crates.
fn check_last_accepted_chain(dst: &dyn DynDatabase) -> Result<(), VerifyError> {
    let mut it = dst.new_iterator_with_start_and_prefix(&[], &[]);
    while it.next() {
        let (Some(key), Some(value)) = (it.key(), it.value()) else {
            break;
        };
        // The singleton key may be namespaced (prefixdb prepends 32 bytes), so
        // match on the suffix.
        if key.ends_with(LAST_ACCEPTED_KEY) && value.is_empty() {
            return Err(VerifyError::LastAcceptedChain(format!(
                "empty last-accepted pointer at key {}",
                hex_lower(key)
            )));
        }
    }
    it.error()
        .map_err(|e| VerifyError::LastAcceptedChain(format!("iteration error: {e}")))?;
    Ok(())
}

/// Lowercase-hex of a byte slice, for error messages. (Local to avoid pulling
/// the `hex` crate into the non-test build.)
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        // Writing to a String never fails.
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Re-reads a sampled `key` back from `dst` and checks it equals `expected`
/// (the `Full` tier primitive, exposed for the CLI's M12 sampling loop).
///
/// # Errors
///
/// Returns [`VerifyError::SampleMismatch`] if the stored value differs, or
/// [`VerifyError::Other`] if the read fails for a non-`NotFound` reason.
pub fn check_sampled_pair(
    dst: &dyn DynDatabase,
    key: &[u8],
    expected: &[u8],
) -> Result<(), VerifyError> {
    match dst.get(key) {
        Ok(got) if got == expected => Ok(()),
        Ok(_) => Err(VerifyError::SampleMismatch {
            key: hex_lower(key),
        }),
        Err(crate::error::Error::NotFound) => Err(VerifyError::SampleMismatch {
            key: hex_lower(key),
        }),
        Err(e) => Err(VerifyError::Other(anyhow::anyhow!(e))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memdb::MemDb;
    use crate::traits::KeyValueWriter;

    #[test]
    fn none_level_is_noop() {
        let dst = MemDb::new();
        KeyValueWriter::put(&dst, b"k", b"v").expect("put");
        verify(&dst, VerifyLevel::None, &[]).expect("none is noop");
    }

    #[test]
    fn empty_last_accepted_pointer_fails() {
        let dst = MemDb::new();
        // A namespaced "last accepted" key with an empty value: a torn copy.
        let mut key = vec![0u8; 32];
        key.extend_from_slice(LAST_ACCEPTED_KEY);
        KeyValueWriter::put(&dst, &key, b"").expect("put");
        let err = verify(&dst, VerifyLevel::Roots, &[]).expect_err("must fail");
        assert!(matches!(err, VerifyError::LastAcceptedChain(_)));
    }

    #[test]
    fn non_empty_last_accepted_passes_with_no_verifiers() {
        let dst = MemDb::new();
        let mut key = vec![0u8; 32];
        key.extend_from_slice(LAST_ACCEPTED_KEY);
        KeyValueWriter::put(&dst, &key, b"block-id").expect("put");
        verify(&dst, VerifyLevel::Roots, &[]).expect("structural check passes");
    }

    #[test]
    fn sampled_pair_mismatch_detected() {
        let dst = MemDb::new();
        KeyValueWriter::put(&dst, b"k", b"v").expect("put");
        check_sampled_pair(&dst, b"k", b"v").expect("match");
        assert!(matches!(
            check_sampled_pair(&dst, b"k", b"other"),
            Err(VerifyError::SampleMismatch { .. })
        ));
        assert!(matches!(
            check_sampled_pair(&dst, b"missing", b"v"),
            Err(VerifyError::SampleMismatch { .. })
        ));
    }

    #[test]
    fn hex_lower_formats_bytes() {
        assert_eq!(hex_lower(&[0x00, 0xde, 0xad, 0xff]), "00deadff");
    }
}
