// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Critical-path mempool bench (specs/02 §9; M9.21): the X-Chain (AVM)
//! [`Mempool`] push → pop hot path. A batch of pre-built, conflict-free txs is
//! constructed ONCE outside the timed loop; each iteration creates a fresh
//! mempool, pushes the whole batch (FIFO `add`), then drains it front-to-back
//! (`peek` + `remove`) — the exact admit/drain path the block builder exercises.
//!
//! Configured for SHORT runs so `cargo xtask bench-guard` finishes well under a
//! minute; this is a perf-gate canary, not a precise micro-benchmark.

// The criterion macros expand to undocumented items.
#![allow(missing_docs)]
// Bench setup panics on failure (representative-fixture build / pool admit),
// which is the correct behavior for a throwaway harness.
#![allow(clippy::expect_used)]

use std::time::Duration;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use ava_avm::mempool::Mempool;
use ava_avm::txs::components::{AvaxBaseTx, Output, TransferableOutput};
use ava_avm::txs::{BaseTx, Tx, UnsignedTx, codec};
use ava_secp256k1fx::{OutputOwners, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

use ava_codec::manager::Manager;

/// Number of txs pushed then popped per iteration — a representative batch the
/// builder might pack from a warm pool.
const BATCH: u32 = 64;

/// Builds an initialized [`Tx`] whose `memo` carries `tag`, giving it a distinct
/// ID and a non-trivial serialized size, with no consumed inputs (so admissions
/// never conflict). Mirrors `mempool.rs`'s `tx_with_tag` test helper exactly so
/// the bench measures the same representative tx shape.
fn tx_with_tag(c: &Manager, tag: u32) -> Tx {
    let owners = OutputOwners::new(0, 1, vec![ShortId::from([0xab; 20])]);
    let base = BaseTx::new(AvaxBaseTx {
        network_id: 1,
        blockchain_id: Id::EMPTY,
        outs: vec![TransferableOutput {
            asset_id: Id::EMPTY,
            out: Output::SecpTransfer(TransferOutput::new(0, owners)),
        }],
        ins: vec![],
        memo: tag.to_be_bytes().to_vec(),
    });
    let mut tx = Tx::new(UnsignedTx::Base(base));
    tx.initialize(c).expect("initialize tx");
    tx
}

/// Push every tx (FIFO `add`), then drain front-to-back (`peek` + `remove`).
/// Times only the push/pop hot path; tx construction happens outside.
fn push_pop(txs: &[Tx]) {
    let mut m = Mempool::new();
    for tx in txs {
        m.add(tx.clone()).expect("add tx");
    }
    while let Some(id) = m.peek().map(Tx::id) {
        let _ = black_box(m.remove(&id));
    }
}

fn bench(c: &mut Criterion) {
    let manager = codec::codec().expect("codec");
    // Build the batch ONCE, outside the timed closure.
    let txs: Vec<Tx> = (0..BATCH).map(|tag| tx_with_tag(&manager, tag)).collect();

    c.bench_function("mempool_push_pop", |b| {
        b.iter(|| push_pop(black_box(&txs)));
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
