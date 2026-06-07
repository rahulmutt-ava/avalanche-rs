// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Weight-diff and public-key-diff disk stores + backward-reconstruction
//! iterators (`vms/platformvm/state/state.go` `ApplyValidatorWeightDiffs` /
//! `ApplyValidatorPublicKeyDiffs`, specs 08 §7.1).
//!
//! Current validator weights and BLS keys are held in memory at the current
//! (last-accepted) height. Historical sets are reconstructed by applying weight
//! diffs and public-key diffs **backward** over a height window `[to, from]`
//! (i.e. `(target_height, current_height]`). Because the on-disk key inverts the
//! height (`inverse_height`, see [`disk_staker_diff_iterator`](super::disk_staker_diff_iterator)),
//! a forward lexicographic scan visits the newest height first — exactly the
//! order backward reconstruction needs.
//!
//! ## Layout (two parallel indexes per kind)
//!
//! Mirroring Go's four prefix DBs (`flatValidatorDiffs`,
//! `flatValidatorDiffsByHeight`, `flatPublicKeyDiffs`,
//! `flatPublicKeyDiffsByHeight`), each store keeps:
//!
//! - a **by-subnet** index ([`disk_staker_diff_iterator::marshal_diff_key_by_subnet_id`])
//!   for single-subnet reconstruction (scanned with the subnet id as prefix),
//!   and
//! - a **by-height** index ([`disk_staker_diff_iterator::marshal_diff_key_by_height`])
//!   for all-subnets reconstruction.
//!
//! The weight store sits under [`prefixes::WEIGHT_DIFF_PREFIX`] and the pk store
//! under [`prefixes::PK_DIFF_PREFIX`]; within each, the by-height index is a
//! joined sub-space (the literal prefix bytes are an on-disk migration concern,
//! not consensus — see `prefixes.rs`).
//!
//! ## Un-applying a diff
//!
//! A weight diff records the change that happened *at* its height:
//! `decrease = true` ⇒ the weight went down, so the prior (older) weight was
//! *higher*. Walking backward therefore **adds** the amount for a decrease and
//! **subtracts** it for an increase. A pk diff stores the BLS key the node *had
//! before* the change, so backward reconstruction restores that stored value.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_database::{Database, Iteratee, Iterator as _, KeyValueWriter, PrefixDb};
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::{Error, Result};
use crate::state::disk_staker_diff_iterator::{
    ValidatorWeightDiff, marshal_diff_key_by_height, marshal_diff_key_by_subnet_id,
    marshal_start_diff_key_by_height, marshal_start_diff_key_by_subnet_id, marshal_weight_diff,
    unmarshal_diff_key_by_height, unmarshal_diff_key_by_subnet_id, unmarshal_weight_diff,
};

/// Sub-prefix joined under the weight/pk parent prefix for the by-subnet index.
/// The literal byte value is a migration concern, not consensus (`prefixes.rs`).
const BY_SUBNET_SUFFIX: &[u8] = b"bySubnetID";
/// Sub-prefix joined under the weight/pk parent prefix for the by-height index.
const BY_HEIGHT_SUFFIX: &[u8] = b"byHeight";

/// The minimal per-node weight projection a reconstruction maintains: a weight
/// and (optionally) the node's prior BLS public key bytes.
///
/// Kept deliberately decoupled from `ava-validators::GetValidatorOutput`: the
/// disk layer speaks raw uncompressed BLS key *bytes* (exactly what Go stores
/// and what `PublicKeyFromValidUncompressedBytes` consumes), leaving the
/// `bls::PublicKey` parse to the validator manager (M4.21). `public_key == None`
/// means the node had no key at the reconstructed height.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffValidator {
    /// The node's voting weight at the reconstructed height.
    pub weight: u64,
    /// The node's uncompressed BLS public key bytes, or `None`.
    pub public_key: Option<Vec<u8>>,
}

/// The persisted staker weight-diff store: a by-subnet index + a by-height index
/// over the [`prefixes::WEIGHT_DIFF_PREFIX`](super::prefixes::WEIGHT_DIFF_PREFIX)
/// space.
pub struct WeightDiffStore<D: Database> {
    by_subnet: PrefixDb<D>,
    by_height: PrefixDb<D>,
}

impl<D: Database> WeightDiffStore<D> {
    /// Builds the store over the weight-diff parent prefix space.
    #[must_use]
    pub fn new(parent: &PrefixDb<D>) -> Self {
        Self {
            by_subnet: parent.join(BY_SUBNET_SUFFIX),
            by_height: parent.join(BY_HEIGHT_SUFFIX),
        }
    }

