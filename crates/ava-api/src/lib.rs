// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-api` — the node HTTP server, JSON-RPC 2.0 shim, and built-in APIs
//! (specs 12 §3, 14 §1/§16).
//!
//! This crate mirrors Go's `api/server` plus the built-in `info`/`health`/
//! `admin`/`metrics` services. Milestone **M8.16** (this module set) implements
//! the **transport layer** only:
//!
//! - [`server::Server`] — the `axum`/`hyper`/`tower` HTTP server mounted under
//!   the base path `/ext`, with h2c, CORS, allowed-hosts, the `node-id`
//!   response header, the per-chain not-bootstrapped `503`, and the
//!   `HTTPConfig`-driven timeout layers.
//! - [`server::ApiServer`] — the trait the node assembles routes through
//!   (`add_route` / `add_aliases` / `register_chain` / `add_header_route` /
//!   `serve` / `shutdown`).
//! - [`middleware`] — the individual `tower`/`axum` middleware mirroring Go's
//!   `api/server` middleware one-for-one.
//!
//! The JSON-RPC 2.0 shim + service registry (M8.17) and full chain mounting
//! (M8.22) build on this surface.

#![forbid(unsafe_code)]

pub mod error;
pub mod middleware;
pub mod server;

pub use error::{ApiError, Result};
pub use server::{ApiServer, BASE_URL, BoxedHandler, MAX_CONCURRENT_STREAMS, Server};
