// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `BloomSet` — Go `network/p2p/gossip/bloom.go`'s `BloomFilter` (the raw,
//! `Set`-decoupled bloom-management logic: size/salt/reset), adapted to take
//! a `refill` callback on reset since this type does not own a persistent
//! inner set the way `gossip/set.go`'s generic `BloomSet[T]` does — see
//! [`BloomSet::reset_if_needed`].
//!
//! Wire format: [`ava_utils::bloom::Filter::marshal`] (`num_hashes || seeds
//! || entries`), exactly what Go's `bloom.Filter.Marshal()` produces, so a
//! filter gossiped out here parses cleanly with
//! [`ava_utils::bloom::ReadFilter::parse`] on a Go peer and vice versa.
//!
//! Float math (`OptimalParameters`/`EstimateCount` sizing) is Go-parity
//! network-filter sizing, not a consensus computation, so it is permitted
//! here per `specs/00-overview-and-conventions.md` §7.7 (the "no floats in
//! consensus paths" rule targets codec/consensus, not gossip dedup filters).

use ava_types::id::Id;
use ava_utils::bloom::{self, Filter, ReadFilter};

use crate::error::{Error, Result};

/// Default minimum target elements (Go `gossip.DefaultMinTargetElements`).
pub const DEFAULT_MIN_TARGET_ELEMENTS: usize = 1000;
/// Default target false positive probability (Go
/// `gossip.DefaultTargetFalsePositiveProbability`).
pub const DEFAULT_TARGET_FALSE_POSITIVE_PROBABILITY: f64 = 0.01;
/// Default reset false positive probability (Go
/// `gossip.DefaultResetFalsePositiveProbability`).
pub const DEFAULT_RESET_FALSE_POSITIVE_PROBABILITY: f64 = 0.05;

/// Length of the random salt, in bytes (matches `ids.ID`'s length).
const SALT_LEN: usize = 32;

/// A bloom filter over known gossip ids, with resets (fresh filter + fresh
/// salt) once its element count outgrows a false-positive-probability
/// threshold (Go `gossip.BloomFilter` / `gossip.NewBloomFilter`).
///
/// Unlike `gossip/set.go`'s `BloomSet[T]` (which resets automatically inside
/// every `Add`), this mirrors `bloom.go`'s explicit-trigger model: callers
/// invoke [`BloomSet::reset_if_needed`] themselves (e.g., periodically or on
/// churn), matching real Go callers such as coreth's atomic mempool
/// (`graft/coreth/plugin/evm/atomic/txpool/mempool.go`).
#[derive(Debug)]
pub struct BloomSet {
    bloom: Filter,
    salt: [u8; SALT_LEN],
    max_count: usize,
    min_target_elements: usize,
    target_false_positive_probability: f64,
    reset_false_positive_probability: f64,
}

impl BloomSet {
    /// Creates a new `BloomSet` (Go `NewBloomFilter`), applying the package
    /// defaults for any zero-valued parameter (Go
    /// `BloomSetConfig.fillDefaults`).
    ///
    /// # Errors
    /// Returns an error if the initial bloom filter could not be constructed.
    pub fn new(
        min_target_elements: usize,
        target_false_positive_probability: f64,
        reset_false_positive_probability: f64,
    ) -> Result<BloomSet> {
        let min_target_elements = if min_target_elements == 0 {
            DEFAULT_MIN_TARGET_ELEMENTS
        } else {
            min_target_elements
        };
        let target_false_positive_probability = if target_false_positive_probability == 0.0 {
            DEFAULT_TARGET_FALSE_POSITIVE_PROBABILITY
        } else {
            target_false_positive_probability
        };
        let reset_false_positive_probability = if reset_false_positive_probability == 0.0 {
            DEFAULT_RESET_FALSE_POSITIVE_PROBABILITY
        } else {
            reset_false_positive_probability
        };

        let (filter, salt, max_count) = new_bloom(
            min_target_elements,
            target_false_positive_probability,
            reset_false_positive_probability,
        )?;

        Ok(BloomSet {
            bloom: filter,
            salt,
            max_count,
            min_target_elements,
            target_false_positive_probability,
            reset_false_positive_probability,
        })
    }

    /// Adds `id` to the bloom filter (Go `BloomFilter.Add`).
    pub fn add(&mut self, id: &Id) {
        let h = bloom::hash(id.as_bytes(), &self.salt);
        self.bloom.add(h);
    }

    /// Returns whether `id` is (possibly) present (Go `BloomFilter.Has`).
    ///
    /// `ava_utils::bloom::Filter` (the writer half) does not expose a
    /// `contains` of its own — only the read-only `ReadFilter` half does —
    /// so this reuses the exact wire encoding via `marshal`+`parse`, which is
    /// no more expensive than what already happens whenever this filter is
    /// gossiped out to a peer.
    #[must_use]
    pub fn has(&self, id: &Id) -> bool {
        let bytes = self.bloom.marshal();
        let Ok(read_filter) = ReadFilter::parse(&bytes) else {
            return false;
        };
        read_filter.contains_key(id.as_bytes(), &self.salt)
    }

    /// Returns the current `(bloom_marshal_bytes, salt)` (Go
    /// `BloomFilter.BloomFilter()`, wire-encoded).
    #[must_use]
    pub fn marshal(&self) -> (Vec<u8>, Vec<u8>) {
        (self.bloom.marshal(), self.salt.to_vec())
    }

