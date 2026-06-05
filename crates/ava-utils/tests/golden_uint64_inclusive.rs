// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use ava_utils::rng::{Mt19937_64, Source};
use ava_utils::sampler::rng::uint64_inclusive;

#[derive(serde::Deserialize)]
struct Case {
    seed: u64,
    n: u64,
    outputs: Vec<u64>,
}

#[derive(serde::Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}

#[test]
fn uint64_inclusive_branches() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/sampler/uint64_inclusive.json"
    ))
    .unwrap();
    let v: Vectors = serde_json::from_str(&raw).unwrap();
    for c in &v.cases {
        let mut src: Box<dyn Source> = {
            let mut g = Mt19937_64::new();
            g.seed(c.seed);
            Box::new(g)
        };
        let got: Vec<u64> = (0..c.outputs.len())
            .map(|_| uint64_inclusive(src.as_mut(), c.n))
            .collect();
        assert_eq!(
            got, c.outputs,
            "uint64_inclusive diverged for seed {} n {}",
            c.seed, c.n
        );
    }
}
