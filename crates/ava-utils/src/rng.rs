// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Deterministic RNG — gonum-exact MT19937 / MT19937-64 (the R1 gate).
//!
//! TODO(M0.3): port verbatim from `specs/03-core-primitives.md` §10.3 — the
//! `Source` trait (`uint64(&mut self) -> u64`); `Mt19937_64 { mt: [u64;312], mti }`
//! (NN=312, MM=156, MATRIX_A=0xB5026F5AA96619E9, the seed schedule, refill
//! twist, 4-line tempering); `Mt19937 { mt: [u32;624], mti }` (N=624, M=397,
//! MATRIX_A=0x9908b0df, 32-bit tempering, `uint64() = (high<<32)|low`). All math
//! `wrapping_*`. This is the ONLY home of the consensus RNG (hazard #4).
