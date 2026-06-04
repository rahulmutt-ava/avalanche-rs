// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Generic golden-vector loader for the `tests/vectors/` corpus.
//!
//! Mirrors the Go test-vector oracle (specs/22 §6): each category directory
//! under `tests/vectors/<category>/` holds one or more JSON files of
//! `{input, expected}` records extracted from a pinned `avalanchego` commit
//! (provenance in each `MANIFEST.md` / the top-level `manifest.json`). Rust
//! `golden_*` tests load via [`load_vectors`] and assert byte/value equality.
//!
//! SCAFFOLD (tier-X task X.11): the typed [`Vector`] record and [`load_vectors`]
//! resolution are wired here; corpus `verify`/`diff`/`regen` and the `golden_*`
//! red→green pull on each subsystem are deepened in X.11/X.12 and the milestone
//! plans. The corpus + manifests already exist under `tests/vectors/`.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use serde::Deserialize;

/// Errors surfaced while loading the golden-vector corpus.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The category directory does not exist under `tests/vectors/`.
    #[error("no vectors found for category `{0}` (expected tests/vectors/{0}/)")]
    MissingCategory(String),
    /// A vector file could not be read.
    #[error("failed to read vector file {path}: {source}")]
    Io {
        /// The offending path.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// A vector file held invalid JSON for the requested record type.
    #[error("failed to parse vectors in {path}: {source}")]
    Parse {
        /// The offending path.
        path: String,
        /// Underlying deserialization error.
        source: serde_json::Error,
    },
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// One golden record: a deterministic `input` and its Go-produced `expected`.
#[derive(Debug, Clone, Deserialize)]
pub struct Vector<I, O> {
    /// The input fed to both implementations.
    pub input: I,
    /// The value avalanchego produced for `input` (the oracle).
    pub expected: O,
}

/// Absolute path to the repo's `tests/vectors/` directory.
///
/// Resolved relative to this crate's manifest dir so it works from any member
/// crate's test harness.
#[must_use]
pub fn vectors_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors")
}

/// Load and concatenate every `*.json` record in `tests/vectors/<category>/`.
///
/// SCAFFOLD: the schema-version / orphan / sha256 checks (X.11) run separately
/// via `xtask vectors verify`; this loader only concatenates typed records.
pub fn load_vectors<I, O>(category: &str) -> Result<Vec<Vector<I, O>>>
where
    I: for<'de> Deserialize<'de>,
    O: for<'de> Deserialize<'de>,
{
    let dir = vectors_root().join(category);
    if !dir.is_dir() {
        return Err(Error::MissingCategory(category.to_owned()));
    }

    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir).map_err(|source| Error::Io {
        path: dir.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        let records: Vec<Vector<I, O>> =
            serde_json::from_str(&raw).map_err(|source| Error::Parse {
                path: path.display().to_string(),
                source,
            })?;
        out.extend(records);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_category_is_reported() {
        let err = load_vectors::<String, String>("definitely-not-a-category").unwrap_err();
        assert!(matches!(err, Error::MissingCategory(_)));
    }

    #[test]
    fn vectors_root_points_at_corpus() {
        // The corpus directory exists in the repo (seeded in M0).
        assert!(vectors_root().join("rng").is_dir());
    }
}
