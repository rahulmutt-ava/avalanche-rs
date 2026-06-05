// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `rpcdb` — a [`Database`](crate::Database) over gRPC (the rpcchainvm plugin
//! protocol), mirroring `database/rpcdb` (04 §2.8, 15 §3.4).
//!
//! The shared wire contract is `proto/rpcdb/rpcdb.proto`, generated into
//! `OUT_DIR` by `build.rs` (tonic/prost) and pulled in via
//! [`tonic::include_proto!`] below — **not committed** (01 §8.1). Because the
//! `.proto` is byte-exact with the Go tree, a Rust [`DatabaseClient`] and a Go
//! `rpcdb` server (or vice-versa) interoperate.
//!
//! ## Sync ↔ async bridge (04 §1.2)
//!
//! The [`Database`](crate::Database) trait surface is **synchronous**, but tonic
//! is async. Each [`DatabaseClient`] owns a current-thread tokio [`Runtime`] and
//! `block_on`s every RPC. This keeps the call-site-blocking discipline (blocking
//! DB work runs at the call site, never inside the trait) while reusing the
//! generated async tonic client. The server side ([`DatabaseServer`]) is a plain
//! tonic service driven by whatever runtime hosts the gRPC server.
//!
//! [`Runtime`]: tokio::runtime::Runtime

#[allow(
    missing_docs,
    dead_code,
    clippy::all,
    clippy::pedantic,
    unreachable_pub,
    clippy::doc_markdown
)]
mod pb {
    //! Generated tonic/prost types for the `rpcdb` package (see `build.rs`).
    tonic::include_proto!("rpcdb");
}

mod client;
mod server;

pub use client::DatabaseClient;
pub use server::DatabaseServer;

use crate::error::Error;

/// Maps a wire [`pb::Error`] enum value back to our sentinel error model
/// (`ErrEnumToError` in Go). `ERROR_UNSPECIFIED` ⇒ `Ok`.
pub(crate) fn err_enum_to_result(err: i32) -> Result<(), Error> {
    // `pb::Error` is `#[repr(i32)]`; match on the raw value to tolerate unknown
    // (open-enum) variants, treating anything unrecognized as "no error" exactly
    // as Go's map lookup would (a miss yields nil).
    match pb::Error::try_from(err) {
        Ok(pb::Error::Closed) => Err(Error::Closed),
        Ok(pb::Error::NotFound) => Err(Error::NotFound),
        Ok(pb::Error::Unspecified) | Err(_) => Ok(()),
    }
}

/// Maps our sentinel error model to the wire [`pb::Error`] enum value
/// (`ErrorToErrEnum` in Go). Only `Closed`/`NotFound` cross the wire as the
/// enum; everything else is surfaced as a gRPC status by the caller.
pub(crate) fn error_to_err_enum(err: &Error) -> i32 {
    match err {
        Error::Closed => pb::Error::Closed as i32,
        Error::NotFound => pb::Error::NotFound as i32,
        Error::Other(_) => pb::Error::Unspecified as i32,
    }
}
