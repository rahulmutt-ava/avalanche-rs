// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Key` / `Path` — bit-paths over a configurable [`BranchFactor`].
//!
//! Byte-exact port of Go `x/merkledb/key.go`. A [`Key`] holds packed *tokens*
//! (a token is `token_size` bits) in [`Key::value`]; [`Key::length`] is the
//! number of *bits*. Go uses an `unsafe` `string`↔`[]byte` aliasing trick for
//! zero-copy; we use [`bytes::Bytes`] / `&[u8]` instead (spec §3.2).

use bytes::Bytes;

/// The branch factor of the trie: the number of children each node can have.
///
/// Token sizes are 1/2/4/8 bits respectively. [`BranchFactor::TwoFiftySix`]
/// (byte-per-token) is the production default. Mirrors Go `BranchFactor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BranchFactor {
    /// Branch factor 2 — 1-bit tokens.
    Two,
    /// Branch factor 4 — 2-bit tokens.
    Four,
    /// Branch factor 16 — 4-bit tokens.
    Sixteen,
    /// Branch factor 256 — 8-bit (byte) tokens. Production default.
    TwoFiftySix,
}

impl BranchFactor {
    /// The largest supported branch factor. Mirrors Go `BranchFactorLargest`.
    pub const LARGEST: BranchFactor = BranchFactor::TwoFiftySix;

    /// Token size in bits (1, 2, 4, or 8). Mirrors `BranchFactorToTokenSize`.
    #[must_use]
    pub fn token_size(self) -> usize {
        match self {
            BranchFactor::Two => 1,
            BranchFactor::Four => 2,
            BranchFactor::Sixteen => 4,
            BranchFactor::TwoFiftySix => 8,
        }
    }

    /// The numeric branch factor (2/4/16/256).
    #[must_use]
    pub fn value(self) -> u16 {
        match self {
            BranchFactor::Two => 2,
            BranchFactor::Four => 4,
            BranchFactor::Sixteen => 16,
            BranchFactor::TwoFiftySix => 256,
        }
    }

    /// Constructs a [`BranchFactor`] from a token size in bits.
    #[must_use]
    pub fn from_token_size(token_size: usize) -> Option<BranchFactor> {
        match token_size {
            1 => Some(BranchFactor::Two),
            2 => Some(BranchFactor::Four),
            4 => Some(BranchFactor::Sixteen),
            8 => Some(BranchFactor::TwoFiftySix),
            _ => None,
        }
    }
}

/// A bit-path. [`value`](Key::value) holds packed tokens; [`length`](Key::length)
/// is in *bits*.
///
/// The derived [`Ord`] compares `value` lexicographically then `length`, exactly
/// matching Go `Key.Compare` (which compares the byte string then the bit length).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    // NOTE: field order matters for the derived Ord — `value` must come before
    // `length` to mirror Go `Key.Compare`.
    value: Bytes,
    length: usize,
}

/// `dualBitIndex` gets the dual of the bit index within a byte.
///
/// e.g. in a byte, the bit 5 from the right is the same as the bit 3 from the
/// left. Mirrors Go `dualBitIndex`.
#[must_use]
fn dual_bit_index(shift: usize) -> usize {
    (8 - shift) % 8
}

/// Returns the number of bytes needed to store `bits` bits.
/// Mirrors Go `bytesNeeded`.
#[must_use]
pub fn bytes_needed(bits: usize) -> usize {
    let size = bits / 8;
    if bits.is_multiple_of(8) {
        size
    } else {
        size + 1
    }
}

impl Key {
    /// An empty key (0 bits). Mirrors Go `Key{}`.
    #[must_use]
    pub fn empty() -> Key {
        Key {
            value: Bytes::new(),
            length: 0,
        }
    }

    /// Returns `key_bytes` as a new key, treating *all* bits as part of the key.
    /// Mirrors Go `ToKey`. Use [`Key::take`] if some trailing bits are unused.
    #[must_use]
    pub fn from_bytes(key_bytes: &[u8]) -> Key {
        Key {
            value: Bytes::copy_from_slice(key_bytes),
            length: key_bytes.len().saturating_mul(8),
        }
    }

    /// Constructs a key directly from already-packed `value` bytes and a bit
    /// `length`. The caller must ensure `value.len() == bytes_needed(length)`
    /// and that any partial-byte padding is zero.
    #[must_use]
    pub fn from_raw(value: Bytes, length: usize) -> Key {
        Key { value, length }
    }

