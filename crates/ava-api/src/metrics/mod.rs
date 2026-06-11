// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The metrics gatherer tree + `/ext/metrics` handler (mirror Go
//! `api/metrics/` — `multi_gatherer.go`, `prefix_gatherer.go`,
//! `label_gatherer.go` — and `utils/metric/namespace.go`; specs 18 §1, 12
//! §3.6, 14 §6).
//!
//! avalanchego does not use a single flat Prometheus registry: the node roots
//! a tree of gatherers at one [`PrefixGatherer`] and exposes the merged result
//! at `/ext/metrics` (18 §1.1):
//!
//! - **[`PrefixGatherer`]** merges child gatherers by rewriting each metric
//!   family name to `<prefix>_<name>` ([`append_namespace`]); registration is
//!   rejected if the prefix would create overlapping namespaces
//!   (`eitherIsPrefix`).
//! - **[`LabelGatherer`]** merges child gatherers by injecting a constant
//!   label (e.g. `chain="P"`) into every metric; per-chain metrics carry the
//!   [`CHAIN_LABEL`] label rather than a chain id in the name (18 §1.1).
//! - **[`make_and_register`]** mints a fresh [`prometheus::Registry`],
//!   registers it under a name, and hands it back for the subsystem to
//!   populate (Go `metrics.MakeAndRegister`).
//!
//! The merge semantics mirror Go's `prometheus.Gatherers.Gather`: same-name
//! families are folded together (rejecting type conflicts, duplicate label
//! names, and duplicate metrics), and the output is sorted by family name —
//! with metrics within a family sorted by label values — for byte-stable
//! scrapes (18 §7).

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use parking_lot::RwLock;
use prometheus::proto::MetricFamily;
use prometheus::{Encoder, Registry, TextEncoder};

use crate::server::BoxedHandler;

/// Separator between a namespace prefix and a metric name (mirror Go
/// `utils/metric/namespace.go` `NamespaceSeparator`). MUST be `_` (18 §1.2).
pub const NAMESPACE_SEP: &str = "_";

/// The namespace separator as a byte (Go `NamespaceSeparatorByte`), used by
/// the `eitherIsPrefix` boundary check.
const NAMESPACE_SEP_BYTE: u8 = b'_';

/// The application namespace root (Go `utils/constants.PlatformName`,
/// lowercased as in `node/node.go`); node-level namespaces are all
/// `avalanche_<subsystem>` (18 §1.1).
pub const PLATFORM_NAME: &str = "avalanche";

/// The per-chain label key (Go `chains.ChainLabel`); its value is the chain's
/// **primary alias** (e.g. `chain="P"`), never the raw chain id (18 §1.1).
pub const CHAIN_LABEL: &str = "chain";

/// The exposition content type served by `/ext/metrics` (Go `promhttp` /
/// `expfmt.FmtText`): `text/plain; version=0.0.4; charset=utf-8` (12 §3.6).
pub const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Joins a namespace prefix and a metric name with [`NAMESPACE_SEP`],
/// mirroring Go `utils/metric.AppendNamespace`: an empty side yields the
/// other side unchanged.
#[must_use]
pub fn append_namespace(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        suffix.to_string()
    } else if suffix.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}{NAMESPACE_SEP}{suffix}")
    }
}

