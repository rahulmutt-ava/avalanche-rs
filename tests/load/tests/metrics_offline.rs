// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.18 offline arm 2 — Prometheus exposition parser + SLO threshold logic
//! (specs/00 §7.3 metric-name parity; specs/18 §2 catalog; specs/02 §10.3 SLOs).
//! Pure Rust, runs every CI run (no feature, not `#[ignore]`).

#![allow(unused_crate_dependencies)]

use ava_load::REQUIRED_PARITY_METRICS;
use ava_load::metrics::{
    Exposition, ParseError, SloMeasurement, SloThresholds, slo_holds, slo_violations,
};
use pretty_assertions::assert_eq;

const GOOD: &str = include_str!("fixtures/ext_metrics_good.prom");
const REGRESSED: &str = include_str!("fixtures/ext_metrics_regressed.prom");

/// The parser extracts the right samples, labels and values from a real-shaped
/// `/ext/metrics` exposition (comments + blank lines skipped, labels parsed).
#[test]
fn parser_extracts_samples_and_labels() {
    let exp = Exposition::parse(GOOD).expect("parse good exposition");

    // A bare (label-free) gauge.
    let peers = exp
        .sample("avalanche_network_peers", &[])
        .expect("avalanche_network_peers present");
    assert_eq!(peers.value, 5.0, "peers gauge value");
    assert!(peers.labels.is_empty(), "peers has no labels");

    // A labelled counter — selected by its label set, value parsed.
    let sent = exp
        .sample("avalanche_network_msgs", &[("io", "sent"), ("op", "put")])
        .expect("sent msgs sample present");
    assert_eq!(sent.value, 120_034.0, "sent msgs value");
    assert!(
        sent.has_label("compressed", "false"),
        "all labels are parsed, not just the queried ones"
    );

    // `sum` aggregates across label sets.
    let total_msgs = exp.sum("avalanche_network_msgs");
    assert_eq!(
        total_msgs, 240_014.0,
        "sum() aggregates sent + received msgs"
    );
}

/// The required parity metric names (specs/00 §7.3) are all present in a healthy
/// scrape — the naming-parity guard.
#[test]
fn parity_metric_names_are_present() {
    let exp = Exposition::parse(GOOD).expect("parse good exposition");
    let missing = exp.missing_parity_names(REQUIRED_PARITY_METRICS);
    assert!(
        missing.is_empty(),
        "all parity metric names present; missing: {missing:?}"
    );

    // A scrape that dropped a family is caught.
    let stripped: String = GOOD
        .lines()
        .filter(|l| !l.starts_with("avalanche_evm_blocks"))
        .collect::<Vec<_>>()
        .join("\n");
    let exp = Exposition::parse(&stripped).expect("parse stripped exposition");
    let missing = exp.missing_parity_names(REQUIRED_PARITY_METRICS);
    assert_eq!(
        missing,
        vec!["avalanche_evm_blocks"],
        "a dropped parity family is reported missing"
    );
}

/// The SLO verdict passes on a good run and fails on a regressed one, and
/// itemizes exactly which dimension(s) failed.
#[test]
fn slo_logic_passes_good_fails_regressed() {
    // SLOs: >= 200 tx/s sustained, <= 2000 ms latency, 0 errors.
    let thresholds = SloThresholds::new(200.0, 2_000.0);

    // --- good run, derived from the good scrape -----------------------------
    let exp = Exposition::parse(GOOD).expect("parse good");
    let good = measurement_from(&exp);
    assert_eq!(good.errors, 0, "good run has zero parse-failure errors");
    assert!(
        slo_holds(&good, &thresholds),
        "good run satisfies all SLOs: {good:?}"
    );
    assert!(
        slo_violations(&good, &thresholds).is_empty(),
        "good run has no SLO violations"
    );

    // --- regressed run ------------------------------------------------------
    let exp = Exposition::parse(REGRESSED).expect("parse regressed");
    let bad = measurement_from(&exp);
    assert!(bad.errors > 0, "regressed run has non-zero errors");
    assert!(
        !slo_holds(&bad, &thresholds),
        "regressed run violates the SLOs: {bad:?}"
    );
    let violations = slo_violations(&bad, &thresholds);
    assert_eq!(
        violations.len(),
        3,
        "regressed run fails throughput, latency AND errors: {violations:?}"
    );
}

/// Each SLO dimension is independently enforced.
#[test]
fn each_slo_dimension_is_enforced() {
    let thresholds = SloThresholds::new(100.0, 1_000.0);

    let ok = SloMeasurement {
        throughput_tps: 150.0,
        latency_ms: 500.0,
        errors: 0,
    };
    assert!(slo_holds(&ok, &thresholds), "all dimensions within bounds");

    // Throughput too low.
    let slow = SloMeasurement {
        throughput_tps: 99.0,
        ..ok
    };
    assert!(!slo_holds(&slow, &thresholds), "low throughput fails");

    // Latency too high.
    let laggy = SloMeasurement {
        latency_ms: 1_001.0,
        ..ok
    };
    assert!(!slo_holds(&laggy, &thresholds), "high latency fails");

    // A single error fails the zero-error SLO.
    let errored = SloMeasurement { errors: 1, ..ok };
    assert!(!slo_holds(&errored, &thresholds), "any error fails");
}

/// The parser rejects malformed sample lines.
#[test]
fn parser_rejects_malformed_lines() {
    assert!(
        matches!(
            Exposition::parse("avalanche_network_peers"),
            Err(ParseError::MissingValue(_))
        ),
        "a sample with no value is rejected"
    );
    let bad_value = "avalanche_network_peers not_a_number";
    assert!(
        matches!(
            Exposition::parse(bad_value),
            Err(ParseError::InvalidValue { .. })
        ),
        "a non-numeric value is rejected"
    );
    let unterminated = r#"avalanche_network_msgs{io="sent" 5"#;
    assert!(
        matches!(
            Exposition::parse(unterminated),
            Err(ParseError::UnterminatedLabels(_))
        ),
        "an unterminated label block is rejected"
    );
}

/// Derive an [`SloMeasurement`] from a scrape exactly the way the live arm does:
/// throughput from accepted blocks, latency from the snowman accept averager
/// (`_sum`/`_count`, ns→ms), errors from the parse-failure counter.
fn measurement_from(exp: &Exposition) -> SloMeasurement {
    // Throughput proxy: C-Chain blocks accepted over an assumed 30s window.
    let blocks = exp.sum("avalanche_evm_blocks");
    let throughput_tps = blocks / 30.0;

    // Latency: snowman issuance->acceptance averager (ns) -> ms.
    let count = exp.sum("avalanche_snowman_blks_accepted_count");
    let sum_ns = exp.sum("avalanche_snowman_blks_accepted_sum");
    let latency_ms = if count > 0.0 {
        (sum_ns / count) / 1_000_000.0
    } else {
        0.0
    };

    // Errors: messages that failed to parse over the run.
    let errors = exp.sum("avalanche_network_msgs_failed_to_parse") as u64;

    SloMeasurement {
        throughput_tps,
        latency_ms,
        errors,
    }
}
