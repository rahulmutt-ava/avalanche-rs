// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Property tests for `ava-utils` (M0.24, specs/02 §4.2):
//!
//! - safemath `add`/`sub`/`mul` against a `u128` reference oracle.
//! - `Bits` set algebra against a `BTreeSet<u64>` oracle + big-endian
//!   round-trip.
//! - CB58 checksum round-trip + decode never panics on arbitrary strings.
//! - weighted-without-replacement: `count` distinct indices, no repeats.

use std::collections::BTreeSet;

use ava_utils::bits::Bits;
use ava_utils::cb58::{cb58_decode, cb58_encode};
use ava_utils::error::Error;
use ava_utils::math;
use ava_utils::rng::Mt19937_64;
use ava_utils::sampler::uniform::Uniform;
use ava_utils::sampler::weighted_without_replacement::WeightedWithoutReplacement;
use ava_utils::sampler::{
    new_deterministic_uniform, new_deterministic_weighted_without_replacement,
};
use proptest::prelude::*;

mod prop {
    use super::*;

    proptest! {
        /// `math::add`/`sub`/`mul` agree exactly with a `u128` reference: `Ok`
        /// with the exact value when it fits in `u64`, else the matching error.
        #[test]
        fn safemath_matches_u128_reference(a in any::<u64>(), b in any::<u64>()) {
            // add -> Overflow
            let add_ref = a as u128 + b as u128;
            if add_ref <= u64::MAX as u128 {
                prop_assert_eq!(math::add(a, b), Ok(add_ref as u64));
            } else {
                prop_assert_eq!(math::add(a, b), Err(Error::Overflow));
            }

            // sub -> Underflow
            if a >= b {
                prop_assert_eq!(math::sub(a, b), Ok(a - b));
            } else {
                prop_assert_eq!(math::sub(a, b), Err(Error::Underflow));
            }

            // mul -> Overflow
            let mul_ref = a as u128 * b as u128;
            if mul_ref <= u64::MAX as u128 {
                prop_assert_eq!(math::mul(a, b), Ok(mul_ref as u64));
            } else {
                prop_assert_eq!(math::mul(a, b), Err(Error::Overflow));
            }
        }
    }

    /// Builds a `Bits` set from a collection of indices.
    fn bits_from(indices: &BTreeSet<u64>) -> Bits {
        let mut bits = Bits::new();
        for &i in indices {
            bits.add(i);
        }
        bits
    }

    proptest! {
        /// `Bits` union/intersection/difference/len match a `BTreeSet<u64>`
        /// oracle. Indices are kept small so the backing `BigUint` stays cheap.
        #[test]
        fn bits_set_algebra(
            a_idx in proptest::collection::btree_set(0u64..256, 0..16),
            b_idx in proptest::collection::btree_set(0u64..256, 0..16),
        ) {
            let a = bits_from(&a_idx);
            let b = bits_from(&b_idx);

            // len == popcount == oracle cardinality.
            prop_assert_eq!(a.len(), a_idx.len() as u64);
            prop_assert_eq!(b.len(), b_idx.len() as u64);

            // membership.
            for i in 0u64..256 {
                prop_assert_eq!(a.contains(i), a_idx.contains(&i));
                prop_assert_eq!(b.contains(i), b_idx.contains(&i));
            }

            let union = Bits::union(&a, &b);
            let union_ref: BTreeSet<u64> = a_idx.union(&b_idx).copied().collect();
            prop_assert_eq!(union.len(), union_ref.len() as u64);
            for &i in &union_ref {
                prop_assert!(union.contains(i));
            }

            let inter = Bits::intersection(&a, &b);
            let inter_ref: BTreeSet<u64> = a_idx.intersection(&b_idx).copied().collect();
            prop_assert_eq!(inter.len(), inter_ref.len() as u64);
            for &i in &inter_ref {
                prop_assert!(inter.contains(i));
            }

            let diff = Bits::difference(&a, &b);
            let diff_ref: BTreeSet<u64> = a_idx.difference(&b_idx).copied().collect();
            prop_assert_eq!(diff.len(), diff_ref.len() as u64);
            for &i in &diff_ref {
                prop_assert!(diff.contains(i));
            }
            // Difference excludes everything in b.
            for &i in &b_idx {
                prop_assert!(!diff.contains(i));
            }
        }
    }