/// Errors produced by the metrics gatherer tree.
///
/// The `OverlappingNamespaces` / `DuplicateGatherer` / `Register` messages are
/// byte-stable mirrors of the Go `api/metrics` error strings
/// (`errOverlappingNamespaces`, `errDuplicateGatherer`, `MakeAndRegister`).
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    /// A prefix registration would create overlapping namespaces (Go
    /// `prefix_gatherer.go` `errOverlappingNamespaces` + wrap).
    #[error("prefix could create overlapping namespaces: {prefix:?} conflicts with {existing:?}")]
    OverlappingNamespaces {
        /// The prefix being registered.
        prefix: String,
        /// The already-registered conflicting prefix.
        existing: String,
    },

    /// A label value was registered twice with the same [`LabelGatherer`] (Go
    /// `label_gatherer.go` `errDuplicateGatherer` + wrap).
    #[error(
        "attempt to register duplicate gatherer: for {label_name:?} with label {label_value:?}"
    )]
    DuplicateGatherer {
        /// The gatherer's label key (e.g. `chain`).
        label_name: String,
        /// The duplicate label value (e.g. `P`).
        label_value: String,
    },

    /// [`make_and_register`] failed to register the fresh registry (Go
    /// `MakeAndRegister`'s `couldn't register %q metrics` wrap).
    #[error("couldn't register {name:?} metrics: {source}")]
    Register {
        /// The name the registry was being registered under.
        name: String,
        /// The underlying registration error.
        source: Box<MetricsError>,
    },

    /// One or more children failed to gather, or the merged families were
    /// inconsistent (type conflict, duplicate label names, duplicate metrics
    /// — the checks Go's `prometheus.Gatherers.Gather` performs).
    #[error("failed to gather metrics: {0}")]
    Gather(String),

    /// An underlying `prometheus` crate error (registration/encoding).
    #[error(transparent)]
    Prometheus(#[from] prometheus::Error),
}

/// A source of metric families — the local mirror of Go's
/// `prometheus.Gatherer` interface (the Rust `prometheus` crate has no
/// gatherer trait of its own; its `Registry::gather` is an inherent method).
pub trait Gatherer: Send + Sync {
    /// Returns the current metric families.
    ///
    /// # Errors
    /// Returns [`MetricsError::Gather`] if a child gatherer failed or the
    /// merged families were inconsistent. Unlike Go (which returns partially
    /// filled families *and* an error), an error here carries no partial
    /// output; the `/ext/metrics` handler discards partial output on error
    /// either way (Go `promhttp` `HTTPErrorOnError`).
    fn gather(&self) -> Result<Vec<MetricFamily>, MetricsError>;
}

impl Gatherer for Registry {
    fn gather(&self) -> Result<Vec<MetricFamily>, MetricsError> {
        Ok(Registry::gather(self))
    }
}

/// A [`Gatherer`] that merges additionally-registered child gatherers (Go's
/// `MultiGatherer` interface, `multi_gatherer.go`).
pub trait MultiGatherer: Gatherer {
    /// Adds `gatherer`'s output to future [`Gatherer::gather`] calls under
    /// `name` (a namespace prefix for [`PrefixGatherer`], a label value for
    /// [`LabelGatherer`]).
    ///
    /// # Errors
    /// [`MetricsError::OverlappingNamespaces`] /
    /// [`MetricsError::DuplicateGatherer`] on a conflicting `name`.
    fn register(&self, name: &str, gatherer: Arc<dyn Gatherer>) -> Result<(), MetricsError>;

    /// Removes the gatherer registered under `name` from future gathers.
    /// Returns whether a gatherer with `name` was found (Go `Deregister`).
    fn deregister(&self, name: &str) -> bool;
}

/// Shared registration state (Go `multiGatherer`): parallel name/gatherer
/// vectors, deregistered by index.
#[derive(Default)]
struct Inner {
    names: Vec<String>,
    gatherers: Vec<Arc<dyn Gatherer>>,
}

impl Inner {
    fn register(&mut self, name: String, gatherer: Arc<dyn Gatherer>) {
        self.names.push(name);
        self.gatherers.push(gatherer);
    }

    fn deregister(&mut self, name: &str) -> bool {
        match self.names.iter().position(|n| n == name) {
            Some(index) => {
                self.names.remove(index);
                self.gatherers.remove(index);
                true
            }
            None => false,
        }
    }
}

/// A [`MultiGatherer`] that merges child gatherers by prefixing every family
/// name with `<prefix>_` (mirror `prefix_gatherer.go`; 18 §1.2).
#[derive(Default)]
pub struct PrefixGatherer {
    inner: RwLock<Inner>,
}

impl PrefixGatherer {
    /// Returns a new, empty prefix gatherer (Go `NewPrefixGatherer`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl MultiGatherer for PrefixGatherer {
    fn register(&self, name: &str, gatherer: Arc<dyn Gatherer>) -> Result<(), MetricsError> {
        let mut inner = self.inner.write();
        for existing in &inner.names {
            if either_is_prefix(name, existing) {
                return Err(MetricsError::OverlappingNamespaces {
                    prefix: name.to_string(),
                    existing: existing.clone(),
                });
            }
        }
        inner.register(
            name.to_string(),
            Arc::new(PrefixedGatherer {
                prefix: name.to_string(),
                gatherer,
            }),
        );
        Ok(())
    }

