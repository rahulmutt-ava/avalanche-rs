// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init steps 7 and 10 (specs/12 §2.2): the node's Prometheus multi-gatherer
//! (mirror Go `initMetrics`) and the `/ext/metrics` route (mirror Go
//! `initMetricsAPI`).

use std::sync::Arc;

use ava_api::metrics::{Gatherer, LabelGatherer, MultiGatherer, PrefixGatherer, metrics_handler};
use ava_api::server::ApiServer;

use crate::error::Result;
use crate::init::namespace;

/// The node's metric gatherers (Go `n.MetricsGatherer` +
/// `n.MeterDBMetricsGatherer`).
pub struct NodeMetrics {
    /// The root prefix gatherer every subsystem namespace registers against.
    pub gatherer: Arc<PrefixGatherer>,
    /// The per-chain (`chain` label) meterdb gatherer, registered under
    /// `avalanche_meterdb`.
    pub meter_db: Arc<LabelGatherer>,
}

/// Step 7: build the prefix gatherer and register the meterdb label gatherer
/// (mirror Go `initMetrics`).
///
/// # Errors
/// Propagates a duplicate-namespace registration (impossible on a fresh
/// gatherer; the `Result` mirrors Go).
pub fn init_metrics() -> Result<NodeMetrics> {
    let gatherer = Arc::new(PrefixGatherer::new());
    let meter_db = Arc::new(LabelGatherer::new(ava_api::metrics::CHAIN_LABEL));
    gatherer.register(
        &namespace::meterdb(),
        Arc::clone(&meter_db) as Arc<dyn Gatherer>,
    )?;
    Ok(NodeMetrics { gatherer, meter_db })
}

/// Step 10: mount `/ext/metrics` (mirror Go `initMetricsAPI`).
///
/// Registers the `avalanche_process` namespace for parity with Go; the Go
/// process/go-runtime collectors have no portable Rust equivalent yet
/// (`tests/PORTING.md`). Skipped entirely (with an info log) when
/// `--api-metrics-enabled=false`.
///
/// # Errors
/// Propagates namespace registration and route-mount failures.
pub fn init_metrics_api(
    enabled: bool,
    metrics: &NodeMetrics,
    api_server: &dyn ApiServer,
) -> Result<()> {
    if !enabled {
        tracing::info!("skipping metrics API initialization because it has been disabled");
        return Ok(());
    }

    // Go registers the process + go-runtime collectors under
    // `avalanche_process`. The Rust `prometheus` process collector is
    // Linux-only; the namespace is registered for layout parity and the
    // collectors are a documented deferral.
    let _process_registry =
        ava_api::metrics::make_and_register(metrics.gatherer.as_ref(), &namespace::process())?;

    tracing::info!("initializing metrics API");
    api_server.add_route(
        metrics_handler(Arc::clone(&metrics.gatherer) as Arc<dyn Gatherer>),
        "metrics",
        "",
    )?;
    Ok(())
}
