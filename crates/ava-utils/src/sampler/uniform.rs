// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Uniform sampler — lazy partial Fisher–Yates (`uniform_replacer.go`).
//!
//! Ported verbatim from `specs/03-core-primitives.md` §4.1. Samples distinct
//! indices from `[0, length)` without replacement using the `drawn` default-map
//! (`get(k, default=k)`) and the exact draw formula, so the RNG draw sequence
//! matches Go bit-for-bit. Single-threaded by contract (no parallelism, §9).
//! Owning spec: `specs/03-core-primitives.md` §4.1.

use std::collections::HashMap;

use crate::rng::Source;
use crate::sampler::rng::uint64_inclusive;

/// Uniform-without-replacement sampler over `[0, length)` (Go
/// `sampler.Uniform`).
pub trait Uniform {
    /// Sets the range size to `n` and resets the draw state (Go `Initialize`).
    fn initialize(&mut self, n: u64);
    /// Draws `k` distinct indices, or `None` if `k > length` (Go `Sample`).
    fn sample(&mut self, k: usize) -> Option<Vec<u64>>;
    /// Draws the next single index, or `None` if the range is exhausted.
    fn next(&mut self) -> Option<u64>;
    /// Resets the draw state without changing `length` (Go `Reset`).
    fn reset(&mut self);
}

/// Deterministic uniform sampler backed by a [`Source`] (Go `uniformReplacer`).
pub struct UniformReplacer {
    rng: Box<dyn Source>,
    length: u64,
    drawn: HashMap<u64, u64>,
    draws_count: u64,
}

impl UniformReplacer {
    /// Wraps `src` as a deterministic uniform sampler.
    #[must_use]
    pub fn new(src: Box<dyn Source>) -> Self {
        Self {
            rng: src,
            length: 0,
            drawn: HashMap::new(),
            draws_count: 0,
        }
    }
}

impl Uniform for UniformReplacer {
    fn initialize(&mut self, n: u64) {
        self.length = n;
        self.reset();
    }

    fn reset(&mut self) {
        self.drawn.clear();
        self.draws_count = 0;
    }

    fn next(&mut self) -> Option<u64> {
        if self.draws_count >= self.length {
            return None;
        }
        // draw = uint64_inclusive(rng, length-1-draws_count) + draws_count
        let draw = uint64_inclusive(self.rng.as_mut(), self.length - 1 - self.draws_count)
            + self.draws_count;
        let ret = self.drawn.get(&draw).copied().unwrap_or(draw);
        // drawn[draw] = drawn.get(draws_count, default=draws_count)
        let replacement = self
            .drawn
            .get(&self.draws_count)
            .copied()
            .unwrap_or(self.draws_count);
        self.drawn.insert(draw, replacement);
        self.draws_count += 1;
        Some(ret)
    }

    fn sample(&mut self, k: usize) -> Option<Vec<u64>> {
        if (k as u64) > self.length {
            return None;
        }
        self.reset();
        let mut out = Vec::with_capacity(k);
        for _ in 0..k {
            out.push(self.next()?);
        }
        Some(out)
    }
}
