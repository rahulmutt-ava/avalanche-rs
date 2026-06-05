// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Consensus-affecting bit-subset helpers over ids.
//!
//! Ported verbatim from Go `ids/bits.go`. These mask id bits for the
//! consensus polling/patricia-routing routines and must be bit-exact.
//!
//! Bit indexing convention (Go): index 7 is the MSB of byte 0 (LSB-first
//! within a byte):
//! `[7 6 5 4 3 2 1 0] [15 14 13 12 11 10 9 8] ... [255 254 253 252 251 250 249 248]`.
//!
//! Owning spec: `specs/03-core-primitives.md` §1.2.

use crate::id::Id;

/// The number of bits a patricia tree manages. Mirrors Go `ids.NumBits`.
pub const NUM_BITS: i32 = 256;

/// The number of bits per byte. Mirrors Go `ids.BitsPerByte`.
pub const BITS_PER_BYTE: i32 = 8;

/// Returns whether `id1` and `id2` are equal from bit `start` to bit `stop`
/// (non-inclusive). Mirrors Go `ids.EqualSubset`.
#[must_use]
pub fn equal_subset(start: i32, stop: i32, id1: &Id, id2: &Id) -> bool {
    let stop = stop - 1;
    if start > stop || stop < 0 {
        return true;
    }
    if stop >= NUM_BITS {
        return false;
    }

    let start_index = (start / BITS_PER_BYTE) as usize;
    let stop_index = (stop / BITS_PER_BYTE) as usize;

    let b1 = id1.as_bytes();
    let b2 = id2.as_bytes();

    // If there is a series of bytes between the first and last, they must be equal.
    if start_index + 1 < stop_index
        && b1[start_index + 1..stop_index] != b2[start_index + 1..stop_index]
    {
        return false;
    }

    let start_bit = (start % BITS_PER_BYTE) as u32; // index in the byte of the first bit
    let stop_bit = (stop % BITS_PER_BYTE) as u32; // index in the byte of the last bit

    let start_mask: i32 = -1 << start_bit; // 111...0... ; trailing zeros == start_bit
    let stop_mask: i32 = (1 << (stop_bit + 1)) - 1; // 000...1... ; ones == stop_bit + 1

    if start_index == stop_index {
        // Same byte: both masks apply.
        let mask = start_mask & stop_mask;
        let v1 = mask & i32::from(b1[start_index]);
        let v2 = mask & i32::from(b2[start_index]);
        return v1 == v2;
    }

    let start1 = start_mask & i32::from(b1[start_index]);
    let start2 = start_mask & i32::from(b2[start_index]);

    let stop1 = stop_mask & i32::from(b1[stop_index]);
    let stop2 = stop_mask & i32::from(b2[stop_index]);

    start1 == start2 && stop1 == stop2
}

/// Returns the index of the first differing bit between `id1` and `id2` inside
/// `[start, stop)`, or `None` if none differ. Mirrors Go
/// `ids.FirstDifferenceSubset`.
#[must_use]
pub fn first_difference_subset(start: i32, stop: i32, id1: &Id, id2: &Id) -> Option<usize> {
    let stop = stop - 1;
    // Kept as three separate comparisons to mirror Go `FirstDifferenceSubset`
    // verbatim (consensus-affecting); do not collapse into a range check.
    #[allow(clippy::manual_range_contains)]
    if start > stop || stop < 0 || stop >= NUM_BITS {
        return None;
    }

    let start_index = (start / BITS_PER_BYTE) as usize;
    let stop_index = (stop / BITS_PER_BYTE) as usize;

    let start_bit = (start % BITS_PER_BYTE) as u32;
    let stop_bit = (stop % BITS_PER_BYTE) as u32;

    let start_mask: i32 = -1 << start_bit;
    let stop_mask: i32 = (1 << (stop_bit + 1)) - 1;

    let b1 = id1.as_bytes();
    let b2 = id2.as_bytes();

    if start_index == stop_index {
        let mask = start_mask & stop_mask;
        let v1 = mask & i32::from(b1[start_index]);
        let v2 = mask & i32::from(b2[start_index]);
        if v1 == v2 {
            return None;
        }
        let bd = (v1 ^ v2) as u8;
        return Some(bd.trailing_zeros() as usize + start_index * BITS_PER_BYTE as usize);
    }

    // First byte, possibly masked.
    let start1 = start_mask & i32::from(b1[start_index]);
    let start2 = start_mask & i32::from(b2[start_index]);
    if start1 != start2 {
        let bd = (start1 ^ start2) as u8;
        return Some(bd.trailing_zeros() as usize + start_index * BITS_PER_BYTE as usize);
    }

    // Interior bytes.
    for i in (start_index + 1)..stop_index {
        let v1 = i32::from(b1[i]);
        let v2 = i32::from(b2[i]);
        if v1 != v2 {
            let bd = (v1 ^ v2) as u8;
            return Some(bd.trailing_zeros() as usize + i * BITS_PER_BYTE as usize);
        }
    }

    // Last byte, possibly masked.
    let stop1 = stop_mask & i32::from(b1[stop_index]);
    let stop2 = stop_mask & i32::from(b2[stop_index]);
    if stop1 != stop2 {
        let bd = (stop1 ^ stop2) as u8;
        return Some(bd.trailing_zeros() as usize + stop_index * BITS_PER_BYTE as usize);
    }

    None
}