    /// Records a weight diff for `(subnet, node)` at `height` into both indexes.
    ///
    /// Mirrors Go `writeValidatorDiffs`: a zero-amount diff is not written.
    ///
    /// # Errors
    /// Propagates the base [`Database`] write error.
    pub fn put(
        &self,
        subnet: Id,
        node: NodeId,
        height: u64,
        diff: &ValidatorWeightDiff,
    ) -> Result<()> {
        if diff.amount == 0 {
            return Ok(());
        }
        let value = marshal_weight_diff(diff);
        self.by_subnet
            .put(&marshal_diff_key_by_subnet_id(subnet, height, node), &value)?;
        self.by_height
            .put(&marshal_diff_key_by_height(height, subnet, node), &value)?;
        Ok(())
    }
}

/// The persisted staker public-key-diff store: a by-subnet index + a by-height
/// index over the [`prefixes::PK_DIFF_PREFIX`](super::prefixes::PK_DIFF_PREFIX)
/// space. The value is the uncompressed BLS key bytes the node *had before* the
/// change at that height (empty ⇒ no prior key).
pub struct PublicKeyDiffStore<D: Database> {
    by_subnet: PrefixDb<D>,
    by_height: PrefixDb<D>,
}

impl<D: Database> PublicKeyDiffStore<D> {
    /// Builds the store over the pk-diff parent prefix space.
    #[must_use]
    pub fn new(parent: &PrefixDb<D>) -> Self {
        Self {
            by_subnet: parent.join(BY_SUBNET_SUFFIX),
            by_height: parent.join(BY_HEIGHT_SUFFIX),
        }
    }

    /// Records the prior BLS key bytes for `(subnet, node)` at `height` into both
    /// indexes. `prev_public_key` empty ⇒ the node had no key before the change.
    ///
    /// # Errors
    /// Propagates the base [`Database`] write error.
    pub fn put(&self, subnet: Id, node: NodeId, height: u64, prev_public_key: &[u8]) -> Result<()> {
        self.by_subnet.put(
            &marshal_diff_key_by_subnet_id(subnet, height, node),
            prev_public_key,
        )?;
        self.by_height.put(
            &marshal_diff_key_by_height(height, subnet, node),
            prev_public_key,
        )?;
        Ok(())
    }
}

/// Un-applies one weight diff onto `node`'s entry in `set`, walking backward in
/// height (`ApplyValidatorWeightDiffs` inner loop): a decrease *adds* the amount
/// (the prior weight was higher), an increase *subtracts* it. An entry that
/// reaches weight `0` is removed.
fn apply_weight_diff(
    set: &mut BTreeMap<NodeId, DiffValidator>,
    node: NodeId,
    diff: &ValidatorWeightDiff,
) -> Result<()> {
    let entry = set.entry(node).or_default();
    let new_weight = if diff.decrease {
        entry
            .weight
            .checked_add(diff.amount)
            .ok_or(Error::Overflow)?
    } else {
        entry
            .weight
            .checked_sub(diff.amount)
            .ok_or(Error::Overflow)?
    };
    if new_weight == 0 {
        set.remove(&node);
    } else {
        entry.weight = new_weight;
    }
    Ok(())
}

/// Applies the weight diffs for a single `subnet` over the height window
/// `[to, from]` (i.e. `(target_height, current_height]`) onto `set`, walking
/// newest height first. Mirrors Go `ApplyValidatorWeightDiffs`.
///
/// `from` is typically the current (last-accepted) height and `to =
/// target_height + 1`; if `from < to` no diffs are applied.
///
/// # Errors
/// Propagates iterator, decode, and arithmetic errors.
pub fn apply_validator_weight_diffs<D: Database>(
    store: &WeightDiffStore<D>,
    set: &mut BTreeMap<NodeId, DiffValidator>,
    from: u64,
    to: u64,
    subnet: Id,
) -> Result<()> {
    let start = marshal_start_diff_key_by_subnet_id(subnet, from);
    let mut it = store
        .by_subnet
        .new_iterator_with_start_and_prefix(&start, subnet.as_bytes());
    while it.next() {
        let key = it.key().ok_or(Error::UnexpectedDiffKeyLength)?;
        let (_, parsed_height, node) = unmarshal_diff_key_by_subnet_id(key)?;
        if parsed_height < to {
            break;
        }
        let value = it.value().ok_or(Error::UnexpectedWeightValueLength)?;
        let diff = unmarshal_weight_diff(value)?;
        apply_weight_diff(set, node, &diff)?;
    }
    it.error()?;
    Ok(())
}

