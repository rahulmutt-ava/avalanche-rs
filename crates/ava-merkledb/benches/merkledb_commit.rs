// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Critical-path merkledb bench (specs/02 §9): insert a representative batch of
//! key/value pairs into a fresh in-memory [`MerkleDb`] and commit it, computing
//! the merkle root — the "merkledb commit" hot path the perf gate watches.
//!
//! Configured for SHORT runs so `cargo xtask bench-guard` finishes well under a
//! minute; this is a perf-gate canary, not a precise micro-benchmark.

use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use ava_database::MemDb;
use ava_merkledb::{BatchOp, BranchFactor, MerkleDb};

/// Number of key/value pairs committed per iteration (small byte keys/values).
const BATCH: usize = 100;

/// Builds a representative batch of `BATCH` small key/value pairs.
fn build_ops() -> Vec<BatchOp> {
    (0..BATCH)
        .map(|i| {
            let key = (i as u32).to_be_bytes();
            let value = (i as u32).wrapping_mul(0x9e37_79b1).to_be_bytes();
            BatchOp::put(&key, &value)
        })
        .collect()
}

/// Inserts `ops` into a fresh in-memory merkledb and commits, returning the root.
fn insert_and_commit(ops: Vec<BatchOp>) {
    let base = Arc::new(MemDb::new());
    let db = MerkleDb::new(base, BranchFactor::TwoFiftySix).expect("MerkleDb::new()");
    let view = db.new_view(ops).expect("new_view()");
    view.commit().expect("commit()");
    let _ = black_box(db.get_merkle_root().expect("get_merkle_root()"));
}

fn bench(c: &mut Criterion) {
    let ops = build_ops();
    c.bench_function("merkledb_commit", |b| {
        b.iter(|| insert_and_commit(black_box(ops.clone())));
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_millis(500))
        .warm_up_time(Duration::from_millis(200));
    targets = bench
}
criterion_main!(benches);
