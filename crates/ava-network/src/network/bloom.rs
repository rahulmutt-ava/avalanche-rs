// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! A byte-exact port of Go `utils/bloom` (the `ReadFilter` + `Hash` halves the
//! peer-list gossip needs).
//!
//! The wire format of a bloom filter is:
//!
//! ```text
//! num_hashes (1 byte) || hash_seeds (num_hashes * 8 BE) || entries (>=1 byte)
//! ```
//!
//! and `Hash(key, salt) = BE_u64( SHA256(key || salt)[..8] )`. `Contains`
//! rotates the hash left by 17 and XORs each seed in turn, matching Go's
//! `contains` exactly so a filter built by a Go peer reads identically here.
//!
//! > **Note:** Go puts this in `utils/bloom`; the avalanche-rs home is most
//! > naturally `ava-utils`. It is ported here (M2.17) so `ava-network` is
//! > self-contained for the handshake milestone; a later refactor can hoist it
//! > into `ava-utils` and have both crates share it.

/// Minimum number of hash seeds (Go `minHashes`).
const MIN_HASHES: usize = 1;
/// Maximum number of hash seeds (Go `maxHashes`).
const MAX_HASHES: usize = 16;
/// Minimum entries byte length (Go `minEntries`).
const MIN_ENTRIES: usize = 1;
/// Bits per entries byte.
const BITS_PER_BYTE: u64 = 8;
/// Bytes per `u64` seed.
const BYTES_PER_U64: usize = 8;
/// Left-rotation applied to the running hash per seed (Go `hashRotation`).
const HASH_ROTATION: u32 = 17;

/// Errors parsing a bloom filter wire blob.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BloomError {
    /// The blob was empty (no hash-count byte).
    #[error("invalid num hashes")]
    InvalidNumHashes,
    /// Fewer than `minHashes` seeds.
    #[error("too few hashes: {0} < {MIN_HASHES}")]
    TooFewHashes(usize),
    /// More than `maxHashes` seeds.
    #[error("too many hashes: {0} > {MAX_HASHES}")]
    TooManyHashes(usize),
    /// Fewer than `minEntries` entry bytes.
    #[error("too few entries")]
    TooFewEntries,
}

/// `Hash(key, salt)` — `BE_u64(SHA256(key || salt)[..8])` (Go `bloom.Hash`).
#[must_use]
pub fn hash(key: &[u8], salt: &[u8]) -> u64 {
    // SHA256(key || salt); avoids an extra hashing dependency by reusing
    // `ava-crypto`'s SHA-256 (identical to Go's `sha256.New().Write(key);
    // Write(salt)`).
    let mut buf = Vec::with_capacity(key.len().saturating_add(salt.len()));
    buf.extend_from_slice(key);
    buf.extend_from_slice(salt);
    let digest = ava_crypto::hashing::sha256(&buf);
    let mut first8 = [0u8; 8];
    if let Some(prefix) = digest.get(..8) {
        first8.copy_from_slice(prefix);
    }
    u64::from_be_bytes(first8)
}

/// A read-only bloom filter (Go `bloom.ReadFilter`).
#[derive(Debug, Clone)]
pub struct ReadFilter {
    hash_seeds: Vec<u64>,
    entries: Vec<u8>,
}

impl ReadFilter {
    /// Parse a wire blob into a `ReadFilter` (Go `bloom.Parse`).
    ///
    /// # Errors
    /// A [`BloomError`] if the blob is malformed or has out-of-range counts.
    pub fn parse(bytes: &[u8]) -> Result<ReadFilter, BloomError> {
        let num_hashes = *bytes.first().ok_or(BloomError::InvalidNumHashes)? as usize;
        if num_hashes < MIN_HASHES {
            return Err(BloomError::TooFewHashes(num_hashes));
        }
        if num_hashes > MAX_HASHES {
            return Err(BloomError::TooManyHashes(num_hashes));
        }
        // `num_hashes <= MAX_HASHES (16)`, so these products/sums never overflow
        // a `usize`; use checked arithmetic to satisfy the lint regardless.
        let entries_offset = num_hashes
            .checked_mul(BYTES_PER_U64)
            .and_then(|n| n.checked_add(1))
            .ok_or(BloomError::TooFewEntries)?;
        let min_len = entries_offset
            .checked_add(MIN_ENTRIES)
            .ok_or(BloomError::TooFewEntries)?;
        if bytes.len() < min_len {
            return Err(BloomError::TooFewEntries);
        }

        let mut hash_seeds = Vec::with_capacity(num_hashes);
        for i in 0..num_hashes {
            let start = i.saturating_mul(BYTES_PER_U64).saturating_add(1);
            let end = start.saturating_add(8);
            let raw = bytes.get(start..end).ok_or(BloomError::TooFewEntries)?;
            let mut seed = [0u8; 8];
            seed.copy_from_slice(raw);
            hash_seeds.push(u64::from_be_bytes(seed));
        }
        let entries = bytes
            .get(entries_offset..)
            .ok_or(BloomError::TooFewEntries)?
            .to_vec();
        Ok(ReadFilter {
            hash_seeds,
            entries,
        })
    }

    /// Returns whether `hash` is (possibly) present (Go `ReadFilter.Contains`).
    #[must_use]
    pub fn contains(&self, mut hash: u64) -> bool {
        let num_bits = BITS_PER_BYTE.saturating_mul(self.entries.len() as u64);
        if num_bits == 0 {
            return false;
        }
        let mut accumulator: u8 = 1;
        for &seed in &self.hash_seeds {
            if accumulator == 0 {
                break;
            }
            hash = hash.rotate_left(HASH_ROTATION) ^ seed;
            // `num_bits != 0` (early-returned above); use checked ops anyway.
            let index = hash.checked_rem(num_bits).unwrap_or(0);
            let byte_index = (index.checked_div(BITS_PER_BYTE).unwrap_or(0)) as usize;
            let bit_index = (index.checked_rem(BITS_PER_BYTE).unwrap_or(0)) as u32;
            let entry = self.entries.get(byte_index).copied().unwrap_or(0);
            accumulator &= entry.checked_shr(bit_index).unwrap_or(0);
        }
        accumulator != 0
    }

    /// Returns whether `key` salted with `salt` is (possibly) present.
    #[must_use]
    pub fn contains_key(&self, key: &[u8], salt: &[u8]) -> bool {
        self.contains(hash(key, salt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal one-hash, one-byte filter blob: `[num_hashes=1, seed(8 zero),
    /// entries=0x00]` — contains nothing.
    fn empty_filter() -> Vec<u8> {
        let mut v = vec![1u8];
        v.extend_from_slice(&0u64.to_be_bytes());
        v.push(0x00);
        v
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(matches!(
            ReadFilter::parse(&[]),
            Err(BloomError::InvalidNumHashes)
        ));
    }

    #[test]
    fn empty_filter_contains_nothing() {
        let f = ReadFilter::parse(&empty_filter()).expect("parse");
        assert!(!f.contains_key(b"anything", b""));
    }

    #[test]
    fn full_filter_contains_everything() {
        let mut v = vec![1u8];
        v.extend_from_slice(&0u64.to_be_bytes());
        v.push(0xFF);
        let f = ReadFilter::parse(&v).expect("parse");
        assert!(f.contains_key(b"x", b""));
        assert!(f.contains_key(b"y", b"salt"));
    }
}
