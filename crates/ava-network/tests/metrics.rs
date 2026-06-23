// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Metric-name parity test for `ava-network` (M2.20).
//!
//! Registers the network + per-peer metric structs into a fresh
//! `prometheus::Registry` and asserts the gathered family names match the
//! byte-exact Go catalog in `specs/18-metrics-and-logging.md` §2.1–§2.3.
//! These names are a frozen compatibility surface scraped by operator
//! dashboards: any rename/relabel here is a protocol break.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};

use prometheus::Registry;
use prometheus::proto::MetricType;

use ava_network::metrics::Metrics;
use ava_network::peer::metrics::PeerMetrics;

/// Gathers `reg` into a `name -> (type, sorted label keys)` map, sampling one
/// metric per family for the label-key set (every series in a Vec shares the
/// same key set).
fn schema(reg: &Registry) -> BTreeMap<String, (MetricType, Vec<String>)> {
    let mut out = BTreeMap::new();
    for fam in reg.gather() {
        let label_keys = fam
            .get_metric()
            .first()
            .map(|m| {
                m.get_label()
                    .iter()
                    .map(|l| l.get_name().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.insert(
            fam.get_name().to_string(),
            (fam.get_field_type(), label_keys),
        );
    }
    out
}

#[test]
fn metric_names_match_go() {
    let reg = Registry::new();
    let metrics = Metrics::new(&reg).expect("register network metrics");
    let peer_metrics = PeerMetrics::new(&reg).expect("register peer metrics");

    // Touch one series per labelled family so it materialises in `gather()`.
    metrics.touch_for_test();
    peer_metrics.touch_for_test();

    let schema = schema(&reg);
    let names: BTreeSet<&str> = schema.keys().map(String::as_str).collect();

    // §2.1 network-level families (subset asserted; full set registered).
    for name in [
        "peers",
        "tracked",
        "peers_subnet",
        "time_since_last_msg_received",
        "time_since_last_msg_sent",
        "send_fail_rate",
        "times_connected",
        "times_disconnected",
        "accept_failed",
        "inbound_conn_throttler_allowed",
        "tls_conn_rejected",
        "outbound_tls_conn_upgrade_failed",
        "num_useless_peerlist_bytes",
        "inbound_conn_throttler_rate_limited",
        "node_uptime_weighted_average",
        "node_uptime_rewarding_stake",
        "peer_connected_duration_average",
    ] {
        assert!(names.contains(name), "missing network metric `{name}`");
    }

    // §2.3 throttler gauges/counters.
    for name in [
        "bandwidth_throttler_inbound_awaiting_acquire",
        "buffer_throttler_inbound_awaiting_acquire",
        "byte_throttler_inbound_remaining_at_large_bytes",
        "byte_throttler_inbound_remaining_validator_bytes",
        "byte_throttler_inbound_awaiting_acquire",
        "byte_throttler_inbound_awaiting_release",
        "throttler_total_waits",
        "throttler_total_no_waits",
        "throttler_awaiting_acquire",
        "throttler_outbound_acquire_successes",
        "throttler_outbound_acquire_failures",
        "throttler_outbound_remaining_at_large_bytes",
        "throttler_outbound_remaining_validator_bytes",
        "throttler_outbound_awaiting_release",
    ] {
        assert!(names.contains(name), "missing throttler metric `{name}`");
    }

    // §2.2 per-peer message I/O families.
    for name in [
        "round_trip_count",
        "round_trip_sum",
        "clock_skew_count",
        "clock_skew_sum",
        "msgs_failed_to_parse",
        "msgs_failed_to_send",
        "msgs",
        "msgs_bytes",
        "msgs_bytes_saved",
    ] {
        assert!(names.contains(name), "missing per-peer metric `{name}`");
    }

    // `msgs` carries io/op/compressed; `msgs_bytes` the same; `msgs_bytes_saved`
    // io/op; `msgs_failed_to_send` op — byte-exact label keys (§2.2).
    assert_eq!(
        schema["msgs"].1,
        vec!["compressed".to_string(), "io".to_string(), "op".to_string()],
        "msgs label keys"
    );
    assert_eq!(
        schema["msgs_bytes"].1,
        vec!["compressed".to_string(), "io".to_string(), "op".to_string()],
        "msgs_bytes label keys"
    );
    assert_eq!(
        schema["msgs_bytes_saved"].1,
        vec!["io".to_string(), "op".to_string()],
        "msgs_bytes_saved label keys"
    );
    assert_eq!(
        schema["msgs_failed_to_send"].1,
        vec!["op".to_string()],
        "msgs_failed_to_send label keys"
    );

    // `peers_subnet` carries the `subnetID` label (§2.1).
    assert_eq!(
        schema["peers_subnet"].1,
        vec!["subnetID".to_string()],
        "peers_subnet label keys"
    );

    // Spot-check types: peers is a gauge, times_connected a counter.
    assert_eq!(schema["peers"].0, MetricType::GAUGE);
    assert_eq!(schema["times_connected"].0, MetricType::COUNTER);
    assert_eq!(schema["tls_conn_rejected"].0, MetricType::COUNTER);
}
