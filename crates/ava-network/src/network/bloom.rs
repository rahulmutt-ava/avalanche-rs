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

/// Fill `buf` with CSPRNG bytes for callers outside this module (the salt).
pub fn fill_random_pub(buf: &mut [u8]) {
    fill_random(buf);
}

/// Fill `buf` with cryptographically-random bytes (Go `crypto/rand`). On the
/// (practically impossible) CSPRNG failure, `buf` is left as-is (all zeros),
/// which is still a *valid* bloom filter — only less random — so callers stay
/// infallible.
fn fill_random(buf: &mut [u8]) {
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    if rng.fill(buf).is_err() {
        // Leave zeros: a zero-seed / zero-salt filter still parses and works.
    }
}

/// A writable bloom filter (Go `bloom.Filter`). Lock-free: callers wrap it in
/// their own lock (`IpTracker` holds it inside its `Mutex`).
#[derive(Debug, Clone)]
pub struct Filter {
    num_bits: u64,
    hash_seeds: Vec<u64>,
    entries: Vec<u8>,
    count: usize,
}

impl Filter {
    /// Create a filter with `num_hashes` random seeds and `num_entries` bytes
    /// (Go `bloom.New`).
    ///
    /// # Errors
    /// [`BloomError`] if `num_entries < minEntries` or `num_hashes` is outside
    /// `[minHashes, maxHashes]`.
    pub fn new(num_hashes: usize, num_entries: usize) -> Result<Filter, BloomError> {
        if num_entries < MIN_ENTRIES {
            return Err(BloomError::TooFewEntries);
        }
        if num_hashes < MIN_HASHES {
            return Err(BloomError::TooFewHashes(num_hashes));
        }
        if num_hashes > MAX_HASHES {
            return Err(BloomError::TooManyHashes(num_hashes));
        }

        let mut seed_bytes = vec![0u8; num_hashes.saturating_mul(BYTES_PER_U64)];
        fill_random(&mut seed_bytes);
        let mut hash_seeds = Vec::with_capacity(num_hashes);
        for i in 0..num_hashes {
            let start = i.saturating_mul(BYTES_PER_U64);
            let end = start.saturating_add(BYTES_PER_U64);
            let mut s = [0u8; 8];
            if let Some(chunk) = seed_bytes.get(start..end) {
                s.copy_from_slice(chunk);
            }
            hash_seeds.push(u64::from_be_bytes(s));
        }

        Ok(Filter {
            num_bits: (num_entries as u64).saturating_mul(BITS_PER_BYTE),
            hash_seeds,
            entries: vec![0u8; num_entries],
            count: 0,
        })
    }

    /// A minimal valid empty filter (1 hash, 1 entry) — infallible fallback.
    #[must_use]
    pub fn minimal() -> Filter {
        let mut seed = [0u8; 8];
        fill_random(&mut seed);
        Filter {
            num_bits: BITS_PER_BYTE,
            hash_seeds: vec![u64::from_be_bytes(seed)],
            entries: vec![0u8; MIN_ENTRIES],
            count: 0,
        }
    }

    /// Add `hash` to the filter (Go `Filter.Add`). Returns `true` if it was not
    /// already present.
    pub fn add(&mut self, mut hash: u64) -> bool {
        if self.num_bits == 0 {
            return false;
        }
        let mut accumulator: u8 = 1;
        for &seed in &self.hash_seeds {
            hash = hash.rotate_left(HASH_ROTATION) ^ seed;
            let index = hash.checked_rem(self.num_bits).unwrap_or(0);
            let byte_index = (index.checked_div(BITS_PER_BYTE).unwrap_or(0)) as usize;
            let bit_index = (index.checked_rem(BITS_PER_BYTE).unwrap_or(0)) as u32;
            if let Some(entry) = self.entries.get_mut(byte_index) {
                accumulator &= entry.checked_shr(bit_index).unwrap_or(0);
                *entry |= 1u8.checked_shl(bit_index).unwrap_or(0);
            }
        }
        let added = accumulator == 0;
        if added {
            self.count = self.count.saturating_add(1);
        }
        added
    }

    /// Add `key` salted with `salt` (Go free function `bloom.Add`).
    pub fn add_key(&mut self, key: &[u8], salt: &[u8]) -> bool {
        self.add(hash(key, salt))
    }

    /// Serialize to the wire format `[num_hashes] || seeds(BE) || entries`
    /// (Go `Filter.Marshal` / `marshal`).
    #[must_use]
    pub fn marshal(&self) -> Vec<u8> {
        let num_hashes = self.hash_seeds.len();
        let entries_offset = num_hashes.saturating_mul(BYTES_PER_U64).saturating_add(1);
        let mut out = vec![0u8; entries_offset.saturating_add(self.entries.len())];
        if let Some(b) = out.first_mut() {
            *b = num_hashes as u8;
        }
        for (i, seed) in self.hash_seeds.iter().enumerate() {
            let start = i.saturating_mul(BYTES_PER_U64).saturating_add(1);
            let end = start.saturating_add(BYTES_PER_U64);
            if let Some(slot) = out.get_mut(start..end) {
                slot.copy_from_slice(&seed.to_be_bytes());
            }
        }
        if let Some(slot) = out.get_mut(entries_offset..) {
            slot.copy_from_slice(&self.entries);
        }
        out
    }

    /// Number of elements added (Go `Filter.Count`).
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }
}

/// Natural-log-of-2, matching Go `math.Ln2`.
const LN2: f64 = std::f64::consts::LN_2;