    /// Resets the bloom filter (fresh salt, freshly sized filter) if its
    /// element count has grown past the false-positive threshold, refilling
    /// it from `refill` (Go `ResetBloomFilterIfNeeded`, merged with
    /// `gossip/set.go`'s `BloomSet[T].resetBloom`'s refill behavior: since
    /// this type owns no persistent set to iterate, the caller supplies the
    /// currently-known ids via `refill`, which is handed an "add to the new
    /// filter" callback to invoke once per known id).
    ///
    /// `count_hint` is the target element count to size the new filter for
    /// (already including any caller-side churn multiplier, matching how Go
    /// callers pass e.g. `len*TxGossipBloomChurnMultiplier`); it is floored
    /// at `min_target_elements`.
    ///
    /// Returns whether a reset happened.
    ///
    /// # Errors
    /// Returns an error if a new bloom filter could not be constructed; the
    /// existing filter and salt are left unchanged.
    // The nested `FnMut` is the task-5 plan's exact signature (an "iterate
    // known ids into this callback" shape); a type alias would only push the
    // complexity behind a name.
    #[allow(clippy::type_complexity)]
    pub fn reset_if_needed(
        &mut self,
        count_hint: usize,
        refill: &mut dyn FnMut(&mut dyn FnMut(&Id)),
    ) -> Result<bool> {
        if self.bloom.count() <= self.max_count {
            return Ok(false);
        }

        let target_elements = count_hint.max(self.min_target_elements);
        let (new_filter, new_salt, new_max_count) = new_bloom(
            target_elements,
            self.target_false_positive_probability,
            self.reset_false_positive_probability,
        )?;

        self.bloom = new_filter;
        self.salt = new_salt;
        self.max_count = new_max_count;

        refill(&mut |id: &Id| self.add(id));

        Ok(true)
    }
}

/// Builds a fresh `(Filter, salt, max_count)` for `target_elements` (Go
/// `resetBloomFilter`): sizes via `OptimalParameters`, draws a fresh salt,
/// and estimates `max_count` via `EstimateCount` at `reset_fpp`.
fn new_bloom(
    target_elements: usize,
    target_false_positive_probability: f64,
    reset_false_positive_probability: f64,
) -> Result<(Filter, [u8; SALT_LEN], usize)> {
    let (num_hashes, num_entries) =
        bloom::optimal_parameters(target_elements, target_false_positive_probability);
    let filter = Filter::new(num_hashes, num_entries)
        .map_err(|e| Error::Set(format!("creating new bloom filter: {e}")))?;

    let mut salt = [0u8; SALT_LEN];
    bloom::fill_random_pub(&mut salt);

    let max_count =
        bloom::estimate_count(num_hashes, num_entries, reset_false_positive_probability);

    Ok((filter, salt, max_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_set_membership() {
        let mut bs = BloomSet::new(64, 0.01, 0.05).expect("BloomSet::new");
        let ids: Vec<Id> = (0u8..3).map(|i| Id::from([i; 32])).collect();
        for id in &ids {
            bs.add(id);
        }
        for id in &ids {
            assert!(bs.has(id), "added id {id:?} should be present");
        }
        let other = Id::from([99u8; 32]);
        assert!(
            !bs.has(&other),
            "unrelated id should (with overwhelming probability) be absent"
        );
    }

    #[test]
    fn bloom_set_marshal_readable_by_read_filter() {
        let mut bs = BloomSet::new(64, 0.01, 0.05).expect("BloomSet::new");
        let id = Id::from([7u8; 32]);
        bs.add(&id);

        let (bloom_bytes, salt) = bs.marshal();
        let read_filter = ReadFilter::parse(&bloom_bytes).expect("ReadFilter::parse of marshal()");
        assert!(
            read_filter.contains_key(id.as_bytes(), &salt),
            "marshaled filter is readable by ReadFilter and contains the added id"
        );
    }

    #[test]
    fn reset_regenerates_salt_and_refills() {
        // min_target_elements=16, target_fpp=0.5 -> a 4-entry-byte (32-bit),
        // 2-hash filter; reset_fpp=0.1 -> maxCount=7 (see
        // ava_utils::bloom::{optimal_parameters,estimate_count}). 32 bits
        // gives enough headroom that additions aren't undercounted by
        // saturation before maxCount is reached (unlike a 1-byte filter,
        // where bit collisions can stall `count()` right at the boundary),
        // so crossing maxCount within a few dozen distinct adds is reliable.
        let mut bs = BloomSet::new(16, 0.5, 0.1).expect("BloomSet::new");
        let all_ids: Vec<Id> = (0u8..64).map(|i| Id::from([i; 32])).collect();

        let (_, old_salt) = bs.marshal();

        let mut known: Vec<Id> = Vec::new();
        let mut reset_happened = false;
        for id in &all_ids {
            bs.add(id);
            known.push(*id);
            let did_reset = bs
                .reset_if_needed(known.len(), &mut |add| {
                    for k in &known {
                        add(k);
                    }
                })
                .expect("reset_if_needed");
            if did_reset {
                reset_happened = true;
                break;
            }
        }

        assert!(
            reset_happened,
            "reset should trigger well within 64 adds at the tiny maxCount"
        );
        let (_, new_salt) = bs.marshal();
        assert_ne!(old_salt, new_salt, "salt is regenerated on reset");
        for id in &known {
            assert!(
                bs.has(id),
                "id {id:?} known before reset should be refilled and still present"
            );
        }
    }
}
