// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M3.6 tests: weight overflow, deterministic sampling vs the M0 sampler, and
//! add/remove/subset weight bookkeeping.

use std::collections::HashSet;

use assert_matches::assert_matches;
use proptest::prelude::*;

use ava_utils::rng::{Mt19937_64, Source};
use ava_utils::sampler::weighted_without_replacement::{
    WeightedWithoutReplacement, WeightedWithoutReplacementGeneric,
};

use ava_types::id::Id;
use ava_types::node_id::{NODE_ID_LEN, NodeId};
use ava_validators::error::Error;
use ava_validators::manager::{DefaultManager, ValidatorManager};
use ava_validators::set::Set;
use ava_validators::validator::Validator;

/// Builds a `NodeId` from a `u32` discriminator (big-endian in the low bytes).
fn node(i: u32) -> NodeId {
    let mut b = [0u8; NODE_ID_LEN];
    b[16..20].copy_from_slice(&i.to_be_bytes());
    NodeId::from(b)
}

fn staker(i: u32, weight: u64) -> Validator {
    Validator {
        node_id: node(i),
        public_key: None,
        tx_id: Id::default(),
        weight,
    }
}

#[test]
fn set_weight_overflow() {
    let mut set = Set::new();
    set.add_staker(staker(1, u64::MAX)).unwrap();
    set.add_staker(staker(2, 1)).unwrap();
    // Summing u64::MAX + 1 overflows.
    assert_matches!(set.total_weight(), Err(Error::WeightOverflow));

    // subset_weight overflows identically.
    let mut ids = HashSet::new();
    ids.insert(node(1));
    ids.insert(node(2));
    assert_matches!(set.subset_weight(&ids), Err(Error::WeightOverflow));
}

#[test]
fn add_remove_weight_roundtrip() {
    let mut set = Set::new();
    set.add_weight(node(1), 100).unwrap();
    assert_eq!(set.get_weight(node(1)), 100);

    set.add_weight(node(1), 50).unwrap();
    assert_eq!(set.get_weight(node(1)), 150);

    set.remove_weight(node(1), 30).unwrap();
    assert_eq!(set.get_weight(node(1)), 120);

    // Removing the full remaining weight drops the validator.
    set.remove_weight(node(1), 120).unwrap();
    assert_eq!(set.get_weight(node(1)), 0);
    assert!(set.is_empty());

    // Removing from an absent validator underflows.
    assert_matches!(
        set.remove_weight(node(1), 1),
        Err(Error::WeightUnderflow {
            requested: 1,
            present: 0
        })
    );

    // Zero weight is rejected.
    assert_matches!(set.add_weight(node(2), 0), Err(Error::ZeroWeight));
}

#[test]
fn subset_weight() {
    let mut set = Set::new();
    for (i, w) in [(1u32, 10u64), (2, 20), (3, 30), (4, 40)] {
        set.add_staker(staker(i, w)).unwrap();
    }
    assert_eq!(set.total_weight().unwrap(), 100);

    let mut subset = HashSet::new();
    subset.insert(node(2));
    subset.insert(node(4));
    assert_eq!(set.subset_weight(&subset).unwrap(), 60);

    // Empty subset → 0.
    assert_eq!(set.subset_weight(&HashSet::new()).unwrap(), 0);

    // Unknown ids contribute nothing.
    let mut unknown = HashSet::new();
    unknown.insert(node(99));
    assert_eq!(set.subset_weight(&unknown).unwrap(), 0);
}

#[test]
fn sorted_snapshot_is_node_id_ascending() {
    let mut set = Set::new();
    // Insert out of order.
    for i in [5u32, 1, 3, 2, 4] {
        set.add_staker(staker(i, u64::from(i) * 10)).unwrap();
    }
    let ids = set.get_validator_ids();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "validator ids must be NodeId-ascending");
    assert_eq!(ids, vec![node(1), node(2), node(3), node(4), node(5)]);
}

