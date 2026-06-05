// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Weighted-without-replacement sampler (generic over uniform + weighted).
//!
//! Ported verbatim from `specs/03-core-primitives.md` §4.1 and Go
//! `utils/sampler/weighted_without_replacement_generic.go`. `initialize` sums
//! weights with **checked add**, initializes the uniform over the total weight
//! space and the weighted over the per-index weights. `sample(count)` resets the
//! uniform, then for each draw maps `uniform.next()` through `weighted.sample`.
//! Duplicates are impossible because the uniform draws without replacement over
//! the weight space — this is the validator-set sampler.
//! Owning spec: `specs/03-core-primitives.md` §4.1.

use crate::error::Result;
use crate::math;
use crate::rng::Source;
use crate::sampler::uniform::{Uniform, UniformReplacer};
use crate::sampler::weighted::{Weighted, WeightedHeap};

/// A weighted sampler that draws `count` distinct indices (Go
/// `sampler.WeightedWithoutReplacement`).
pub trait WeightedWithoutReplacement {
    /// Initializes with per-index weights (Go `Initialize`).
    ///
    /// # Errors
    /// Returns [`crate::error::Error::Overflow`] if the weights sum overflows `u64`.
    fn initialize(&mut self, weights: &[u64]) -> Result<()>;
    /// Draws `count` distinct indices, or `None` if the request cannot be
    /// satisfied (Go `Sample`).
    fn sample(&mut self, count: usize) -> Option<Vec<usize>>;
}

/// Deterministic weighted-without-replacement sampler composing a
/// [`UniformReplacer`] (which owns the RNG) over the weight space with a
/// [`WeightedHeap`] mapping a weight position to an index.
pub struct WeightedWithoutReplacementGeneric {
    uniform: UniformReplacer,
    weighted: WeightedHeap,
}

impl WeightedWithoutReplacementGeneric {
    /// Wraps `src` as a deterministic weighted-without-replacement sampler.
    #[must_use]
    pub fn new(src: Box<dyn Source>) -> Self {
        Self {
            uniform: UniformReplacer::new(src),
            weighted: WeightedHeap::new(),
        }
    }
}

impl WeightedWithoutReplacement for WeightedWithoutReplacementGeneric {
    fn initialize(&mut self, weights: &[u64]) -> Result<()> {
        let mut total: u64 = 0;
        for &w in weights {
            total = math::add(total, w)?;
        }
        self.uniform.initialize(total);
        self.weighted.initialize(weights)?;
        Ok(())
    }

    fn sample(&mut self, count: usize) -> Option<Vec<usize>> {
        self.uniform.reset();
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let w = self.uniform.next()?;
            let index = self.weighted.sample(w)?;
            out.push(index);
        }
        Some(out)
    }
}
