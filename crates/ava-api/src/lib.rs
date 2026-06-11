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
//! Milestone **M8.17** adds the gorilla-`json2`-parity JSON-RPC 2.0 shim and
//! service registry ([`jsonrpc`]) plus the error model ([`error::json2_code`],
//! [`error::IntoJsonRpcError`]); the `#[rpc_service("name")]` macro
//! ([`ava_api_macros::rpc_service`]) generates the method registration. Full
//! chain mounting (M8.22) and the built-in `info`/`admin`/`health` services
//! (M8.18–M8.20) build on this surface.
//!
//! Milestone **M8.21** adds [`metrics`] — the gatherer tree mirroring Go
//! `api/metrics/` ([`metrics::PrefixGatherer`] / [`metrics::LabelGatherer`] /
//! [`metrics::make_and_register`]) and the `/ext/metrics` handler
//! ([`metrics::metrics_handler`]; specs 18 §1, 12 §3.6, 14 §6).

#![forbid(unsafe_code)]

pub mod error;
pub mod jsonrpc;
pub mod metrics;
pub mod middleware;
pub mod server;

pub use ava_api_macros::rpc_service;
pub use error::{ApiError, IntoJsonRpcError, JsonRpcError, Result, json2_code};
pub use jsonrpc::{BoxedRpcMethod, RpcError, ServiceRegistry, dispatch};
pub use metrics::{
    CHAIN_LABEL, Gatherer, LabelGatherer, MetricsError, MultiGatherer, NAMESPACE_SEP,
    PLATFORM_NAME, PrefixGatherer, make_and_register, metrics_handler,
};
pub use server::{ApiServer, BASE_URL, BoxedHandler, MAX_CONCURRENT_STREAMS, Server};