/// Applies the weight diffs across **all** subnets over `[to, from]` onto
/// `all`, keyed by subnet then node. Mirrors Go `ApplyAllValidatorWeightDiffs`.
///
/// # Errors
/// Propagates iterator, decode, and arithmetic errors.
pub fn apply_all_validator_weight_diffs<D: Database>(
    store: &WeightDiffStore<D>,
    all: &mut BTreeMap<Id, BTreeMap<NodeId, DiffValidator>>,
    from: u64,
    to: u64,
) -> Result<()> {
    let start = marshal_start_diff_key_by_height(from);
    let mut it = store.by_height.new_iterator_with_start(&start);
    while it.next() {
        let key = it.key().ok_or(Error::UnexpectedDiffKeyLength)?;
        let (parsed_height, subnet, node) = unmarshal_diff_key_by_height(key)?;
        if parsed_height < to {
            break;
        }
        let value = it.value().ok_or(Error::UnexpectedWeightValueLength)?;
        let diff = unmarshal_weight_diff(value)?;
        let set = all.entry(subnet).or_default();
        apply_weight_diff(set, node, &diff)?;
        if set.is_empty() {
            all.remove(&subnet);
        }
    }
    it.error()?;
    Ok(())
}

/// Restores prior BLS public keys for a single `subnet` over the height window
/// `[to, from]` onto `set`, walking newest height first. Mirrors Go
/// `ApplyValidatorPublicKeyDiffs`: a node absent from `set` is skipped; an empty
/// stored value clears the key.
///
/// # Errors
/// Propagates iterator and decode errors.
pub fn apply_validator_public_key_diffs<D: Database>(
    store: &PublicKeyDiffStore<D>,
    set: &mut BTreeMap<NodeId, DiffValidator>,
    from: u64,
    to: u64,
    subnet: Id,
) -> Result<()> {
    let start = marshal_start_diff_key_by_subnet_id(subnet, from);
    let mut it = store
        .by_subnet
        .new_iterator_with_start_and_prefix(&start, subnet.as_bytes());
    while it.next() {
        let key = it.key().ok_or(Error::UnexpectedDiffKeyLength)?;
        let (_, parsed_height, node) = unmarshal_diff_key_by_subnet_id(key)?;
        if parsed_height < to {
            break;
        }
        if let Some(entry) = set.get_mut(&node) {
            let value = it.value().unwrap_or(&[]);
            entry.public_key = if value.is_empty() {
                None
            } else {
                Some(value.to_vec())
            };
        }
    }
    it.error()?;
    Ok(())
}

/// Restores prior BLS public keys across **all** subnets over `[to, from]`.
/// Mirrors Go `ApplyAllValidatorPublicKeyDiffs`.
///
/// # Errors
/// Propagates iterator and decode errors.
pub fn apply_all_validator_public_key_diffs<D: Database>(
    store: &PublicKeyDiffStore<D>,
    all: &mut BTreeMap<Id, BTreeMap<NodeId, DiffValidator>>,
    from: u64,
    to: u64,
) -> Result<()> {
    let start = marshal_start_diff_key_by_height(from);
    let mut it = store.by_height.new_iterator_with_start(&start);
    while it.next() {
        let key = it.key().ok_or(Error::UnexpectedDiffKeyLength)?;
        let (parsed_height, subnet, node) = unmarshal_diff_key_by_height(key)?;
        if parsed_height < to {
            break;
        }
        if let Some(entry) = all.get_mut(&subnet).and_then(|s| s.get_mut(&node)) {
            let value = it.value().unwrap_or(&[]);
            entry.public_key = if value.is_empty() {
                None
            } else {
                Some(value.to_vec())
            };
        }
    }
    it.error()?;
    Ok(())
}

/// Convenience constructor building both stores over a base [`Database`] under
/// the canonical [`prefixes`](super::prefixes) layout (the same nesting
/// `State::new` uses for its handles).
#[must_use]
pub fn stores_over<D: Database>(base: Arc<D>) -> (WeightDiffStore<D>, PublicKeyDiffStore<D>) {
    use crate::state::prefixes;
    let l1_parent = PrefixDb::new_arc(prefixes::L1_VALIDATORS_PREFIX, base);
    let weight_parent = l1_parent.join(prefixes::WEIGHT_DIFF_PREFIX);
    let pk_parent = l1_parent.join(prefixes::PK_DIFF_PREFIX);
    (
        WeightDiffStore::new(&weight_parent),
        PublicKeyDiffStore::new(&pk_parent),
    )
}

#[cfg(test)]
mod prop {
    //! `diff_iter_newest_first` — forward key-order iteration walks newest
    //! height first, and backward reconstruction recovers the prior set.
    //!
    //! Oracle: Go `ApplyValidatorWeightDiffs` (08 §7.1). We populate a MemDb-
    //! backed store directly, then assert (a) the by-subnet scan yields strictly
    //! decreasing heights, and (b) un-applying the window reproduces the
    //! hand-computed prior weights.

    use ava_database::MemDb;
    use proptest::prelude::*;

    use super::*;
    use crate::state::disk_staker_diff_iterator::unmarshal_diff_key_by_subnet_id;

