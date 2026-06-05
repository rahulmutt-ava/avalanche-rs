// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The sentinel error model, mirroring `database/errors.go`.
//!
//! Go exposes exactly two sentinels — `ErrClosed = "closed"` and
//! `ErrNotFound = "not found"` — matched via `errors.Is`. In Rust they are
//! enum variants matched directly; the `#[error(...)]` strings are byte-exact
//! with Go so any code that formats them (logs, RPC mapping) is identical.
//!
//! Any other backend failure (RocksDB, IO, gRPC, corruption) is
//! [`Error::Other`]. `corruptabledb` poisons on [`Error::Other`] only — never
//! on [`Error::Closed`]/[`Error::NotFound`], which are control flow, not poison
//! (04 §2.6, 27 §6.1).

/// The two sentinel errors plus a catch-all, mirroring `database/errors.go`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An operation was attempted after the database was closed
    /// (`database.ErrClosed`).
    #[error("closed")]
    Closed,
    /// A `get` was attempted for a key that does not exist
    /// (`database.ErrNotFound`).
    #[error("not found")]
    NotFound,
    /// Any backend-specific failure (RocksDB, IO, gRPC, corruption).
    /// `corruptabledb` poisons on *these* only (not `Closed`/`NotFound`).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Display` text must be byte-exact with `database/errors.go` so any
    /// log/RPC formatting stays identical across the Go and Rust nodes.
    #[test]
    fn error_variants() {
        assert_eq!(Error::NotFound.to_string(), "not found");
        assert_eq!(Error::Closed.to_string(), "closed");

        // The catch-all is transparent (forwards the inner error's Display).
        let other = Error::Other(anyhow::anyhow!("disk on fire"));
        assert_eq!(other.to_string(), "disk on fire");

        // Variants are matchable directly (the Rust analog of errors.Is).
        assert!(matches!(Error::Closed, Error::Closed));
        assert!(matches!(Error::NotFound, Error::NotFound));
    }
}
