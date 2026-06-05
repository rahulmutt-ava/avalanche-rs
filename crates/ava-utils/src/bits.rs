// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Big-integer-backed `Bits` set + `Bits64` fast path.
//!
//! Mirrors Go `utils/set/bits.go` (`Bits`) and `utils/set/bits_64.go`
//! (`Bits64`). `Bits` stores membership in a [`BigUint`]; `Bytes`/`from_bytes`
//! use the big-endian representation. Used by consensus polls.
//! Owning spec: `specs/03-core-primitives.md` §4.2.

use num_bigint::BigUint;

/// A big-integer-backed bitset. Bit `i` set ⇔ `i` is a member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bits {
    inner: BigUint,
}

impl Bits {
    /// An empty bit set (Go `NewBits()` with no args).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: BigUint::ZERO,
        }
    }

    /// Adds index `i` to the set (Go `Bits.Add`).
    pub fn add(&mut self, i: u64) {
        self.inner.set_bit(i, true);
    }

    /// Removes index `i` from the set (Go `Bits.Remove`).
    pub fn remove(&mut self, i: u64) {
        self.inner.set_bit(i, false);
    }

    /// Reports whether index `i` is in the set (Go `Bits.Contains`).
    #[must_use]
    pub fn contains(&self, i: u64) -> bool {
        self.inner.bit(i)
    }

    /// The number of set bits (Go `Bits.Len` == popcount).
    #[must_use]
    pub fn len(&self) -> u64 {
        self.inner.count_ones()
    }

    /// Reports whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner == BigUint::ZERO
    }

    /// The index of the highest set bit plus one (Go `Bits.BitLen`).
    #[must_use]
    pub fn bit_len(&self) -> u64 {
        self.inner.bits()
    }

    /// Union of two bit sets (Go `Bits.Union`).
    #[must_use]
    pub fn union(a: &Self, b: &Self) -> Self {
        Self {
            inner: &a.inner | &b.inner,
        }
    }

    /// Intersection of two bit sets (Go `Bits.Intersection`).
    #[must_use]
    pub fn intersection(a: &Self, b: &Self) -> Self {
        Self {
            inner: &a.inner & &b.inner,
        }
    }

    /// Set difference `a \ b` (Go `Bits.Difference`).
    #[must_use]
    pub fn difference(a: &Self, b: &Self) -> Self {
        // a & ~b, but BigUint has no bitwise-not; clear each bit of b from a.
        let mut out = a.inner.clone();
        let mut i = 0u64;
        let nbits = b.inner.bits();
        while i < nbits {
            if b.inner.bit(i) {
                out.set_bit(i, false);
            }
            i += 1;
        }
        Self { inner: out }
    }

    /// Big-endian byte representation (Go `Bits.Bytes`); empty for an empty set.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        if self.inner == BigUint::ZERO {
            Vec::new()
        } else {
            self.inner.to_bytes_be()
        }
    }

    /// Constructs a `Bits` from a big-endian byte slice (Go `BitsFromBytes`).
    #[must_use]
    pub fn from_bytes(b: &[u8]) -> Self {
        Self {
            inner: BigUint::from_bytes_be(b),
        }
    }
}

impl std::fmt::Display for Bits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Go `Bits.String` formats as hex.
        write!(f, "{:x}", self.inner)
    }
}

/// A `u64` fast-path bit set (Go `Bits64`). Indices must be in `[0, 64)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bits64(u64);

impl Bits64 {
    /// An empty 64-bit set.
    #[must_use]
    pub fn new() -> Self {
        Self(0)
    }

    /// Adds index `i` (`i < 64`).
    pub fn add(&mut self, i: u8) {
        self.0 |= 1u64 << i;
    }

    /// Removes index `i` (`i < 64`).
    pub fn remove(&mut self, i: u8) {
        self.0 &= !(1u64 << i);
    }

    /// Reports whether index `i` is set.
    #[must_use]
    pub fn contains(&self, i: u8) -> bool {
        self.0 & (1u64 << i) != 0
    }

    /// The number of set bits (popcount).
    #[must_use]
    pub fn len(&self) -> u32 {
        self.0.count_ones()
    }

    /// Reports whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Union of two 64-bit sets.
    #[must_use]
    pub fn union(a: Self, b: Self) -> Self {
        Self(a.0 | b.0)
    }

    /// Intersection of two 64-bit sets.
    #[must_use]
    pub fn intersection(a: Self, b: Self) -> Self {
        Self(a.0 & b.0)
    }

    /// Set difference `a \ b`.
    #[must_use]
    pub fn difference(a: Self, b: Self) -> Self {
        Self(a.0 & !b.0)
    }
}
