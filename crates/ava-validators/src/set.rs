// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! A single subnet's validator [`Set`] (port of `snow/validators/set.go`).
//!
//! The set stores validators keyed by [`NodeId`] in a [`BTreeMap`], so the
//! iteration order is canonically `NodeId`-ascending — this is the determinism
//! binding the sampler and the windower rely on (`specs/06-consensus.md` §6.1,
//! §6.2). Sampling builds the weight array from that sorted order and feeds the
//! M0 deterministic weighted-without-replacement sampler so the index sequence is
//! bit-for-bit identical to Go's `sampler.NewDeterministicWeightedWithoutReplacement`.

use std::collections::{BTreeMap, HashSet};

use ava_utils::math;
use ava_utils::rng::Source;
use ava_utils::sampler::weighted_without_replacement::{
    WeightedWithoutReplacement, WeightedWithoutReplacementGeneric,
};

use ava_types::node_id::NodeId;

use crate::error::{Error, Result};
use crate::validator::Validator;

/// A subnet's validators, keyed by [`NodeId`] (Go `validators.vdrSet`).
#[derive(Default)]
pub struct Set {
    validators: BTreeMap<NodeId, Validator>,
}

impl Set {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a brand-new staker.
    ///
    /// # Errors
    /// - [`Error::ZeroWeight`] if `weight == 0`.
    /// - [`Error::DuplicateValidator`] if `node_id` already exists.
    /// - [`Error::WeightOverflow`] if it would overflow the running total (the
    ///   total is recomputed lazily, so this guards the per-validator weight only;
    ///   the aggregate is checked in [`Set::total_weight`]).
    pub fn add_staker(&mut self, v: Validator) -> Result<()> {
        if v.weight == 0 {
            return Err(Error::ZeroWeight);
        }
        if self.validators.contains_key(&v.node_id) {
            return Err(Error::DuplicateValidator {
                node_id: v.node_id.hex(),
            });
        }
        self.validators.insert(v.node_id, v);
        Ok(())
    }

    /// Adds `weight` to an existing validator, or inserts a weight-only validator
    /// (no BLS key / tx id) if absent — mirroring Go `Set.AddWeight`.
    ///
    /// # Errors
    /// [`Error::ZeroWeight`] if `weight == 0`; [`Error::WeightOverflow`] if the
    /// validator's own weight would overflow `u64`.
    pub fn add_weight(&mut self, node_id: NodeId, weight: u64) -> Result<()> {
        if weight == 0 {
            return Err(Error::ZeroWeight);
        }
        match self.validators.get_mut(&node_id) {
            Some(v) => {
                v.weight = math::add(v.weight, weight)?;
            }
            None => {
                self.validators.insert(
                    node_id,
                    Validator {
                        node_id,
                        public_key: None,
                        tx_id: ava_types::id::Id::default(),
                        weight,
                    },
                );
            }
        }
        Ok(())
    }

    /// Removes `weight` from an existing validator; drops the validator when its
    /// weight reaches zero (Go `Set.RemoveWeight`).
    ///
    /// # Errors
    /// [`Error::WeightUnderflow`] if the validator is absent or holds less than
    /// `weight`.
    pub fn remove_weight(&mut self, node_id: NodeId, weight: u64) -> Result<()> {
        if weight == 0 {
            return Ok(());
        }
        let present = self.validators.get(&node_id).map_or(0, |v| v.weight);
        if present < weight {
            return Err(Error::WeightUnderflow {
                requested: weight,
                present,
            });
        }
        if present == weight {
            self.validators.remove(&node_id);
        } else if let Some(v) = self.validators.get_mut(&node_id) {
            // Subtraction cannot underflow: `present > weight` here.
            v.weight = math::sub(v.weight, weight)?;
        }
        Ok(())
    }

    /// Returns a validator's weight, or `0` if absent (Go `GetWeight`).
    #[must_use]
    pub fn get_weight(&self, node_id: NodeId) -> u64 {
        self.validators.get(&node_id).map_or(0, |v| v.weight)
    }