    fn deregister(&self, name: &str) -> bool {
        self.inner.write().deregister(name)
    }
}

impl Gatherer for PrefixGatherer {
    fn gather(&self) -> Result<Vec<MetricFamily>, MetricsError> {
        merge_gather(&self.inner.read().gatherers)
    }
}

/// A child of [`PrefixGatherer`]: renames every gathered family to
/// `<prefix>_<name>` (Go `prefixedGatherer`).
struct PrefixedGatherer {
    prefix: String,
    gatherer: Arc<dyn Gatherer>,
}

impl Gatherer for PrefixedGatherer {
    fn gather(&self) -> Result<Vec<MetricFamily>, MetricsError> {
        let mut families = self.gatherer.gather()?;
        for family in &mut families {
            let renamed = append_namespace(&self.prefix, family.get_name());
            family.set_name(renamed);
        }
        Ok(families)
    }
}

/// A [`MultiGatherer`] that merges child gatherers by injecting a constant
/// label (`<label_name>="<registered name>"`) into every metric (mirror
/// `label_gatherer.go`; 18 §1.2). Used per-chain with [`CHAIN_LABEL`].
pub struct LabelGatherer {
    label_name: String,
    inner: RwLock<Inner>,
}

impl LabelGatherer {
    /// Returns a new gatherer injecting the `label_name` label (Go
    /// `NewLabelGatherer`).
    #[must_use]
    pub fn new(label_name: impl Into<String>) -> Self {
        Self {
            label_name: label_name.into(),
            inner: RwLock::new(Inner::default()),
        }
    }
}

impl MultiGatherer for LabelGatherer {
    fn register(&self, name: &str, gatherer: Arc<dyn Gatherer>) -> Result<(), MetricsError> {
        let mut inner = self.inner.write();
        if inner.names.iter().any(|n| n == name) {
            return Err(MetricsError::DuplicateGatherer {
                label_name: self.label_name.clone(),
                label_value: name.to_string(),
            });
        }
        inner.register(
            name.to_string(),
            Arc::new(LabeledGatherer {
                label_name: self.label_name.clone(),
                label_value: name.to_string(),
                gatherer,
            }),
        );
        Ok(())
    }

    fn deregister(&self, name: &str) -> bool {
        self.inner.write().deregister(name)
    }
}

impl Gatherer for LabelGatherer {
    fn gather(&self) -> Result<Vec<MetricFamily>, MetricsError> {
        merge_gather(&self.inner.read().gatherers)
    }
}

/// A child of [`LabelGatherer`]: appends `<label_name>="<label_value>"` to
/// every gathered metric (Go `labeledGatherer`). A metric that already
/// carries the label key is NOT rejected here — exactly like Go, the
/// duplicate label name fails later, in the merge's consistency check.
struct LabeledGatherer {
    label_name: String,
    label_value: String,
    gatherer: Arc<dyn Gatherer>,
}

impl Gatherer for LabeledGatherer {
    fn gather(&self) -> Result<Vec<MetricFamily>, MetricsError> {
        let mut families = self.gatherer.gather()?;
        for family in &mut families {
            for metric in family.mut_metric() {
                let mut pair = prometheus::proto::LabelPair::default();
                pair.set_name(self.label_name.clone());
                pair.set_value(self.label_value.clone());
                let mut labels = metric.take_label();
                labels.push(pair);
                metric.set_label(labels);
            }
        }
        Ok(families)
    }
}

