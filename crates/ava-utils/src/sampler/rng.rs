// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Uint64Inclusive` — rejection-sampling wrapper with exact draw-count parity.
//!
//! TODO(M0.4): port `uint64_inclusive(src: &mut impl Source, n: u64) -> u64`
//! verbatim from `specs/03-core-primitives.md` §4.1 — branch 1 (power-of-two
//! mask), branch 2 (`n > i64::MAX`), branch 3 (uint63 mask + reject loop +
//! `% (n+1)`). The reject loop MUST consume the same RNG draws as Go.