    /// Creates a single-token key from `val` with bit length `token_size`.
    /// Mirrors Go `ToToken`. The token is stored left-aligned in its byte.
    #[must_use]
    pub fn to_token(val: u8, token_size: usize) -> Key {
        let stored = val << dual_bit_index(token_size);
        Key {
            value: Bytes::copy_from_slice(&[stored]),
            length: token_size,
        }
    }

    /// The number of bits in the key. Mirrors Go `Key.Length`.
    #[must_use]
    pub fn length(&self) -> usize {
        self.length
    }

    /// The raw packed bytes of the key. Mirrors Go `Key.Bytes`.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.value
    }

    /// Returns the token at the specified `bit_index`.
    ///
    /// Assumes `bit_index + token_size` doesn't cross a byte boundary.
    /// Mirrors Go `Key.Token`.
    #[must_use]
    pub fn token(&self, bit_index: usize, token_size: usize) -> u8 {
        let storage_byte = self.value[bit_index / 8];
        // Shift right so the last bit of the token is rightmost.
        let shifted = storage_byte >> dual_bit_index((bit_index + token_size) % 8);
        // Mask off any other bits.
        shifted & (0xFF >> dual_bit_index(token_size))
    }

    /// `true` iff the key occupies a non-whole number of bytes.
    /// Mirrors Go `Key.hasPartialByte`.
    #[must_use]
    pub fn has_partial_byte(&self) -> bool {
        !self.length.is_multiple_of(8)
    }

    /// Returns `true` iff `prefix` is a prefix of (or equal to) this key.
    /// Mirrors Go `Key.HasPrefix`.
    #[must_use]
    pub fn has_prefix(&self, prefix: &Key) -> bool {
        // [prefix] must be no longer than [self] to be a prefix.
        if self.length < prefix.length {
            return false;
        }

        // The number of token-bits in the last byte of [prefix], or zero if
        // [prefix] fits into a whole number of bytes.
        let remainder_bit_count = prefix.length % 8;
        if remainder_bit_count == 0 {
            return self.value.starts_with(&prefix.value);
        }

        // Compare the partially-filled final byte under a mask of the padding
        // bits, then compare the whole-byte prefix preceding it.
        let remainder_bits_mask: u8 = 0xFF >> remainder_bit_count;
        let last = prefix.value.len() - 1;
        let prefix_remainder_tokens = prefix.value[last] | remainder_bits_mask;
        let remainder_tokens = self.value[last] | remainder_bits_mask;

        if prefix_remainder_tokens != remainder_tokens {
            return false;
        }

        self.value.starts_with(&prefix.value[..last])
    }

    /// Returns `true` iff `prefix` is a strict (non-equal) prefix of this key.
    /// Mirrors Go `Key.HasStrictPrefix`.
    #[must_use]
    pub fn has_strict_prefix(&self, prefix: &Key) -> bool {
        self != prefix && self.has_prefix(prefix)
    }

    /// Checks if `prefix` is a prefix of this key *starting after the
    /// `bits_offset`-th bit*. Avoids the allocation of `skip().has_prefix()`.
    /// Mirrors Go `Key.iteratedHasPrefix`.
    #[must_use]
    pub fn iterated_has_prefix(&self, prefix: &Key, bits_offset: usize, token_size: usize) -> bool {
        if self.length < bits_offset || self.length - bits_offset < prefix.length {
            return false;
        }
        let mut i = 0;
        while i < prefix.length {
            if self.token(bits_offset + i, token_size) != prefix.token(i, token_size) {
                return false;
            }
            i += token_size;
        }
        true
    }

    /// Returns a new key containing the last `length - bits_to_skip` bits.
    /// Mirrors Go `Key.Skip`.
    #[must_use]
    pub fn skip(&self, bits_to_skip: usize) -> Key {
        if self.length <= bits_to_skip {
            return Key::empty();
        }
        let new_length = self.length - bits_to_skip;
        let tail = self.value.slice(bits_to_skip / 8..);

        // A whole-byte skip: the remaining bytes are exactly the new key.
        if bits_to_skip.is_multiple_of(8) {
            return Key {
                value: tail,
                length: new_length,
            };
        }

        // Partial-byte skip: copy the remaining bytes shifted into a new buffer.
        let mut buffer = vec![0u8; bytes_needed(new_length)];
        let bits_removed = bits_to_skip % 8;
        shift_copy(&mut buffer, &tail, bits_removed);
        Key {
            value: Bytes::from(buffer),
            length: new_length,
        }
    }

    /// Returns a new key containing the first `bits_to_take` bits.
    /// Mirrors Go `Key.Take`.
    #[must_use]
    pub fn take(&self, bits_to_take: usize) -> Key {
        if self.length <= bits_to_take {
            return self.clone();
        }

        let remainder_bits = bits_to_take % 8;
        if remainder_bits == 0 {
            return Key {
                value: self.value.slice(..bits_to_take / 8),
                length: bits_to_take,
            };
        }

        // Zero out everything to the right of the last token (at index
        // bits_to_take-1). Mask = (8-remainder_bits) 1s then remainder_bits 0s.
        let n = bytes_needed(bits_to_take);
        let mut buffer = vec![0u8; n];
        buffer.copy_from_slice(&self.value[..n]);
        let last = n - 1;
        buffer[last] &= 0xFF << dual_bit_index(remainder_bits);
        Key {
            value: Bytes::from(buffer),
            length: bits_to_take,
        }
    }

    /// Returns the in-order aggregation of this key with `keys`.
    /// Mirrors Go `Key.Extend`.
    #[must_use]
    pub fn extend(&self, keys: &[Key]) -> Key {
        let mut total_bit_length = self.length;
        for k in keys {
            total_bit_length += k.length;
        }
        let mut buffer = vec![0u8; bytes_needed(total_bit_length)];
        let copy_len = self.value.len().min(buffer.len());
        buffer[..copy_len].copy_from_slice(&self.value[..copy_len]);
        let mut current_total = self.length;
        for k in keys {
            extend_into_buffer(&mut buffer, k, current_total);
            current_total += k.length;
        }
        Key {
            value: Bytes::from(buffer),
            length: total_bit_length,
        }
    }
}

