// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use ava_utils::rng::{Mt19937_64, Source};
use ava_utils::sampler::uniform::Uniform;
use ava_utils::sampler::weighted::Weighted;
use ava_utils::sampler::weighted_without_replacement::WeightedWithoutReplacement;
use ava_utils::sampler::{
    new_deterministic_uniform, new_deterministic_weighted,
    new_deterministic_weighted_without_replacement,
};

#[derive(serde::Deserialize)]
struct Case {
    kind: String,
    seed: u64,
    #[serde(default)]
    length: u64,
    #[serde(default)]
    count: u64,
    #[serde(default)]
    weights: Vec<u64>,
    #[serde(default)]
    sample_values: Vec<u64>,
    sampled_indices: Vec<u64>,
}

#[derive(serde::Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}

fn src(seed: u64) -> Box<dyn Source> {
    let mut g = Mt19937_64::new();
    g.seed(seed);
    Box::new(g)
}

#[test]
fn deterministic_samplers() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/sampler/samplers.json"
    ))
    .unwrap();
    let v: Vectors = serde_json::from_str(&raw).unwrap();
    for c in &v.cases {
        match c.kind.as_str() {
            "uniform" => {
                let mut u = new_deterministic_uniform(src(c.seed));
                u.initialize(c.length);
                let got = u.sample(c.count as usize).expect("uniform sample");
                assert_eq!(got, c.sampled_indices, "uniform seed {}", c.seed);
            }
            "weighted" => {
                let mut w = new_deterministic_weighted(src(c.seed));
                w.initialize(&c.weights).expect("weighted init");
                let got: Vec<u64> = c
                    .sample_values
                    .iter()
                    .map(|&val| w.sample(val).expect("weighted sample") as u64)
                    .collect();
                assert_eq!(got, c.sampled_indices, "weighted seed {}", c.seed);
            }
            "wwr" => {
                let mut w = new_deterministic_weighted_without_replacement(src(c.seed));
                w.initialize(&c.weights).expect("wwr init");
                let got: Vec<u64> = w
                    .sample(c.count as usize)
                    .expect("wwr sample")
                    .into_iter()
                    .map(|i| i as u64)
                    .collect();
                assert_eq!(got, c.sampled_indices, "wwr seed {}", c.seed);
            }
            other => panic!("unknown sampler kind {other}"),
        }
    }
}
