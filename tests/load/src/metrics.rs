// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Prometheus exposition parser + SLO threshold logic (M9.18; specs/00 §7.3
//! metric-name parity; specs/18 §2 metric catalog).
//!
//! The live arm scrapes a Rust node's `/ext/metrics` (the merged
//! `MultiGatherer` exposition, specs/18 §1) and must verify two things:
//!
//! 1. **Naming parity** — the parity-critical metric families Go exposes are
//!    present under the same `avalanche_<subsystem>_<name>` names (specs/00 §7.3,
//!    specs/18 §3 naming-parity rule). [`Exposition::has_metric`] /
//!    [`Exposition::assert_parity_names`] guard this against a representative
//!    fixture committed under `tests/fixtures/`.
//! 2. **SLOs** — throughput, latency and error counters meet thresholds:
//!    throughput ≥ min, p-latency ≤ max, **zero** errors (specs/02 §10.3).
//!    [`slo_holds`] is the pure verdict over already-extracted numbers.
//!
//! No floats are used to *parse* metric names or do consensus-adjacent work; the
//! SLO numbers themselves (throughput tx/s, latency ms) are operator-facing
//! observability values, not consensus state, so f64 is appropriate here and is
//! confined to this leaf module (specs/00 forbids floats only on
//! codec/consensus paths).

use std::collections::BTreeMap;

/// A single parsed Prometheus sample: a metric name, its (sorted) label set, and
/// the numeric value.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// The metric family name, e.g. `avalanche_network_msgs`.
    pub name: String,
    /// Label key→value pairs, sorted by key (so lookups are order-independent;
    /// avoids any `HashMap` iteration-order leak, specs/00 §6.1).
    pub labels: BTreeMap<String, String>,
    /// The sample value.
    pub value: f64,
}

impl Sample {
    /// Whether this sample carries `label=value`.
    #[must_use]
    pub fn has_label(&self, key: &str, value: &str) -> bool {
        self.labels.get(key).is_some_and(|v| v == value)
    }
}

/// A parsed Prometheus text-format exposition (one node's `/ext/metrics`).
#[derive(Debug, Clone, Default)]
pub struct Exposition {
    samples: Vec<Sample>,
}

/// Error parsing a Prometheus exposition line.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    /// A sample line had no value field.
    #[error("missing value on line: {0}")]
    MissingValue(String),
    /// A sample line's value did not parse as a float.
    #[error("invalid value {value:?} on line: {line}")]
    InvalidValue {
        /// The offending value token.
        value: String,
        /// The full offending line.
        line: String,
    },
    /// A label block was not terminated by `}`.
    #[error("unterminated label block on line: {0}")]
    UnterminatedLabels(String),
}

impl Exposition {
    /// Parse a Prometheus text-format exposition.
    ///
    /// Handles `# HELP` / `# TYPE` comment lines (skipped), blank lines, and
    /// sample lines of the form `name[{labels}] value [timestamp]`. Label values
    /// may be quoted and contain commas inside the quotes.
    ///
    /// # Errors
    /// Returns [`ParseError`] if a sample line is missing its value, has a
    /// non-numeric value, or an unterminated label block.
    pub fn parse(text: &str) -> Result<Exposition, ParseError> {
        let mut samples = Vec::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            samples.push(parse_sample(line)?);
        }
        Ok(Exposition { samples })
    }

    /// All parsed samples.
    #[must_use]
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    /// Whether any sample carries the metric family `name`.
    #[must_use]
    pub fn has_metric(&self, name: &str) -> bool {
        self.samples.iter().any(|s| s.name == name)
    }

    /// The first sample for `name` whose labels include every `(k, v)` in
    /// `labels`, or `None`.
    #[must_use]
    pub fn sample(&self, name: &str, labels: &[(&str, &str)]) -> Option<&Sample> {
        self.samples
            .iter()
            .find(|s| s.name == name && labels.iter().all(|(k, v)| s.has_label(k, v)))
    }

    /// The summed value of every sample for `name` (across label sets) — e.g.
    /// total messages handled regardless of `op`.
    #[must_use]
    pub fn sum(&self, name: &str) -> f64 {
        self.samples
            .iter()
            .filter(|s| s.name == name)
            .map(|s| s.value)
            .sum()
    }

    /// Assert every name in `required` is present; returns the list of *missing*
    /// names (empty ⇒ full parity). Pure, so the offline arm can table-test it.
    #[must_use]
    pub fn missing_parity_names<'a>(&self, required: &[&'a str]) -> Vec<&'a str> {
        required
            .iter()
            .copied()
            .filter(|name| !self.has_metric(name))
            .collect()
    }
}