/// Crate-internal access to [`extend_into_buffer`] for the intermediate-node
/// store's sub-byte-token DB-key padding (Go `constructDBKey`).
pub(crate) fn extend_into_buffer_pub(buffer: &mut [u8], val: &Key, bits_offset: usize) {
    extend_into_buffer(buffer, val, bits_offset);
}

/// Writes `val` into `buffer` at bit offset `bits_offset`, ORing into the
/// partial byte if necessary. Mirrors Go `extendIntoBuffer`.
fn extend_into_buffer(buffer: &mut [u8], val: &Key, bits_offset: usize) {
    if val.length == 0 {
        return;
    }
    let bytes_offset = bytes_needed(bits_offset);
    let bits_remainder = bits_offset % 8;
    if bits_remainder == 0 {
        let len = val.value.len();
        buffer[bytes_offset..bytes_offset + len].copy_from_slice(&val.value);
        return;
    }

    // Fill the partial byte with the first [shift] bits of the extension path.
    buffer[bytes_offset - 1] |= val.value[0] >> bits_remainder;

    // Copy the rest, shifted by the dual of the remainder.
    shift_copy(
        &mut buffer[bytes_offset..],
        &val.value,
        dual_bit_index(bits_remainder),
    );
}

/// Treats `src` as a bit array and copies it into `dst` shifted left by `shift`
/// bits. Mirrors Go `shiftCopy`. Assumes `dst.len() >= src.len() - 1`.
fn shift_copy(dst: &mut [u8], src: &[u8], shift: usize) {
    if src.is_empty() {
        return;
    }
    let dual_shift = dual_bit_index(shift);
    let mut i = 0;
    while i + 1 < src.len() {
        dst[i] = (src[i] << shift) | (src[i + 1] >> dual_shift);
        i += 1;
    }
    // The last byte only has values from byte i (no byte i+1).
    if i < dst.len() {
        dst[i] = src[i] << shift;
    }
}

