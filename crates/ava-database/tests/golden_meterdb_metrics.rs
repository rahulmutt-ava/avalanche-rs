// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! meterdb metric-name golden (04 §2.5, 02 §7.3).
//!
//! Drives every wrapped method once, then gathers the `prometheus::Registry`
//! and asserts the registered metric names (`calls`/`duration`/`size`) and the
//! full `method`-label value set match the committed Go-extracted vector
//! (`tests/vectors/meterdb/metric_names.json`). This guards parity of the
//! observability surface with avalanchego's `database/meterdb`.
//!
//! The `unused_crate_dependencies` allow is unconditional (a known
//! false-positive of that lint for integration-test binaries).

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    unused_crate_dependencies
)]

/// The committed Go vector.
const VECTOR: &str = include_str!("vectors/meterdb/metric_names.json");

#[cfg(feature = "testutil")]
mod golden {
    use std::collections::BTreeSet;

    use ava_database::MemDb;
    use ava_database::meterdb::MeterDb;
    use ava_database::traits::{
        Batcher, Compacter, Database, Iteratee, Iterator, KeyValueDeleter, KeyValueReader,
        KeyValueWriter,
    };
    use prometheus::Registry;

    use super::VECTOR;

    /// Exercise every wrapped method so each `(metric, method-label)` series is
    /// instantiated.
    fn drive(db: &MeterDb<MemDb>) {
        let _ = db.has(b"k");
        db.put(b"k", b"v").unwrap();
        let _ = db.get(b"k");
        db.delete(b"k").unwrap();

        let mut b = db.new_batch();
        b.put(b"k", b"v").unwrap();
        b.delete(b"k").unwrap();
        let _ = b.size();
        b.write().unwrap();
        b.reset();
        let sink_db = MemDb::new();
        let mut sink = sink_db.new_batch();
        b.replay(sink.as_mut()).unwrap();
        let _ = b.inner();

        let mut it = db.new_iterator();
        it.next();
        it.error().unwrap();
        let _ = it.key();
        let _ = it.value();
        it.release();

        db.compact(None, None).unwrap();
        let _ = db.health_check();
        db.close().unwrap();
    }

    #[test]
    fn meterdb_metric_names() {
        let v: serde_json::Value = serde_json::from_str(VECTOR).unwrap();

        let reg = Registry::new();
        let db = MeterDb::new(&reg, MemDb::new()).unwrap();
        drive(&db);

        // Gather the registry and collect the metric names + method-label set.
        let mfs = reg.gather();
        let mut names = BTreeSet::new();
        let mut methods = BTreeSet::new();
        for mf in &mfs {
            names.insert(mf.get_name().to_string());
            for m in mf.get_metric() {
                for l in m.get_label() {
                    if l.get_name() == "method" {
                        methods.insert(l.get_value().to_string());
                    }
                }
            }
        }

        let want_names: BTreeSet<String> = v["metric_names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        let want_methods: BTreeSet<String> = v["method_labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();

        assert_eq!(names, want_names, "metric names diverge from Go vector");
        assert_eq!(
            methods, want_methods,
            "method-label set diverges from Go vector"
        );

        // The label name itself is fixed to "method".
        assert_eq!(v["label_name"].as_str().unwrap(), "method");
    }
}
