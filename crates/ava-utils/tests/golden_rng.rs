// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use ava_utils::rng::{Mt19937, Mt19937_64, Source};

#[derive(serde::Deserialize)]
struct Vec64 {
    seed: u64,
    stream: Vec<u64>,
}

#[test]
fn sampler_mt19937_stream() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/rng/mt19937_64.json"
    ))
    .unwrap();
    let cases: Vec<Vec64> = serde_json::from_str(&raw).unwrap();
    // R1 gate: assert seed 0 first, then every committed seed.
    assert!(cases.iter().any(|c| c.seed == 0), "seed-0 vector required");
    for c in &cases {
        let mut g = Mt19937_64::new();
        g.seed(c.seed);
        let got: Vec<u64> = (0..c.stream.len()).map(|_| g.uint64()).collect();
        assert_eq!(
            got, c.stream,
            "MT19937-64 stream diverged for seed {}",
            c.seed
        );
    }
    // 32-bit variant: high-word-first Uint64 composition.
    let raw32 = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/rng/mt19937_32.json"
    ))
    .unwrap();
    let cases32: Vec<Vec64> = serde_json::from_str(&raw32).unwrap();
    for c in &cases32 {
        let mut g = Mt19937::new();
        g.seed(c.seed);
        let got: Vec<u64> = (0..c.stream.len()).map(|_| g.uint64()).collect();
        assert_eq!(
            got, c.stream,
            "MT19937(32) Uint64 stream diverged for seed {}",
            c.seed
        );
    }
}
