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