#[test]
fn manager_tracks_subnets_and_samples() {
    let mgr = DefaultManager::new();
    let subnet_a = Id::default();
    let subnet_b = Id::from([1u8; 32]);

    mgr.add_staker(subnet_a, node(1), None, Id::default(), 10)
        .unwrap();
    mgr.add_staker(subnet_a, node(2), None, Id::default(), 20)
        .unwrap();
    mgr.add_weight(subnet_b, node(3), 5).unwrap();

    assert_eq!(mgr.num_subnets(), 2);
    assert_eq!(mgr.num_validators(subnet_a), 2);
    assert_eq!(mgr.total_weight(subnet_a).unwrap(), 30);
    assert_eq!(mgr.get_weight(subnet_a, node(2)), 20);
    assert_eq!(mgr.get_validator_ids(subnet_a), vec![node(1), node(2)]);

    // Duplicate add fails.
    assert_matches!(
        mgr.add_staker(subnet_a, node(1), None, Id::default(), 1),
        Err(Error::DuplicateValidator { .. })
    );

    // Sampling returns the right count, all from the subnet.
    let sampled = mgr.sample(subnet_a, 2).unwrap();
    assert_eq!(sampled.len(), 2);
    for n in &sampled {
        assert!(*n == node(1) || *n == node(2));
    }

    // Go parity: sampling MORE than the validator count still succeeds while
    // the TOTAL WEIGHT (30) suffices — the extra draws repeat validators
    // proportionally to their weight (weight-unit sampling with duplicates).
    let sampled3 = mgr.sample(subnet_a, 3).unwrap();
    assert_eq!(sampled3.len(), 3);
    for n in &sampled3 {
        assert!(*n == node(1) || *n == node(2));
    }

    // Over-sampling past the TOTAL WEIGHT fails.
    assert_matches!(
        mgr.sample(subnet_a, 31),
        Err(Error::InsufficientValidators { requested: 31 })
    );

    // Unknown subnet samples to MissingValidators.
    assert_matches!(
        mgr.sample(Id::from([9u8; 32]), 1),
        Err(Error::MissingValidators)
    );

    // Remove all weight drops the validator.
    mgr.remove_weight(subnet_a, node(1), 10).unwrap();
    assert_eq!(mgr.num_validators(subnet_a), 1);
}

/// Go-parity: `Set::sample(k)` draws WEIGHT UNITS without replacement, so `k`
/// may exceed the validator COUNT — a single validator whose weight spans
/// multiple drawn weight positions appears MULTIPLE times in one sample. Go's
/// `snow/validators/set.go` `sample` has NO `len()` guard: it defers entirely
/// to `sampler.WeightedWithoutReplacement`, which returns duplicate indices in
/// exactly this case. The old `size > self.validators.len()` guard wrongly
/// rejected this — the live-proven `k=20`-on-5-validators bug (a networked
/// node could not even sample its poll set).
#[test]
fn sample_exceeding_validator_count_succeeds_with_duplicates() {
    let mut set = Set::new();
    // 5 heavy, equally-weighted validators: total weight 5_000 >> k = 20.
    for i in 0..5u32 {
        set.add_staker(staker(i, 1_000)).unwrap();
    }
    let mut src = Mt19937_64::new();
    src.seed(42);
    let sampled = set
        .sample(20, Box::new(src) as Box<dyn Source>)
        .expect("sample(20) over 5 heavy validators must succeed (Go parity)");
    assert_eq!(sampled.len(), 20, "the sample holds exactly k ids");
    // Only 5 distinct validators exist, so 20 draws MUST repeat some of them
    // (weight-unit sampling with duplicates).
    let unique: HashSet<NodeId> = sampled.iter().copied().collect();
    assert!(
        unique.len() < sampled.len(),
        "20 draws over 5 validators must repeat at least one id"
    );
    assert!(
        unique.len() <= 5,
        "no id outside the 5-validator set appears"
    );
}

