// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `sae`-namespace prometheus metrics (specs/18 §2.11; Go `sae/metrics.go`,
//! `844535b313` / `72adc639e6`).
//!
//! Go registers these via `metrics.MakeAndRegister(snowCtx.Metrics, "sae")` —
//! the snow context hands the VM a registry under the `sae` namespace. The Rust
//! VM-facing metrics-registry seam (a registerer on `ChainContext`) is the
//! node-assembly integration left to M8; this module provides the SAE half: a
//! [`SaeMetrics`] handle that registers the three parity metrics into a
//! caller-supplied [`Registry`] and samples them from the live [`Frontier`] +
//! the [`ava_saevm_blocks::in_memory_block_count`] GC counter at scrape time.
//!
//! | Metric | Type | Source |
//! |---|---|---|
//! | `last_settled_height` | G | [`Frontier::last_settled_height`] |
//! | `last_executed_height` | G | [`Frontier::last_executed_height`] |
//! | `in_memory_blocks` | G (`GaugeFunc`) | [`ava_saevm_blocks::in_memory_block_count`] |
//!
//! All three are **sampled at scrape time** (the `GaugeFunc` pattern Go uses for
//! `in_memory_blocks`, since GC has no event to hook; for the heights, sampling
//! is observationally identical to the event-driven `Gauge.Set`). The bare
//! metric names carry no `sae_` prefix: the `sae` namespace is applied by the
//! node's prefix gatherer at registration (`MakeAndRegister`, specs/18 §1).

use std::sync::Arc;

use prometheus::core::{Collector, Desc};
use prometheus::proto::MetricFamily;
use prometheus::{IntGauge, Opts, Registry};

use ava_saevm_blocks::in_memory_block_count;

use crate::frontier::Frontier;

/// The `sae`-namespace metrics handle (Go `sae.metrics`).
///
/// Holds the [`Frontier`] whose S/E heights it samples; [`register_into`] mints
/// the scrape-time collector and registers it into the caller's `sae` registry.
///
/// [`register_into`]: SaeMetrics::register_into
pub struct SaeMetrics {
    frontier: Arc<Frontier>,
}

impl SaeMetrics {
    /// Builds a `sae` metrics handle over `frontier`.
    #[must_use]
    pub fn new(frontier: Arc<Frontier>) -> Self {
        Self { frontier }
    }

    /// Registers the three `sae` metrics into `registry` (the `sae`-namespaced
    /// registry the node hands the VM, Go `MakeAndRegister(metrics, "sae")`).
    ///
    /// # Errors
    /// Propagates a [`prometheus::Error`] on duplicate registration or an
    /// invalid metric descriptor.
    pub fn register_into(&self, registry: &Registry) -> Result<(), prometheus::Error> {
        let collector = SaeCollector::new(Arc::clone(&self.frontier))?;
        registry.register(Box::new(collector))
    }
}

/// Scrape-time collector for the three `sae` gauges (the `GaugeFunc` pattern):
/// every [`collect`](Collector::collect) reads the live backing stores and
/// re-publishes them, so the exposed value is always current with no setter on
/// the hot path.
struct SaeCollector {
    frontier: Arc<Frontier>,
    last_settled_height: IntGauge,
    last_executed_height: IntGauge,
    in_memory_blocks: IntGauge,
}

impl SaeCollector {
    fn new(frontier: Arc<Frontier>) -> Result<Self, prometheus::Error> {
        Ok(Self {
            frontier,
            last_settled_height: IntGauge::with_opts(Opts::new(
                "last_settled_height",
                "Height of the latest settled (S) SAE block.",
            ))?,
            last_executed_height: IntGauge::with_opts(Opts::new(
                "last_executed_height",
                "Height of the latest SAE block whose async execution has completed (E).",
            ))?,
            in_memory_blocks: IntGauge::with_opts(Opts::new(
                "in_memory_blocks",
                "SAE blocks still live in memory (created but not yet garbage-collected).",
            ))?,
        })
    }
}

impl Collector for SaeCollector {
    fn desc(&self) -> Vec<&Desc> {
        let mut descs = self.last_settled_height.desc();
        descs.extend(self.last_executed_height.desc());
        descs.extend(self.in_memory_blocks.desc());
        descs
    }

    fn collect(&self) -> Vec<MetricFamily> {
        // Sample the live backing stores at scrape time. `try_from` saturates a
        // (practically impossible) height above `i64::MAX` rather than truncate
        // — the SAE crates forbid lossy `as` casts.
        self.last_settled_height
            .set(i64::try_from(self.frontier.last_settled_height()).unwrap_or(i64::MAX));
        self.last_executed_height
            .set(i64::try_from(self.frontier.last_executed_height()).unwrap_or(i64::MAX));
        self.in_memory_blocks.set(in_memory_block_count());

        let mut families = self.last_settled_height.collect();
        families.extend(self.last_executed_height.collect());
        families.extend(self.in_memory_blocks.collect());
        families
    }
}