/// Parse one `name[{labels}] value [timestamp]` sample line.
fn parse_sample(line: &str) -> Result<Sample, ParseError> {
    // Split off the labels block if present.
    let (name, labels, rest) = if let Some((name, after_brace)) = line.split_once('{') {
        // `after_brace` is `labels} value [ts]`; split on the closing brace.
        let (label_str, rest) = after_brace
            .split_once('}')
            .ok_or_else(|| ParseError::UnterminatedLabels(line.to_owned()))?;
        (name.to_owned(), parse_labels(label_str), rest.trim())
    } else {
        // No labels: `name value [ts]`.
        let mut it = line.splitn(2, char::is_whitespace);
        let name = it
            .next()
            .ok_or_else(|| ParseError::MissingValue(line.to_owned()))?
            .to_owned();
        let rest = it
            .next()
            .ok_or_else(|| ParseError::MissingValue(line.to_owned()))?
            .trim();
        (name, BTreeMap::new(), rest)
    };

    // The value is the first whitespace token of `rest` (a trailing timestamp,
    // if any, is ignored).
    let value_tok = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| ParseError::MissingValue(line.to_owned()))?;
    let value = parse_value(value_tok).ok_or_else(|| ParseError::InvalidValue {
        value: value_tok.to_owned(),
        line: line.to_owned(),
    })?;

    Ok(Sample {
        name,
        labels,
        value,
    })
}

/// Parse a `k="v",k2="v2"` label block into a sorted map. Tolerates unquoted
/// values and surrounding whitespace.
fn parse_labels(label_str: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in split_labels(label_str) {
        if let Some((k, v)) = pair.split_once('=') {
            let key = k.trim().to_owned();
            let val = v.trim().trim_matches('"').to_owned();
            if !key.is_empty() {
                out.insert(key, val);
            }
        }
    }
    out
}

/// Split a label block on commas that are *outside* quotes (so a quoted value
/// may itself contain a comma).
fn split_labels(label_str: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in label_str.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(ch);
            }
            ',' if !in_quotes => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    parts
}

/// Parse a Prometheus sample value, including the `+Inf`/`-Inf`/`NaN` specials.
fn parse_value(tok: &str) -> Option<f64> {
    match tok {
        "+Inf" => Some(f64::INFINITY),
        "-Inf" => Some(f64::NEG_INFINITY),
        "NaN" => Some(f64::NAN),
        other => other.parse::<f64>().ok(),
    }
}

/// The SLO thresholds a sustained-load run must satisfy (specs/02 §10.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SloThresholds {
    /// Minimum sustained throughput (accepted tx/s) that must be met or exceeded.
    pub min_throughput_tps: f64,
    /// Maximum acceptable p-latency (issuance→acceptance) in milliseconds.
    pub max_latency_ms: f64,
    /// Maximum tolerated error count over the run (the run requires **zero**).
    pub max_errors: u64,
}

impl SloThresholds {
    /// The default sustained-load SLOs: ≥ `min_tps` tx/s, ≤ `max_ms` latency,
    /// zero errors.
    #[must_use]
    pub fn new(min_throughput_tps: f64, max_latency_ms: f64) -> SloThresholds {
        SloThresholds {
            min_throughput_tps,
            max_latency_ms,
            max_errors: 0,
        }
    }
}

/// The measured outcome of a sustained-load run, extracted from a scrape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SloMeasurement {
    /// Observed sustained throughput (accepted tx/s).
    pub throughput_tps: f64,
    /// Observed p-latency (issuance→acceptance) in milliseconds.
    pub latency_ms: f64,
    /// Observed error count over the run.
    pub errors: u64,
}

/// The pure SLO verdict: throughput ≥ min, latency ≤ max, errors ≤ max (zero by
/// default). Returns `true` iff every threshold holds (specs/02 §10.3).
#[must_use]
pub fn slo_holds(measured: &SloMeasurement, thresholds: &SloThresholds) -> bool {
    measured.throughput_tps >= thresholds.min_throughput_tps
        && measured.latency_ms <= thresholds.max_latency_ms
        && measured.errors <= thresholds.max_errors
}

/// Itemize *which* SLOs a measurement violates (empty ⇒ all hold). Lets the
/// live arm report a precise failure message and the offline arm assert the
/// exact failing dimension.
#[must_use]
pub fn slo_violations(measured: &SloMeasurement, thresholds: &SloThresholds) -> Vec<String> {
    let mut out = Vec::new();
    if measured.throughput_tps < thresholds.min_throughput_tps {
        out.push(format!(
            "throughput {:.2} tps < min {:.2}",
            measured.throughput_tps, thresholds.min_throughput_tps
        ));
    }
    if measured.latency_ms > thresholds.max_latency_ms {
        out.push(format!(
            "latency {:.2} ms > max {:.2}",
            measured.latency_ms, thresholds.max_latency_ms
        ));
    }
    if measured.errors > thresholds.max_errors {
        out.push(format!(
            "errors {} > max {}",
            measured.errors, thresholds.max_errors
        ));
    }
    out
}
