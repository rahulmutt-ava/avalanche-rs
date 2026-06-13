// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Node assembly (specs/12 §2/§7/§8, specs/17 §1/§2/§4).
//!
//! This crate hosts the `node/` + `app/` + `main/` equivalents of avalanchego:
//!
//! - [`node`] — [`node::Node`] + `Node::new`, the 26-step Go-faithful
//!   initialization sequence (specs/12 §2.2, M8.29).
//! - [`init`] — one module per init concern (identity, metrics, NAT, API
//!   server, database, networking, chain manager, VMs, aliases, indexer, …).
//! - [`error`] — the per-step typed error enum mirroring Go `node.New`'s
//!   error wraps.
//! - [`trace`] — the OpenTelemetry wiring (specs/12 §7, 18 §6). [`trace::new`]
//!   builds an OTLP exporter (gRPC or HTTP) wrapped by `tracing-opentelemetry`,
//!   or a no-op tracer when `--tracing-exporter-type=disabled`.
//! - [`nat`] — the NAT port-mapping seam (specs/12 §8, 17 §2 task #23). The
//!   `Router` trait + UPnP / no-op routers + the `Mapper` are reused from
//!   `ava-network`; this crate adds the NAT-PMP (RFC 6886) router and the
//!   `dynamicip` updater that feeds the network's advertised IP.
//! - [`logging`] — the bridge from the resolved `ava_config` logging block to
//!   the `ava-logging` factory + the [`logging::LogFactory`] registry
//!   (specs/18 §5).
//!
//! - [`dispatch`] — [`Node::dispatch`], the run loop (process-context write,
//!   API + warn task spawn, manual peer tracking, P2P event loop; specs/12
//!   §2.3, M8.30).
//! - [`shutdown`] — [`Node::shutdown`], the 14-step teardown run exactly once
//!   (specs/12 §2.4, 17 §4.3/§4.4, M8.30).
//!
//! `Node` owns the root `CancellationToken` tree (17 §4.1) and the task tracker
//! that dispatch + shutdown drive.
//!
//! [`Node::dispatch`]: crate::node::Node::dispatch
//! [`Node::shutdown`]: crate::node::Node::shutdown

#![forbid(unsafe_code)]

pub mod dispatch;
pub mod error;
pub mod init;
pub mod logging;
pub mod nat;
pub mod node;
pub mod shutdown;
pub mod trace;

#[cfg(test)]
pub(crate) mod testutil;
