// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Node assembly submodules (specs/12 §2/§7/§8, specs/17 §2).
//!
//! This crate hosts the `node/` + `app/` + `main/` equivalents of avalanchego.
//! Task M8.28 lands the three observability/networking submodules the later
//! `Node::new` / `dispatch` / `shutdown` tasks consume:
//!
//! - [`trace`] — the OpenTelemetry wiring (specs/12 §7, 18 §6). [`trace::new`]
//!   builds an OTLP exporter (gRPC or HTTP) wrapped by `tracing-opentelemetry`,
//!   or a no-op tracer when `--tracing-exporter-type=disabled`.
//! - [`nat`] — the NAT port-mapping seam (specs/12 §8, 17 §2 task #23). The
//!   `Router` trait + UPnP / no-op routers + the `Mapper` are reused from
//!   `ava-network`; this crate adds the NAT-PMP (RFC 6886) router and the
//!   `dynamicip` updater that feeds the network's advertised IP.
//! - [`logging`] — the bridge from the resolved `ava_config` logging block to
//!   the `ava-logging` factory (specs/18 §5).
//!
//! `Node::new` (M8.29), dispatch/shutdown (M8.30) and the admin API (M8.19)
//! arrive in later tasks; this crate only exposes the factories + handles they
//! consume.

#![forbid(unsafe_code)]

pub mod logging;
pub mod nat;
pub mod trace;