    /// Returns a clone of the validator record, if present (Go `GetValidator`).
    #[must_use]
    pub fn get_validator(&self, node_id: NodeId) -> Option<Validator> {
        self.validators.get(&node_id).cloned()
    }

    /// Returns the validator node ids in canonical (`NodeId`-ascending) order
    /// (Go `GetValidatorIDs`).
    #[must_use]
    pub fn get_validator_ids(&self) -> Vec<NodeId> {
        self.validators.keys().copied().collect()
    }

    /// Sums the weights of the supplied subset (Go `SubsetWeight`).
    ///
    /// # Errors
    /// [`Error::WeightOverflow`] if the partial sum overflows `u64`.
    pub fn subset_weight(&self, ids: &HashSet<NodeId>) -> Result<u64> {
        let mut total: u64 = 0;
        // Iterate the sorted set (not the HashSet) so the order is deterministic.
        for (node_id, v) in &self.validators {
            if ids.contains(node_id) {
                total = math::add(total, v.weight)?;
            }
        }
        Ok(total)
    }

    /// Sums all validator weights (Go `TotalWeight`).
    ///
    /// # Errors
    /// [`Error::WeightOverflow`] if the total overflows `u64`.
    pub fn total_weight(&self) -> Result<u64> {
        let mut total: u64 = 0;
        for v in self.validators.values() {
            total = math::add(total, v.weight)?;
        }
        Ok(total)
    }

    /// Number of validators in the set (Go `Len`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.validators.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.validators.is_empty()
    }

    /// Returns the `NodeId`-sorted `(node_id, weight)` snapshot used to build the
    /// sampler weight array. This is the determinism boundary: the slice order is
    /// always `NodeId`-ascending.
    #[must_use]
    pub fn sorted_weights(&self) -> Vec<(NodeId, u64)> {
        self.validators
            .iter()
            .map(|(id, v)| (*id, v.weight))
            .collect()
    }

    /// Deterministic weighted-without-replacement sampling over the
    /// `NodeId`-sorted weight slice, using the supplied RNG `source`.
    ///
    /// The returned ids preserve the sampler's draw order. Reusing the M0
    /// [`WeightedWithoutReplacementGeneric`] guarantees the index sequence matches
    /// Go bit-for-bit for the same `(weights, source)` (`specs/06` §6.2). For
    /// non-deterministic poll sampling the caller passes an OS-seeded source; for
    /// the windower it passes a seeded gonum MT.
    ///
    /// The sampler draws WEIGHT UNITS without replacement (Go
    /// `snow/validators/set.go` `sample` → `sampler.WeightedWithoutReplacement`),
    /// so `size` may exceed the validator COUNT: a validator whose weight spans
    /// multiple drawn positions appears multiple times in the returned `Vec`.
    /// The failure boundary is TOTAL WEIGHT, not count — requesting more than
    /// the summed weight yields [`Error::InsufficientValidators`] (Go's
    /// `errInsufficientWeight`). Deliberately NO `size > len()` guard: that
    /// cap is a Go-parity bug (it broke `k`-sampling on a network with fewer
    /// heavy validators than `k`).
    ///
    /// # Errors
    /// - [`Error::MissingValidators`] if the set is empty.
    /// - [`Error::InsufficientValidators`] if `size` exceeds the TOTAL WEIGHT.
    /// - [`Error::WeightOverflow`] if the weights sum overflows `u64`.
    pub fn sample(&self, size: usize, source: Box<dyn Source>) -> Result<Vec<NodeId>> {
        if self.validators.is_empty() {
            return Err(Error::MissingValidators);
        }
        let sorted = self.sorted_weights();
        let weights: Vec<u64> = sorted.iter().map(|(_, w)| *w).collect();

        let mut sampler = WeightedWithoutReplacementGeneric::new(source);
        sampler.initialize(&weights)?;
        let indices = sampler
            .sample(size)
            .ok_or(Error::InsufficientValidators { requested: size })?;

        Ok(indices.into_iter().map(|i| sorted[i].0).collect())
    }
}
