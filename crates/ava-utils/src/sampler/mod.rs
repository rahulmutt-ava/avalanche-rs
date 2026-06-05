// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Deterministic samplers over a [`crate::rng::Source`].
//!
//! Single-threaded by contract — no parallelism (`specs/03-core-primitives.md`
//! §9). Submodules:
//!
//! - [`rng`] — `Uint64Inclusive` rejection wrapper (exact draw count, M0.4)
//! - [`uniform`] — lazy partial Fisher–Yates uniform sampler (M0.10)
//! - [`weighted`] — weighted-heap sampler (M0.10)
//! - [`weighted_without_replacement`] — wwr sampler (M0.10)

pub mod rng;
pub mod uniform;
pub mod weighted;
pub mod weighted_without_replacement;

use crate::rng::Source;
use crate::sampler::uniform::UniformReplacer;
use crate::sampler::weighted::WeightedHeap;
use crate::sampler::weighted_without_replacement::WeightedWithoutReplacementGeneric;

/// Builds a deterministic uniform sampler over `src` (Go
/// `NewDeterministicUniform`).
#[must_use]
pub fn new_deterministic_uniform(src: Box<dyn Source>) -> UniformReplacer {
    UniformReplacer::new(src)
}

/// Builds a deterministic weighted sampler (Go `NewDeterministicWeighted`).
///
/// The heap-based weighted sampler is itself deterministic given the sampled
/// value, so `src` is unused here; it is accepted for API symmetry with Go and
/// to keep call sites uniform.
#[must_use]
pub fn new_deterministic_weighted(_src: Box<dyn Source>) -> WeightedHeap {
    WeightedHeap::new()
}

/// Builds the deterministic weighted-without-replacement sampler used by the
/// validator set / consensus polls (Go
/// `NewDeterministicWeightedWithoutReplacement`).
#[must_use]
pub fn new_deterministic_weighted_without_replacement(
    src: Box<dyn Source>,
) -> WeightedWithoutReplacementGeneric {
    WeightedWithoutReplacementGeneric::new(src)
}