/// Go-parity boundary: sampling exactly the TOTAL WEIGHT succeeds (a full
/// permutation of the weight units — each validator appears weight-many times),
/// but sampling MORE than the total weight fails. Go returns
/// `errInsufficientWeight`; our sampler yields `None`, surfaced as
/// [`Error::InsufficientValidators`]. The failure boundary is TOTAL WEIGHT, not
/// validator count.
#[test]
fn sample_boundary_is_total_weight_not_count() {
    // 2 validators, total weight 6.
    let build = || {
        let mut set = Set::new();
        set.add_staker(staker(1, 3)).unwrap();
        set.add_staker(staker(2, 3)).unwrap();
        set
    };

    // size == total weight (6 > count 2): succeeds with a full permutation.
    let mut src = Mt19937_64::new();
    src.seed(7);
    let ok = build()
        .sample(6, Box::new(src) as Box<dyn Source>)
        .expect("sample(total_weight) must succeed even when it exceeds the count");
    assert_eq!(ok.len(), 6);
    let mut counts = std::collections::HashMap::new();
    for id in &ok {
        *counts.entry(*id).or_insert(0u32) += 1;
    }
    assert_eq!(
        counts.get(&node(1)),
        Some(&3),
        "each weight-3 validator drawn 3x"
    );
    assert_eq!(
        counts.get(&node(2)),
        Some(&3),
        "each weight-3 validator drawn 3x"
    );

    // size > total weight (7 > 6): fails on insufficient TOTAL WEIGHT.
    let mut src = Mt19937_64::new();
    src.seed(7);
    assert_matches!(
        build().sample(7, Box::new(src) as Box<dyn Source>),
        Err(Error::InsufficientValidators { requested: 7 })
    );
}

proptest! {
    /// For a fixed `(validators, seed)`, `Set::sample` reproduces the *exact*
    /// index sequence the M0 `WeightedWithoutReplacementGeneric` produces over the
    /// NodeId-sorted weight slice — i.e. the validator sampling is bit-for-bit
    /// consistent with the M0 RNG (the R1 determinism risk). We also confirm the
    /// NodeId-sorted ordering: the i-th sampled index maps to the i-th NodeId in
    /// ascending order.
    #[test]
    fn sample_determinism(
        // distinct node discriminators with positive weights
        weights in prop::collection::vec(1u64..1_000_000, 1..32),
        seed in any::<u64>(),
        size in 1usize..32,
    ) {
        let n = weights.len();
        let size = size.min(n);

        // Build the set with node(i) carrying weights[i].
        let mut set = Set::new();
        for (i, w) in weights.iter().enumerate() {
            set.add_staker(staker(i as u32, *w)).unwrap();
        }

        // The NodeId-sorted snapshot. node(i) sorts by big-endian i, so index i
        // already equals NodeId rank — assert that explicitly.
        let sorted = set.sorted_weights();
        for (i, (id, w)) in sorted.iter().enumerate() {
            prop_assert_eq!(*id, node(i as u32));
            prop_assert_eq!(*w, weights[i]);
        }
        let sorted_weights: Vec<u64> = sorted.iter().map(|(_, w)| *w).collect();

        // Oracle: drive the M0 sampler directly with the same weights + a freshly
        // seeded gonum MT19937_64, and capture the raw index sequence.
        let mut oracle_src = Mt19937_64::new();
        oracle_src.seed(seed);
        let mut oracle = WeightedWithoutReplacementGeneric::new(Box::new(oracle_src));
        oracle.initialize(&sorted_weights).unwrap();
        let oracle_indices = oracle.sample(size).unwrap();

        // The Set sampler under test, with an identically-seeded source.
        let mut src = Mt19937_64::new();
        src.seed(seed);
        let sampled = set.sample(size, Box::new(src) as Box<dyn Source>).unwrap();

        // The NodeIds out must equal the oracle indices mapped through the sorted
        // order — and because node(i) ranks at index i, the NodeId is node(idx).
        let expected: Vec<NodeId> = oracle_indices.iter().map(|i| sorted[*i].0).collect();
        prop_assert_eq!(&sampled, &expected);
        let expected_by_rank: Vec<NodeId> =
            oracle_indices.iter().map(|i| node(*i as u32)).collect();
        prop_assert_eq!(&sampled, &expected_by_rank);
    }
}
