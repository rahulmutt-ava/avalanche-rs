// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The per-crate error enum (specs 12 §3, 00 §8).

/// Errors produced by the API server subsystem.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// A route's base/endpoint failed URL validation (Go's
    /// `url.ParseRequestURI`), or the resulting mount path was malformed.
    #[error("invalid route path {path:?}: {msg}")]
    InvalidPath {
        /// The offending base/endpoint path.
        path: String,
        /// Why it was rejected.
        msg: String,
    },

    /// A route or alias was registered under a path that is already taken
    /// (mirror Go `errAlreadyReserved`).
    #[error("API route {path:?} is already reserved")]
    AlreadyReserved {
        /// The conflicting path.
        path: String,
    },

    /// The HTTP listener could not be bound or accept connections.
    #[error("failed to bind/serve HTTP listener on {addr}: {source}")]
    Listen {
        /// The address the server tried to bind.
        addr: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// The crate result alias.
pub type Result<T> = std::result::Result<T, ApiError>;