    proptest! {
        /// `Bits::from_bytes(b).bytes()` reproduces `b` modulo leading-zero
        /// normalization: the backing `BigUint` is value-based, so leading
        /// zero bytes are dropped (an all-zero input yields the empty slice).
        #[test]
        fn bits_from_bytes_roundtrip(b in proptest::collection::vec(any::<u8>(), 0..32)) {
            let got = Bits::from_bytes(&b).bytes();
            // Reference: strip leading zero bytes (big-endian normalization).
            let want: &[u8] = match b.iter().position(|&x| x != 0) {
                Some(p) => &b[p..],
                None => &[],
            };
            prop_assert_eq!(got.as_slice(), want);
        }
    }

    proptest! {
        /// CB58 checksum round-trip: `cb58_decode(cb58_encode(d)) == d`.
        #[test]
        fn cb58_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..256)) {
            let encoded = cb58_encode(&data).expect("encode small payload");
            let decoded = cb58_decode(&encoded).expect("decode our own encoding");
            prop_assert_eq!(decoded, data);
        }

        /// `cb58_decode` never panics on arbitrary strings (returns Ok/Err).
        #[test]
        fn cb58_decode_never_panics(s in ".{0,64}") {
            let _ = cb58_decode(&s);
        }
    }

    proptest! {
        /// The uniform-without-replacement sampler (the component that owns the
        /// distinctness guarantee, Go `uniformReplacer`) returns exactly `count`
        /// **distinct** indices in `[0, length)` for arbitrary seed and length.
        ///
        /// NB: distinctness is a property of the *uniform* sampler over slot
        /// space, NOT of `WeightedWithoutReplacement` over weight space — the
        /// latter intentionally repeats an index whose weight band is drawn
        /// more than once (see Go `utils/sampler` + golden vectors in
        /// `tests/golden_samplers.rs`, e.g. wwr `[1, 1, 4]`). Asserting "no
        /// repeats" against the weighted sampler would contradict the real API.
        #[test]
        fn uniform_wor_distinct_and_count(
            seed in any::<u64>(),
            length in 1u64..256,
            req in 0u64..256,
        ) {
            let count = req.min(length);

            let mut g = Mt19937_64::new();
            g.seed(seed);
            let mut sampler = new_deterministic_uniform(Box::new(g));
            sampler.initialize(length);

            let sampled = sampler.sample(count as usize).expect("count <= length");
            prop_assert_eq!(sampled.len() as u64, count);

            // Distinct (without replacement) and in range.
            let distinct: BTreeSet<u64> = sampled.iter().copied().collect();
            prop_assert_eq!(distinct.len() as u64, count);
            for &i in &sampled {
                prop_assert!(i < length);
            }
        }

        /// Weighted-without-replacement: `sample(count)` returns exactly `count`
        /// indices, each a valid index into `weights`, for arbitrary seed and
        /// weights. Indices may repeat (without replacement is over the weight
        /// space, matching Go) — so this asserts count + range, not distinctness.
        #[test]
        fn wwr_count_and_range(
            seed in any::<u64>(),
            weights in proptest::collection::vec(1u64..1_000_000, 1..32),
            req in 0usize..32,
        ) {
            let count = req.min(weights.len());

            let mut g = Mt19937_64::new();
            g.seed(seed);
            let mut sampler = new_deterministic_weighted_without_replacement(Box::new(g));
            sampler.initialize(&weights).expect("init wwr");

            let sampled = sampler.sample(count).expect("sample count <= total weight");
            prop_assert_eq!(sampled.len(), count);

            // Every returned value is a valid index into `weights`.
            for &i in &sampled {
                prop_assert!(i < weights.len());
            }
        }
    }
}
