// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Deterministic RNG — gonum-exact MT19937 / MT19937-64 (the R1 gate).
//!
//! Ported verbatim from `specs/03-core-primitives.md` §10.3. This is the ONLY
//! home of the consensus RNG (determinism hazard #4). The validator sampler
//! output is stored in chain state, so this stream must match gonum's
//! `prng.MT19937_64` / `prng.MT19937` bit-for-bit. All arithmetic uses
//! `wrapping_*` to reproduce Go integer-overflow semantics.

/// Mirrors `sampler.Source` (`utils/sampler/rand.go`). Only `uint64` is on the
/// consensus path. Implemented by both MT variants below.
pub trait Source {
    /// Random value in `[0, u64::MAX]`; advances generator state. == Go `Uint64()`.
    fn uint64(&mut self) -> u64;
}

/// gonum `prng.MT19937_64` — hand-port, byte-for-byte. Used by all deterministic
/// samplers and the Post-Durango windower.
pub struct Mt19937_64 {
    mt: [u64; 312],
    mti: usize, // sentinel 313 == "Seed never called"
}

impl Mt19937_64 {
    const NN: usize = 312;
    const MM: usize = 156;
    const MATRIX_A: u64 = 0xB502_6F5A_A966_19E9;
    const UPPER: u64 = 0xFFFF_FFFF_8000_0000;
    const LOWER: u64 = 0x0000_0000_7FFF_FFFF;

    /// == `NewMT19937_64()`: unseeded sentinel (lazy default-seed 5489 on first draw).
    #[must_use]
    pub fn new() -> Self {
        Self {
            mt: [0; 312],
            mti: Self::NN + 1,
        }
    }

    /// == `(*MT19937_64).Seed`. Wrapping arithmetic is REQUIRED (Go integer overflow).
    pub fn seed(&mut self, seed: u64) {
        self.mt[0] = seed;
        let mut i = 1;
        while i < Self::NN {
            let prev = self.mt[i - 1];
            self.mt[i] = 6_364_136_223_846_793_005u64
                .wrapping_mul(prev ^ (prev >> 62))
                .wrapping_add(i as u64);
            i += 1;
        }
        self.mti = Self::NN;
    }

    fn refill(&mut self) {
        const MAG01: [u64; 2] = [0, Mt19937_64::MATRIX_A];
        if self.mti == Self::NN + 1 {
            self.seed(5489); // lazy default seed
        }
        let mut x;
        let mut i = 0;
        while i < Self::NN - Self::MM {
            x = (self.mt[i] & Self::UPPER) | (self.mt[i + 1] & Self::LOWER);
            self.mt[i] = self.mt[i + Self::MM] ^ (x >> 1) ^ MAG01[(x & 1) as usize];
            i += 1;
        }
        while i < Self::NN - 1 {
            x = (self.mt[i] & Self::UPPER) | (self.mt[i + 1] & Self::LOWER);
            self.mt[i] = self.mt[i + Self::MM - Self::NN] ^ (x >> 1) ^ MAG01[(x & 1) as usize];
            i += 1;
        }
        x = (self.mt[Self::NN - 1] & Self::UPPER) | (self.mt[0] & Self::LOWER);
        self.mt[Self::NN - 1] = self.mt[Self::MM - 1] ^ (x >> 1) ^ MAG01[(x & 1) as usize];
        self.mti = 0;
    }
}

impl Default for Mt19937_64 {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for Mt19937_64 {
    fn uint64(&mut self) -> u64 {
        if self.mti >= Self::NN {
            self.refill();
        }
        let mut x = self.mt[self.mti];
        self.mti += 1;
        // Tempering — exact gonum constants.
        x ^= (x >> 29) & 0x5555_5555_5555_5555;
        x ^= (x << 17) & 0x71D6_7FFF_EDA6_0000;
        x ^= (x << 37) & 0xFFF7_EEE0_0000_0000;
        x ^= x >> 43;
        x
    }
}

/// gonum `prng.MT19937` (32-bit). Used ONLY by the Pre-Durango `Proposers` path.
/// Its `Source::uint64` draws TWO u32s (high first); seed truncates to low 32 bits.
pub struct Mt19937 {
    mt: [u32; 624],
    mti: usize, // sentinel 625
}

impl Mt19937 {
    const N: usize = 624;
    const M: usize = 397;
    const MATRIX_A: u32 = 0x9908_b0df;
    const UPPER: u32 = 0x8000_0000;
    const LOWER: u32 = 0x7fff_ffff;

    /// == `NewMT19937()`: unseeded sentinel (lazy default-seed 5489 on first draw).
    #[must_use]
    pub fn new() -> Self {
        Self {
            mt: [0; 624],
            mti: Self::N + 1,
        }
    }

    /// == `(*MT19937).Seed` — NOTE the `as u32` truncation of the u64 seed.
    pub fn seed(&mut self, seed: u64) {
        self.mt[0] = seed as u32;
        let mut i = 1;
        while i < Self::N {
            let prev = self.mt[i - 1];
            self.mt[i] = 1_812_433_253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
            i += 1;
        }
        self.mti = Self::N;
    }

    fn uint32(&mut self) -> u32 {
        const MAG01: [u32; 2] = [0, Mt19937::MATRIX_A];
        if self.mti >= Self::N {
            if self.mti == Self::N + 1 {
                self.seed(5489);
            }
            let mut y;
            let mut kk = 0;
            while kk < Self::N - Self::M {
                y = (self.mt[kk] & Self::UPPER) | (self.mt[kk + 1] & Self::LOWER);
                self.mt[kk] = self.mt[kk + Self::M] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
                kk += 1;
            }
            while kk < Self::N - 1 {
                y = (self.mt[kk] & Self::UPPER) | (self.mt[kk + 1] & Self::LOWER);
                self.mt[kk] = self.mt[kk + Self::M - Self::N] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
                kk += 1;
            }
            y = (self.mt[Self::N - 1] & Self::UPPER) | (self.mt[0] & Self::LOWER);
            self.mt[Self::N - 1] = self.mt[Self::M - 1] ^ (y >> 1) ^ MAG01[(y & 1) as usize];
            self.mti = 0;
        }
        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }
}

impl Default for Mt19937 {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for Mt19937 {
    /// == `(*MT19937).Uint64`: high word drawn first, low word second.
    fn uint64(&mut self) -> u64 {
        let h = u64::from(self.uint32());
        let l = u64::from(self.uint32());
        (h << 32) | l
    }
}
