// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Critical-path signature bench (specs/02 §9): secp256k1 recoverable-signature
//! verify over a 32-byte digest — the per-input verify the tx-verification path
//! runs for every credential.
//!
//! Configured for SHORT runs so `cargo xtask bench-guard` finishes well under a
//! minute; this is a perf-gate canary, not a precise micro-benchmark.

use std::time::Duration;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use ava_crypto::secp256k1::PrivateKey;

fn bench(c: &mut Criterion) {
    // A fixed non-zero scalar — deterministic so the bench is reproducible.
    let sk = PrivateKey::from_bytes(&[0x11u8; 32]).expect("valid fixed secp256k1 scalar");
    let pk = sk.public_key();
    let hash = [0x42u8; 32];
    let sig = sk.sign_hash(&hash).expect("sign fixed digest");

    c.bench_function("secp256k1_verify", |b| {
        b.iter(|| {
            black_box(pk.verify_hash(black_box(&hash), black_box(&sig)));
        });
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