/// `OptimalParameters` (Go `utils/bloom/optimal.go`): returns the
/// `(num_hashes, num_entries)` minimizing size for `count` elements at
/// `false_positive_probability`. Bloom *sizing* only — not a consensus path.
#[must_use]
pub fn optimal_parameters(count: usize, false_positive_probability: f64) -> (usize, usize) {
    let num_entries = optimal_entries(count, false_positive_probability);
    let num_hashes = optimal_hashes(num_entries, count);
    (num_hashes, num_entries)
}

/// `OptimalEntries` (Go).
#[must_use]
pub fn optimal_entries(count: usize, false_positive_probability: f64) -> usize {
    if count == 0 {
        return MIN_ENTRIES;
    }
    if false_positive_probability >= 1.0 {
        return MIN_ENTRIES;
    }
    if false_positive_probability <= 0.0 {
        return usize::MAX;
    }
    let ln2_squared = LN2 * LN2;
    let entries_in_bits = -(count as f64) * false_positive_probability.ln() / ln2_squared;
    let entries = (entries_in_bits + (BITS_PER_BYTE as f64) - 1.0) / (BITS_PER_BYTE as f64);
    if entries >= usize::MAX as f64 {
        return usize::MAX;
    }
    (entries as usize).max(MIN_ENTRIES)
}

/// `OptimalHashes` (Go).
#[must_use]
pub fn optimal_hashes(num_entries: usize, count: usize) -> usize {
    if num_entries < MIN_ENTRIES {
        return MIN_HASHES;
    }
    if count == 0 {
        return MAX_HASHES;
    }
    let num_hashes =
        ((num_entries as f64) * (BITS_PER_BYTE as f64) * LN2 / (count as f64)).ceil();
    if num_hashes >= MAX_HASHES as f64 {
        return MAX_HASHES;
    }
    (num_hashes as usize).max(MIN_HASHES)
}

/// `EstimateCount` (Go): the element count at which the filter reaches
/// `false_positive_probability`.
#[must_use]
pub fn estimate_count(
    num_hashes: usize,
    num_entries: usize,
    false_positive_probability: f64,
) -> usize {
    if num_hashes < MIN_HASHES || num_entries < MIN_ENTRIES || false_positive_probability <= 0.0 {
        return 0;
    }
    if false_positive_probability >= 1.0 {
        return usize::MAX;
    }
    let inv_num_hashes = 1.0 / (num_hashes as f64);
    let num_bits = (num_entries as f64) * (BITS_PER_BYTE as f64);
    let exp = 1.0 - false_positive_probability.powf(inv_num_hashes);
    let count = (-exp.ln() * num_bits * inv_num_hashes).ceil();
    if count >= usize::MAX as f64 {
        return usize::MAX;
    }
    count as usize
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

    #[test]
    fn filter_marshal_roundtrips_through_read_filter() {
        let mut f = Filter::new(3, 16).expect("Filter::new");
        f.add_key(b"node-a", b"salt");
        let bytes = f.marshal();
        // marshal layout: [num_hashes] || seeds(n*8) || entries
        assert_eq!(*bytes.first().expect("marshal non-empty") as usize, 3, "num_hashes byte");
        assert_eq!(bytes.len(), 1 + 3 * 8 + 16, "marshal length");
        let rf = ReadFilter::parse(&bytes).expect("ReadFilter::parse of marshaled Filter");
        assert!(rf.contains_key(b"node-a", b"salt"), "added key present after roundtrip");
    }

    #[test]
    fn filter_add_then_contains() {
        let mut f = Filter::new(8, 256).expect("Filter::new");
        assert!(f.add_key(b"present", b""), "first add returns true (newly added)");
        assert_eq!(f.count(), 1, "count after one add");
        let rf = ReadFilter::parse(&f.marshal()).expect("parse");
        assert!(rf.contains_key(b"present", b""), "present key contained");
        assert!(!rf.contains_key(b"absent-key-xyz", b""), "absent key not contained");
    }

    #[test]
    fn filter_minimal_is_valid_empty() {
        let f = Filter::minimal();
        let bytes = f.marshal();
        assert_eq!(bytes.len(), 1 + 8 + 1, "minimal = 1 hash, 1 entry");
        let rf = ReadFilter::parse(&bytes).expect("minimal parses");
        assert!(!rf.contains_key(b"anything", b""), "minimal contains nothing");
    }

    #[test]
    fn filter_new_rejects_too_few_entries() {
        assert!(matches!(Filter::new(1, 0), Err(BloomError::TooFewEntries)));
    }

    #[test]
    fn optimal_parameters_matches_go_reference() {
        // Reference computed from avalanchego utils/bloom/optimal.go for the
        // ip_tracker fresh-node inputs (count = minCountEstimate = 128,
        // targetFalsePositiveProbability = 0.001).
        let (num_hashes, num_entries) = optimal_parameters(128, 0.001);
        assert_eq!(num_hashes, 10, "num_hashes for (128, 0.001)");
        assert_eq!(num_entries, 230, "num_entries for (128, 0.001)");
    }

    #[test]
    fn optimal_parameters_are_buildable_and_marshal_to_311_bytes() {
        let (nh, ne) = optimal_parameters(128, 0.001);
        let f = Filter::new(nh, ne).expect("Filter::new with optimal params");
        assert_eq!(f.marshal().len(), 1 + 10 * 8 + 230, "fresh empty filter is 311 bytes");
    }

    #[test]
    fn optimal_entries_floors_and_caps() {
        assert_eq!(optimal_entries(0, 0.001), 1, "non-positive count -> minEntries");
        assert_eq!(optimal_entries(128, 1.0), 1, "fpp>=1 -> minEntries");
    }

    #[test]
    fn optimal_hashes_floors_and_caps() {
        assert_eq!(optimal_hashes(0, 128), 1, "numEntries<minEntries -> minHashes");
        assert_eq!(optimal_hashes(230, 0), 16, "count<=0 -> maxHashes");
    }
}