/// Gathers every child and folds same-name families together, mirroring Go's
/// `prometheus.Gatherers.Gather` consistency checks: a type conflict, a
/// metric with duplicate label names, or a duplicate metric (same family +
/// same label values) is an error. Errors from multiple children/families
/// are accumulated (Go's `MultiError`) and joined. The output is sorted by
/// family name — with each family's metrics sorted by label values — for
/// byte-stable scrapes (18 §7).
fn merge_gather(children: &[Arc<dyn Gatherer>]) -> Result<Vec<MetricFamily>, MetricsError> {
    let mut by_name: BTreeMap<String, MetricFamily> = BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut errs: Vec<String> = Vec::new();

    for child in children {
        let families = match child.gather() {
            Ok(families) => families,
            Err(err) => {
                errs.push(err.to_string());
                continue;
            }
        };
        for mut family in families {
            let name = family.get_name().to_string();
            let metrics = family.take_metric();
            let merged = match by_name.entry(name.clone()) {
                Entry::Occupied(occupied) => {
                    let existing = occupied.into_mut();
                    if existing.get_field_type() != family.get_field_type() {
                        errs.push(format!(
                            "gathered metric family {name:?} has inconsistent types"
                        ));
                        continue;
                    }
                    existing
                }
                Entry::Vacant(vacant) => vacant.insert(family),
            };
            for mut metric in metrics {
                // Sort label pairs by name (Go normalizes/hashes sorted
                // labels; sorted output is also what the text encoder emits).
                let mut labels = metric.take_label();
                labels.sort_by(|a, b| a.get_name().cmp(b.get_name()));
                if labels
                    .iter()
                    .zip(labels.iter().skip(1))
                    .any(|(a, b)| a.get_name() == b.get_name())
                {
                    errs.push(format!(
                        "gathered metric family {name:?} has two or more labels with the same name"
                    ));
                    continue;
                }
                // Identity = family name + sorted label pairs (Go's metric
                // hash); `\u{1}`/`\u{2}` separators cannot occur in names.
                let mut identity = name.clone();
                for label in &labels {
                    identity.push('\u{1}');
                    identity.push_str(label.get_name());
                    identity.push('\u{2}');
                    identity.push_str(label.get_value());
                }
                if !seen.insert(identity) {
                    errs.push(format!(
                        "gathered metric family {name:?} was collected before with the same name and label values"
                    ));
                    continue;
                }
                metric.set_label(labels);
                merged.mut_metric().push(metric);
            }
        }
    }

    if !errs.is_empty() {
        return Err(MetricsError::Gather(errs.join("; ")));
    }

    let mut families: Vec<MetricFamily> = by_name.into_values().collect();
    for family in &mut families {
        // Sort each family's metrics by their (sorted) label values, like
        // Go's NormalizeMetricFamilies metricSorter.
        family.mut_metric().sort_by(|a, b| {
            let key = |m: &prometheus::proto::Metric| {
                m.get_label()
                    .iter()
                    .map(|l| l.get_value().to_string())
                    .collect::<Vec<_>>()
            };
            key(a).cmp(&key(b))
        });
    }
    Ok(families)
}

/// Mints a fresh [`Registry`], registers it with `gatherer` under `name`, and
/// returns it for the subsystem to populate (mirror Go
/// `metrics.MakeAndRegister`). The returned registry shares state with the
/// registered one (`Registry` is internally reference-counted).
///
/// # Errors
/// Returns [`MetricsError::Register`] wrapping the underlying conflict.
pub fn make_and_register(
    gatherer: &dyn MultiGatherer,
    name: &str,
) -> Result<Registry, MetricsError> {
    let registry = Registry::new();
    gatherer
        .register(name, Arc::new(registry.clone()))
        .map_err(|source| MetricsError::Register {
            name: name.to_string(),
            source: Box::new(source),
        })?;
    Ok(registry)
}

/// Builds the `/ext/metrics` handler: a GET-only route serving the gathered
/// families in Prometheus text exposition format ([`METRICS_CONTENT_TYPE`]),
/// 500 on gather error (Go `promhttp.HandlerFor` with default
/// `HTTPErrorOnError`; 12 §3.6, 14 §6).
pub fn metrics_handler(gatherer: Arc<dyn Gatherer>) -> BoxedHandler {
    Router::new().route(
        "/",
        get(move || {
            let gatherer = Arc::clone(&gatherer);
            async move { serve_metrics(gatherer.as_ref()) }
        }),
    )
}

