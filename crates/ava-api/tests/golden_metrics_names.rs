// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Metrics-name parity golden test (specs 18 §3, with the §4 waivers).
//!
//! The committed snapshot `tests/vectors/api/metrics_schema.json` is emitted
//! by the **real Go gatherer tree** (`api/metrics/`) via the in-repo oracle
//! `tests/go-oracle/metrics_schema_oracle_test.go` (see `tests/PORTING.md`
//! for scope + the regeneration command). This test rebuilds the identical
//! tree with the Rust `ava_api::metrics` machinery and asserts the Rust
//! `/ext/metrics` schema `{(name, type, sorted(label_keys))}` is a
//! **superset** of every non-waived Go family.
//!
//! Waivers (documented, never silent — 18 §4):
//! - `…go_*`: Go-runtime collector families (GC, goroutines, memstats) have
//!   no Rust equivalent; we never fake them.
//! - `…process_*` off Linux: the Rust `process_*` collector is Linux-only
//!   (the production target); on macOS dev machines those rows are skipped.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ava_api::metrics::{
    CHAIN_LABEL, Gatherer, LabelGatherer, MultiGatherer, PrefixGatherer, append_namespace,
    make_and_register, register_process_collector,
};
use prometheus::proto::MetricType;
use prometheus::{Gauge, IntCounter, IntGaugeVec, Opts, Registry};
use serde::Deserialize;

/// One family row of the committed Go schema snapshot.
#[derive(Debug, Deserialize)]
struct FamilySchema {
    name: String,
    #[serde(rename = "type")]
    family_type: String,
    label_keys: Vec<String>,
}

/// The committed Go schema snapshot (provenance + family rows).
#[derive(Debug, Deserialize)]
struct Snapshot {
    avalanchego_commit: String,
    emitter: String,
    #[allow(dead_code)]
    goos: String,
    families: Vec<FamilySchema>,
}

fn metric_type_str(t: MetricType) -> &'static str {
    match t {
        MetricType::COUNTER => "counter",
        MetricType::GAUGE => "gauge",
        MetricType::SUMMARY => "summary",
        MetricType::UNTYPED => "untyped",
        MetricType::HISTOGRAM => "histogram",
    }
}

/// Builds the same gatherer tree the Go oracle builds (see
/// `tests/go-oracle/metrics_schema_oracle_test.go` — keep the two in sync)
/// and returns the Rust schema `{name -> (type, sorted label keys)}`.
fn rust_schema() -> BTreeMap<String, (String, Vec<String>)> {
    let root = PrefixGatherer::new();

    // node.go initMetricsAPI: the process/runtime collectors under
    // `avalanche_process`. Rust side: `process_*` on Linux only; no `go_*`
    // ever (18 §4).
    let process_namespace = append_namespace(ava_api::metrics::PLATFORM_NAME, "process");
    let process_reg =
        make_and_register(&root, &process_namespace).expect("make_and_register(process)");
    register_process_collector(&process_reg).expect("register_process_collector");

    // network/metrics.go representative families under `avalanche_network`
    // (18 §2.1: an unlabelled gauge + a labelled gauge vec).
    let network_reg =
        make_and_register(&root, "avalanche_network").expect("make_and_register(network)");
    let peers = Gauge::new("peers", "Number of network peers").expect("peers gauge");
    network_reg
        .register(Box::new(peers))
        .expect("register peers");
    let peers_subnet = IntGaugeVec::new(
        Opts::new(
            "peers_subnet",
            "Number of peers that are validating a particular subnet",
        ),
        &["subnetID"],
    )
    .expect("peers_subnet gauge vec");
    peers_subnet
        .with_label_values(&["11111111111111111111111111111111LpoYY"])
        .set(0);
    network_reg
        .register(Box::new(peers_subnet))
        .expect("register peers_subnet");

    // chains/manager.go wiring: a per-chain LabelGatherer("chain") registered
    // under the `avalanche_snowman` namespace; the chain registers its own
    // registry under its primary alias (18 §1.1, §2.8).
    let snowman = Arc::new(LabelGatherer::new(CHAIN_LABEL));
    root.register("avalanche_snowman", snowman.clone() as Arc<dyn Gatherer>)
        .expect("register snowman label gatherer");
    let p_chain = Registry::new();
    snowman
        .register("P", Arc::new(p_chain.clone()))
        .expect("register P chain");
    let polls_successful =
        IntCounter::new("polls_successful", "Number of successful polls").expect("counter");
    let polls_failed = IntCounter::new("polls_failed", "Number of failed polls").expect("counter");
    p_chain
        .register(Box::new(polls_successful))
        .expect("register polls_successful");
    p_chain
        .register(Box::new(polls_failed))
        .expect("register polls_failed");

    let families = root.gather().expect("root.gather()");
    families
        .iter()
        .map(|fam| {
            let keys: BTreeSet<String> = fam
                .get_metric()
                .iter()
                .flat_map(|m| m.get_label().iter().map(|l| l.get_name().to_string()))
                .collect();
            (
                fam.get_name().to_string(),
                (
                    metric_type_str(fam.get_field_type()).to_string(),
                    keys.into_iter().collect(),
                ),
            )
        })
        .collect()
}