    fn build() -> (WeightDiffStore<MemDb>, PublicKeyDiffStore<MemDb>) {
        stores_over(Arc::new(MemDb::new()))
    }

    #[test]
    fn diff_iter_newest_first() {
        let (weight, _pk) = build();
        let subnet = Id::from([0x07; 32]);
        let node = NodeId::from([0x01; 20]);

        // Weight grew +10 at h1, +5 at h2, then dropped -3 at h3.
        weight
            .put(
                subnet,
                node,
                1,
                &ValidatorWeightDiff {
                    decrease: false,
                    amount: 10,
                },
            )
            .expect("h1");
        weight
            .put(
                subnet,
                node,
                2,
                &ValidatorWeightDiff {
                    decrease: false,
                    amount: 5,
                },
            )
            .expect("h2");
        weight
            .put(
                subnet,
                node,
                3,
                &ValidatorWeightDiff {
                    decrease: true,
                    amount: 3,
                },
            )
            .expect("h3");

        // Forward scan over the by-subnet index must visit h3, h2, h1.
        let start = marshal_start_diff_key_by_subnet_id(subnet, u64::MAX);
        let mut it = weight
            .by_subnet
            .new_iterator_with_start_and_prefix(&start, subnet.as_bytes());
        let mut heights = Vec::new();
        while it.next() {
            let (_, h, _) =
                unmarshal_diff_key_by_subnet_id(it.key().expect("key")).expect("decode");
            heights.push(h);
        }
        it.error().expect("iter");
        assert_eq!(heights, vec![3, 2, 1]);

        // Current (height-3) weight is 10 + 5 - 3 = 12.
        let mut set = BTreeMap::new();
        set.insert(
            node,
            DiffValidator {
                weight: 12,
                public_key: None,
            },
        );

        // Reconstruct height 1 by un-applying (1, 3] = heights {3, 2}:
        //   undo -3 ⇒ +3 (15), undo +5 ⇒ -5 (10). Expect weight 10.
        apply_validator_weight_diffs(&weight, &mut set, 3, 2, subnet).expect("apply");
        assert_eq!(set.get(&node).expect("node").weight, 10);

        // Reconstruct height 0 by un-applying all of [1, 3]:
        //   from 12: +3 (15), -5 (10), -10 (0) ⇒ node removed.
        let mut set0 = BTreeMap::new();
        set0.insert(
            node,
            DiffValidator {
                weight: 12,
                public_key: None,
            },
        );
        apply_validator_weight_diffs(&weight, &mut set0, 3, 1, subnet).expect("apply0");
        assert!(!set0.contains_key(&node));
    }

    #[test]
    fn public_key_diffs_restore_prior() {
        let (_weight, pk) = build();
        let subnet = Id::from([0x07; 32]);
        let node = NodeId::from([0x09; 20]);
        let prior_key = vec![0xABu8; 96];

        // At height 5 the node's key changed; the store records the prior key.
        pk.put(subnet, node, 5, &prior_key).expect("put pk");

        // Current set (height 5) has the new key.
        let mut set = BTreeMap::new();
        set.insert(
            node,
            DiffValidator {
                weight: 1,
                public_key: Some(vec![0xCD; 96]),
            },
        );

        // Reconstruct height 4 by un-applying (4, 5]: restore the prior key.
        apply_validator_public_key_diffs(&pk, &mut set, 5, 5, subnet).expect("apply pk");
        assert_eq!(set.get(&node).expect("node").public_key, Some(prior_key));
    }

    proptest! {
        /// A by-subnet scan always yields strictly decreasing heights regardless
        /// of insertion order (the `inverse_height` ordering contract).
        #[test]
        fn scan_is_newest_first(mut hs in proptest::collection::vec(1u64..1_000, 1..16)) {
            hs.sort_unstable();
            hs.dedup();
            let (weight, _pk) = build();
            let subnet = Id::from([0x07; 32]);
            let node = NodeId::from([0x01; 20]);
            for &h in &hs {
                weight
                    .put(subnet, node, h, &ValidatorWeightDiff { decrease: false, amount: 1 })
                    .expect("put");
            }
            let start = marshal_start_diff_key_by_subnet_id(subnet, u64::MAX);
            let mut it = weight
                .by_subnet
                .new_iterator_with_start_and_prefix(&start, subnet.as_bytes());
            let mut seen = Vec::new();
            while it.next() {
                let (_, h, _) =
                    unmarshal_diff_key_by_subnet_id(it.key().expect("key")).expect("decode");
                seen.push(h);
            }
            it.error().expect("iter");
            let mut expected = hs.clone();
            expected.sort_unstable_by(|a, b| b.cmp(a));
            prop_assert_eq!(seen, expected);
        }
    }
}