/// Serves one scrape: gather, then encode with [`TextEncoder`]. A gather or
/// encode failure yields a `500` whose body mirrors Go `promhttp`'s
/// `HTTPErrorOnError` (`http.Error` appends the trailing newline).
fn serve_metrics(gatherer: &dyn Gatherer) -> Response {
    let result = gatherer.gather().and_then(|families| {
        let mut buffer = Vec::new();
        TextEncoder::new().encode(&families, &mut buffer)?;
        Ok(buffer)
    });
    match result {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, METRICS_CONTENT_TYPE)],
            body,
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("An error has occurred while serving metrics:\n\n{err}\n"),
        )
            .into_response(),
    }
}

/// Registers the `process_*` collector (`process_cpu_seconds_total`, …) on
/// `registry` — node.go registers Go's equivalent under the
/// `avalanche_process` namespace (18 §4).
///
/// # Errors
/// Returns [`MetricsError::Prometheus`] if registration fails.
#[cfg(target_os = "linux")]
pub fn register_process_collector(registry: &Registry) -> Result<(), MetricsError> {
    let collector = prometheus::process_collector::ProcessCollector::for_self();
    registry.register(Box::new(collector))?;
    Ok(())
}

/// No-op off Linux: the `prometheus` crate's process collector reads `/proc`
/// and only exists on Linux (the production target). The `process_*` parity
/// assertion of the metrics-name golden test is Linux-only (18 §4); Go's
/// `go_*` runtime families have **no** Rust equivalent on any platform and
/// are a documented waiver — we never fake them.
///
/// # Errors
/// Never fails; the `Result` keeps the call-site identical across platforms.
#[cfg(not(target_os = "linux"))]
pub fn register_process_collector(registry: &Registry) -> Result<(), MetricsError> {
    let _ = registry;
    Ok(())
}