/// Returns the bit-length of the longest common prefix between `first` and
/// `second` (with `second` consulted from `second_offset`), counting in whole
/// tokens. Mirrors Go `getLengthOfCommonPrefix`.
#[must_use]
pub fn longest_common_prefix(
    first: &Key,
    second: &Key,
    second_offset: usize,
    token_size: usize,
) -> usize {
    let mut common_index = 0;
    while first.length > common_index
        && second.length > common_index + second_offset
        && first.token(common_index, token_size)
            == second.token(common_index + second_offset, token_size)
    {
        common_index += token_size;
    }
    common_index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_extraction() {
        // BranchFactor256: one token per byte.
        let k = Key::from_bytes(&[0x12, 0x34]);
        assert_eq!(k.length(), 16);
        assert_eq!(k.token(0, 8), 0x12);
        assert_eq!(k.token(8, 8), 0x34);

        // BranchFactor16: 4-bit tokens (nibbles).
        let k = Key::from_bytes(&[0xab]);
        assert_eq!(k.token(0, 4), 0xa);
        assert_eq!(k.token(4, 4), 0xb);

        // BranchFactor4: 2-bit tokens.
        let k = Key::from_bytes(&[0b11_01_10_00]);
        assert_eq!(k.token(0, 2), 0b11);
        assert_eq!(k.token(2, 2), 0b01);
        assert_eq!(k.token(4, 2), 0b10);
        assert_eq!(k.token(6, 2), 0b00);

        // BranchFactor2: single-bit tokens.
        let k = Key::from_bytes(&[0b1010_0000]);
        assert_eq!(k.token(0, 1), 1);
        assert_eq!(k.token(1, 1), 0);
        assert_eq!(k.token(2, 1), 1);
    }

    #[test]
    fn to_token_left_aligned() {
        // token_size 4: value 0xa stored as 0xa0.
        let t = Key::to_token(0xa, 4);
        assert_eq!(t.bytes(), &[0xa0]);
        assert_eq!(t.length(), 4);
        assert_eq!(t.token(0, 4), 0xa);

        // token_size 8: value stored as-is.
        let t = Key::to_token(0xab, 8);
        assert_eq!(t.bytes(), &[0xab]);
        assert_eq!(t.length(), 8);
    }

    #[test]
    fn skip_and_take() {
        let k = Key::from_bytes(&[0x12, 0x34, 0x56]);

        // Whole-byte skip.
        let s = k.skip(8);
        assert_eq!(s.length(), 16);
        assert_eq!(s.bytes(), &[0x34, 0x56]);

        // Whole-byte take.
        let t = k.take(8);
        assert_eq!(t.length(), 8);
        assert_eq!(t.bytes(), &[0x12]);

        // Partial take (4 bits of 0xff -> 0xf0).
        let k = Key::from_bytes(&[0xff]);
        let t = k.take(4);
        assert_eq!(t.length(), 4);
        assert_eq!(t.bytes(), &[0xf0]);
        assert!(t.has_partial_byte());

        // Skip beyond length yields empty.
        assert_eq!(k.skip(100), Key::empty());
        // Take beyond length yields the whole key.
        assert_eq!(k.take(100), k);
    }

    #[test]
    fn prefixes() {
        let k = Key::from_bytes(&[0x12, 0x34, 0x56]);
        assert!(k.has_prefix(&Key::from_bytes(&[0x12])));
        assert!(k.has_prefix(&Key::from_bytes(&[0x12, 0x34])));
        assert!(k.has_prefix(&k));
        assert!(k.has_prefix(&Key::empty()));
        assert!(!k.has_prefix(&Key::from_bytes(&[0x13])));
        assert!(k.has_strict_prefix(&Key::from_bytes(&[0x12])));
        assert!(!k.has_strict_prefix(&k));

        // Partial-byte prefix.
        let p = Key::from_bytes(&[0x1f]).take(4); // 4 bits: 0x10
        assert!(k.has_prefix(&p));

        // iterated_has_prefix with an offset.
        assert!(k.iterated_has_prefix(&Key::from_bytes(&[0x34]), 8, 8));
        assert!(!k.iterated_has_prefix(&Key::from_bytes(&[0x99]), 8, 8));
    }

    #[test]
    fn lcp() {
        let a = Key::from_bytes(&[0x12, 0x34]);
        let b = Key::from_bytes(&[0x12, 0x35]);
        assert_eq!(longest_common_prefix(&a, &b, 0, 8), 8);
        assert_eq!(longest_common_prefix(&a, &a, 0, 8), 16);
        // 4-bit tokens: 0x12 vs 0x13 share the high nibble (4 bits).
        let a = Key::from_bytes(&[0x12]);
        let b = Key::from_bytes(&[0x13]);
        assert_eq!(longest_common_prefix(&a, &b, 0, 4), 4);
    }

    #[test]
    fn ord_matches_go_compare() {
        // value compared first, then length.
        let a = Key::from_bytes(&[0x01]);
        let b = Key::from_bytes(&[0x02]);
        assert!(a < b);
        // same value bytes, shorter length is less.
        let short = Key::from_bytes(&[0xff]).take(4);
        let long = Key::from_bytes(&[0xf0]);
        // both have value 0xf0; short has length 4, long has length 8.
        assert_eq!(short.bytes(), long.bytes());
        assert!(short < long);
    }
}
