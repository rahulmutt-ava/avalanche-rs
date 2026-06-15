// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Critical-path codec bench (specs/02 §9): a `Packer` encode → decode
//! round-trip over a representative mix of primitives + a length-prefixed byte
//! blob, the shape the linear codec emits for wire payloads.
//!
//! Configured for SHORT runs so `cargo xtask bench-guard` finishes well under a
//! minute; this is a perf-gate canary, not a precise micro-benchmark.

use std::time::Duration;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use ava_codec::packer::Packer;

/// Encode a representative primitive mix + a 256-byte blob, then decode it back.
fn roundtrip(payload: &[u8]) {
    // Write side.
    let mut w = Packer::new_write(512);
    w.pack_u64(0x0123_4567_89ab_cdef);
    w.pack_u32(0xdead_beef);
    w.pack_u16(0xc0fe);
    w.pack_byte(0x42);
    w.pack_bool(true);
    w.pack_bytes(payload);
    let bytes = w.into_bytes();

    // Read side.
    let mut r = Packer::new_read(&bytes);
    let _ = black_box(r.unpack_u64());
    let _ = black_box(r.unpack_u32());
    let _ = black_box(r.unpack_u16());
    let _ = black_box(r.unpack_byte());
    let _ = black_box(r.unpack_bool());
    let _ = black_box(r.unpack_bytes());
}

fn bench(c: &mut Criterion) {
    let payload = vec![0xa5u8; 256];
    c.bench_function("codec_roundtrip", |b| {
        b.iter(|| roundtrip(black_box(&payload)));
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