/// Whether either string is a prefix of the other **at a namespace
/// boundary** (mirror Go `prefix_gatherer.go` `eitherIsPrefix`): `hello` is a
/// prefix of `hello_world` but not of `helloworld`.
fn either_is_prefix(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() > b.len() { (b, a) } else { (a, b) };
    let (short, long) = (short.as_bytes(), long.as_bytes());
    long.starts_with(short) // `short` is a byte-prefix of `long`, and …
        && (short.is_empty() // … it is empty,
            || short.len() == long.len() // … they are equal,
            // … or it ends at a namespace boundary of `long`. The index is in
            // bounds: the two `||` arms above ruled out len() >= long.len().
            || long.get(short.len()) == Some(&NAMESPACE_SEP_BYTE))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use prometheus::{Gauge, IntCounter, IntCounterVec, Opts};
    use tower::ServiceExt;

    use super::*;

    /// Looks up a family by (already-prefixed) name.
    fn family<'a>(families: &'a [MetricFamily], name: &str) -> Option<&'a MetricFamily> {
        families.iter().find(|f| f.get_name() == name)
    }

    // ------------------------------------------------------------------
    // 18 §1.2: family `peers` under prefix `avalanche_network` exposes
    // `avalanche_network_peers` (separator `_`); overlapping-namespace
    // registration is rejected with the exact Go error string.
    // ------------------------------------------------------------------
    #[test]
    fn prefix_namespace() {
        let root = PrefixGatherer::new();
        let network =
            make_and_register(&root, "avalanche_network").expect("make_and_register(network)");
        let peers = Gauge::new("peers", "Number of network peers").expect("gauge");
        network.register(Box::new(peers)).expect("register peers");

        let families = root.gather().expect("gather");
        let names: Vec<&str> = families.iter().map(MetricFamily::get_name).collect();
        assert_eq!(
            names,
            vec!["avalanche_network_peers"],
            "PrefixGatherer must join with '_' (Go AppendNamespace)"
        );

        // `avalanche` is a namespace-boundary prefix of `avalanche_network`
        // => rejected (eitherIsPrefix).
        let err = root
            .register("avalanche", Arc::new(Registry::new()))
            .expect_err("overlapping prefix must be rejected");
        assert_eq!(
            err.to_string(),
            "prefix could create overlapping namespaces: \"avalanche\" conflicts with \"avalanche_network\"",
            "error string must match Go prefix_gatherer.go byte-for-byte"
        );

        // The registered prefix is a boundary-prefix of the new one => rejected.
        let err = root
            .register("avalanche_network_peers", Arc::new(Registry::new()))
            .expect_err("existing prefix of new prefix must be rejected");
        assert!(matches!(err, MetricsError::OverlappingNamespaces { .. }));

        // Same byte-prefix but NOT at a namespace boundary => accepted
        // ("hello" is not a prefix of "helloworld").
        root.register("avalanche_networking", Arc::new(Registry::new()))
            .expect("non-boundary sibling namespace is not overlapping");

        // Duplicate exact prefix => rejected.
        let err = root
            .register("avalanche_network", Arc::new(Registry::new()))
            .expect_err("duplicate prefix must be rejected");
        assert!(matches!(err, MetricsError::OverlappingNamespaces { .. }));
    }

    // Mirror Go TestEitherIsPrefix, both argument orders.
    #[test]
    fn either_is_prefix_table() {
        let cases = [
            ("", "", true),
            ("", "hello", true),
            ("x", "x", true),
            ("x", "y", false),
            ("hello", "hello_world", true),
            ("hello", "helloworld", false),
        ];
        for (a, b, expected) in cases {
            assert_eq!(
                either_is_prefix(a, b),
                expected,
                "either_is_prefix({a:?}, {b:?})"
            );
            assert_eq!(
                either_is_prefix(b, a),
                expected,
                "either_is_prefix({b:?}, {a:?})"
            );
        }
    }

    // ------------------------------------------------------------------
    // 18 §1.1: LabelGatherer("chain") injects chain="<alias>" into every
    // family; duplicate label values are rejected at registration with the
    // exact Go error string.
    // ------------------------------------------------------------------
    #[test]
    fn label_injection() {
        let chains = LabelGatherer::new(CHAIN_LABEL);

        let p = Registry::new();
        chains
            .register("P", Arc::new(p.clone()))
            .expect("register P");
        let p_polls =
            IntCounter::new("polls_successful", "Number of successful polls").expect("counter");
        p.register(Box::new(p_polls)).expect("register p counter");

        let x = Registry::new();
        chains
            .register("X", Arc::new(x.clone()))
            .expect("register X");
        let x_polls =
            IntCounter::new("polls_successful", "Number of successful polls").expect("counter");
        x_polls.inc();
        x.register(Box::new(x_polls)).expect("register x counter");

        let families = chains.gather().expect("gather");
        // Same-name families from both chains merge into ONE family carrying
        // both label values (Go prometheus.Gatherers.Gather folding).
        assert_eq!(families.len(), 1, "same-name families must merge");
        let fam = family(&families, "polls_successful").expect("polls_successful family");
        assert_eq!(fam.get_metric().len(), 2, "one metric per chain");
        let mut values: Vec<(String, f64)> = fam
            .get_metric()
            .iter()
            .map(|m| {
                let labels = m.get_label();
                assert_eq!(labels.len(), 1, "exactly the injected label");
                let label = labels.first().expect("one label pair");
                assert_eq!(label.get_name(), CHAIN_LABEL);
                (label.get_value().to_string(), m.get_counter().get_value())
            })
            .collect();
        values.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            values,
            vec![("P".to_string(), 0.0), ("X".to_string(), 1.0)],
            "chain label values + per-chain counts"
        );

        // Duplicate label value => exact Go error string.
        let err = chains
            .register("P", Arc::new(Registry::new()))
            .expect_err("duplicate label value must be rejected");
        assert_eq!(
            err.to_string(),
            "attempt to register duplicate gatherer: for \"chain\" with label \"P\"",
            "error string must match Go label_gatherer.go byte-for-byte"
        );
    }

    // A family already carrying the injected label key fails at GATHER time
    // (Go errors via Gatherers.Gather's consistency check — see
    // TestLabelGatherer_Gather "has overlap").
    #[test]
    fn label_injection_conflicting_label_errors() {
        let chains = LabelGatherer::new("smith");
        let reg = Registry::new();
        chains
            .register("rick", Arc::new(reg.clone()))
            .expect("register");
        let counter = IntCounterVec::new(
            Opts::new("counter", "help"),
            &["smith"], // the SAME label key the gatherer injects
        )
        .expect("counter vec");
        counter.with_label_values(&["a"]).inc();
        reg.register(Box::new(counter)).expect("register counter");

        let err = chains
            .gather()
            .expect_err("conflicting label key must fail the gather");
        assert!(matches!(err, MetricsError::Gather(_)), "got: {err:?}");
    }

    // make_and_register wires the returned registry into the parent and wraps
    // conflicts in the Go `couldn't register %q metrics` message.
    #[test]
    fn make_and_register_wires_and_rejects_duplicates() {
        let root = PrefixGatherer::new();
        let reg = make_and_register(&root, "avalanche_db").expect("first registration");
        let gauge = Gauge::new("size", "database size").expect("gauge");
        reg.register(Box::new(gauge)).expect("register gauge");

        let families = root.gather().expect("gather");
        assert!(
            family(&families, "avalanche_db_size").is_some(),
            "the returned registry must be live in the parent tree"
        );

        let err =
            make_and_register(&root, "avalanche_db").expect_err("duplicate name must be rejected");
        assert_eq!(
            err.to_string(),
            "couldn't register \"avalanche_db\" metrics: prefix could create overlapping namespaces: \"avalanche_db\" conflicts with \"avalanche_db\"",
            "wrap must match Go MakeAndRegister byte-for-byte"
        );
    }

    #[test]
    fn deregister_removes_gatherer() {
        let root = PrefixGatherer::new();
        let reg = make_and_register(&root, "avalanche_network").expect("register");
        let gauge = Gauge::new("peers", "peers").expect("gauge");
        reg.register(Box::new(gauge)).expect("register gauge");

        assert!(!root.deregister("unknown"), "unknown name => false");
        assert!(root.deregister("avalanche_network"), "known name => true");
        assert!(
            root.gather().expect("gather").is_empty(),
            "deregistered gatherer must not be gathered"
        );
        assert!(
            !root.deregister("avalanche_network"),
            "second deregister => false"
        );
    }

    // 18 §7: families are sorted by name for byte-stable scrapes.
    #[test]
    fn gather_sorts_families_by_name() {
        let root = PrefixGatherer::new();
        // Register in non-sorted order.
        let b = make_and_register(&root, "b").expect("b");
        b.register(Box::new(IntCounter::new("zz", "zz").expect("counter")))
            .expect("register");
        let a = make_and_register(&root, "a").expect("a");
        a.register(Box::new(IntCounter::new("yy", "yy").expect("counter")))
            .expect("register");

        let names: Vec<String> = root
            .gather()
            .expect("gather")
            .iter()
            .map(|f| f.get_name().to_string())
            .collect();
        assert_eq!(names, vec!["a_yy".to_string(), "b_zz".to_string()]);
    }

    // ------------------------------------------------------------------
    // /ext/metrics handler: GET-only, text exposition content type, 500 on
    // gather error (12 §3.6, 14 §6).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn handler_serves_text_exposition() {
        let root = Arc::new(PrefixGatherer::new());
        let network = make_and_register(root.as_ref(), "avalanche_network").expect("register");
        let peers = Gauge::new("peers", "Number of network peers").expect("gauge");
        network.register(Box::new(peers)).expect("register peers");

        let router = metrics_handler(root);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some(METRICS_CONTENT_TYPE),
            "content type must match Go promhttp (expfmt.FmtText)"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("avalanche_network_peers 0"),
            "exposition must contain the prefixed family, got: {text}"
        );

        // Non-GET is rejected (405) — the route is GET-only (14 §6).
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn handler_gather_error_is_500() {
        struct Failing;
        impl Gatherer for Failing {
            fn gather(&self) -> Result<Vec<MetricFamily>, MetricsError> {
                Err(MetricsError::Gather("boom".to_string()))
            }
        }

        let router = metrics_handler(Arc::new(Failing));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.starts_with("An error has occurred while serving metrics:\n\n"),
            "500 body must mirror Go promhttp HTTPErrorOnError, got: {text}"
        );
    }
}