/// Whether a Go snapshot family is waived rather than asserted (18 §4).
fn waived(name: &str) -> Option<&'static str> {
    // Go-runtime collector families (no Rust equivalent on any platform).
    if name.starts_with("go_") || name.starts_with("avalanche_process_go_") {
        return Some("go_* runtime collector (18 §4 waiver)");
    }
    // The Rust process collector is Linux-only.
    #[cfg(not(target_os = "linux"))]
    if name.starts_with("process_") || name.starts_with("avalanche_process_process_") {
        return Some("process_* collector is Linux-only in Rust (18 §4)");
    }
    None
}

#[test]
fn metrics_name_parity() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/api/metrics_schema.json"
    );
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read Go schema snapshot {path}: {e}"));
    let snapshot: Snapshot = serde_json::from_str(&raw).expect("parse metrics_schema.json");
    assert!(
        !snapshot.avalanchego_commit.is_empty() && !snapshot.emitter.is_empty(),
        "snapshot must carry provenance"
    );
    assert!(
        !snapshot.families.is_empty(),
        "snapshot must not be empty — regenerate per tests/PORTING.md"
    );

    let rust = rust_schema();

    let mut waived_rows = Vec::new();
    let mut failures = Vec::new();
    for fam in &snapshot.families {
        if let Some(reason) = waived(&fam.name) {
            waived_rows.push(format!("{} ({reason})", fam.name));
            continue;
        }
        match rust.get(&fam.name) {
            None => failures.push(format!(
                "missing family {:?} (Go type {:?}, labels {:?})",
                fam.name, fam.family_type, fam.label_keys
            )),
            Some((family_type, label_keys)) => {
                let mut want_keys = fam.label_keys.clone();
                want_keys.sort();
                if *family_type != fam.family_type {
                    failures.push(format!(
                        "family {:?}: type mismatch (Go {:?}, Rust {:?})",
                        fam.name, fam.family_type, family_type
                    ));
                }
                if *label_keys != want_keys {
                    failures.push(format!(
                        "family {:?}: label keys mismatch (Go {:?}, Rust {:?})",
                        fam.name, want_keys, label_keys
                    ));
                }
            }
        }
    }

    // The waivers must never silently swallow the whole snapshot.
    assert!(
        snapshot.families.len() > waived_rows.len(),
        "every snapshot family was waived — the test asserts nothing; \
         tighten the waiver or regenerate the snapshot"
    );
    assert!(
        failures.is_empty(),
        "Rust /ext/metrics schema is not a superset of the Go snapshot \
         (emitter {}, avalanchego @ {}):\n  {}\nwaived rows:\n  {}",
        snapshot.emitter,
        snapshot.avalanchego_commit,
        failures.join("\n  "),
        waived_rows.join("\n  "),
    );
}
