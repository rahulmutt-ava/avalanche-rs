// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Uint64Inclusive` — rejection-sampling wrapper with exact draw-count parity.
//!
//! Ported verbatim from `specs/03-core-primitives.md` §4.1 (`rand.go:39`). The
//! reject loops MUST consume the same number of RNG draws as Go so downstream
//! sampler state stays in lock-step — drift in draw count, not just bit layout,
//! desyncs all consensus-critical sampling.

use crate::rng::Source;

/// Returns a uniformly-distributed value in `[0, n]` (inclusive), drawing from
/// `src`. Mirrors the three branches of Go's `rng.Uint64Inclusive`.
///
/// `src` is `?Sized` so a `&mut dyn Source` / `Box<dyn Source>` composes.
pub fn uint64_inclusive<S: Source + ?Sized>(src: &mut S, n: u64) -> u64 {
    if n & n.wrapping_add(1) == 0 {
        // n+1 is a power of two
        src.uint64() & n
    } else if n > i64::MAX as u64 {
        // n > MaxInt64
        let mut v = src.uint64();
        while v > n {
            v = src.uint64();
        }
        v
    } else {
        // max = (1<<63) - 1 - (1<<63) % (n+1); rejection-sample uint63.
        let max = ((1u64 << 63) - 1) - ((1u64 << 63) % (n + 1));
        let mut v = src.uint64() & i64::MAX as u64;
        while v > max {
            v = src.uint64() & i64::MAX as u64;
        }
        v % (n + 1)
    }
}
