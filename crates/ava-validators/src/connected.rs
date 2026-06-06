// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`ConnectedValidators`] connectivity tracker + the [`Connector`] trait.
//!
//! Port of `snow/validators/connector.go` and the connected-stake bookkeeping in
//! the networking layer. `ConnectedValidators` tracks which validators are
//! currently connected and their connected weight, supplying:
//! - the "sample one connected validator" gossip path, and
//! - the `min_percent_connected` health input — the fraction of total subnet stake
//!   that is currently connected (`specs/06-consensus.md` §6.2).
//!
//! The connectivity ratio is the one place a float is allowed in this crate (it is
//! a health-reporting heuristic, never a consensus decision — see `specs/06` §2).

use std::collections::BTreeMap;

use async_trait::async_trait;
use ava_crypto::bls::PublicKey;
use ava_utils::math;
use ava_utils::rng::Source;

use ava_types::node_id::NodeId;

use crate::error::Result;

/// Peer up/down notifications (Go `validators.Connector`). Pre-authenticated node
/// ids arrive from the network layer.
#[async_trait]
pub trait Connector: Send + Sync {
    /// A peer connected.
    async fn connected(&self, node: NodeId) -> Result<()>;
    /// A peer disconnected.
    async fn disconnected(&self, node: NodeId) -> Result<()>;
}

/// Tracks currently-connected validators and their connected weight for one
/// subnet (Go `validators.connectedValidators`).
///
/// Validators are keyed by `NodeId` in a `BTreeMap` so the connected snapshot the
/// "sample one connected validator" path reads is canonically ordered.
#[derive(Default)]
pub struct ConnectedValidators {
    /// Total weight of all validators in the subnet (connected or not).
    total_weight: u64,
    /// Per-validator weight, keyed by `NodeId` (ascending).
    weights: BTreeMap<NodeId, u64>,
    /// BLS keys of validators, for completeness on the connected path.
    public_keys: BTreeMap<NodeId, Option<PublicKey>>,
    /// Currently-connected validators and their weight at connect time.
    connected: BTreeMap<NodeId, u64>,
    /// Sum of `connected` values (maintained incrementally).
    connected_weight: u64,
}

impl ConnectedValidators {
    /// Creates an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers (or re-registers) a validator and its weight. Does not change
    /// connectivity; call [`ConnectedValidators::connect`] separately.
    ///
    /// # Errors
    /// [`crate::Error::WeightOverflow`] if the total subnet weight overflows.
    pub fn add_validator(
        &mut self,
        node: NodeId,
        public_key: Option<PublicKey>,
        weight: u64,
    ) -> Result<()> {
        // Remove any prior weight contribution before re-adding.
        if let Some(prev) = self.weights.insert(node, weight) {
            self.total_weight = math::sub(self.total_weight, prev)?;
            if let Some(c) = self.connected.get_mut(&node) {
                self.connected_weight = math::sub(self.connected_weight, *c)?;
                *c = weight;
                self.connected_weight = math::add(self.connected_weight, weight)?;
            }
        }
        self.total_weight = math::add(self.total_weight, weight)?;
        self.public_keys.insert(node, public_key);
        Ok(())
    }

    /// Marks `node` connected, crediting its weight to the connected total.
    /// A non-validator (unknown node) is recorded with zero weight.
    ///
    /// # Errors
    /// [`crate::Error::WeightOverflow`] if the connected weight overflows.
    pub fn connect(&mut self, node: NodeId) -> Result<()> {
        let weight = self.weights.get(&node).copied().unwrap_or(0);
        if self.connected.insert(node, weight).is_none() {
            self.connected_weight = math::add(self.connected_weight, weight)?;
        }
        Ok(())
    }

    /// Marks `node` disconnected, debiting its connected weight.
    ///
    /// # Errors
    /// [`crate::Error::WeightUnderflow`] never (debit is bounded by the credit),
    /// but propagated for API symmetry with [`ava_utils::math::sub`].
    pub fn disconnect(&mut self, node: NodeId) -> Result<()> {
        if let Some(weight) = self.connected.remove(&node) {
            self.connected_weight = math::sub(self.connected_weight, weight)?;
        }
        Ok(())
    }

    /// Total subnet weight.
    #[must_use]
    pub fn total_weight(&self) -> u64 {
        self.total_weight
    }

    /// Weight of currently-connected validators.
    #[must_use]
    pub fn connected_weight(&self) -> u64 {
        self.connected_weight
    }

    /// Number of currently-connected validators.
    #[must_use]
    pub fn num_connected(&self) -> usize {
        self.connected.len()
    }

    /// Fraction of subnet stake currently connected, in `[0.0, 1.0]`. Returns
    /// `1.0` when the subnet has no weight (Go treats an empty subnet as fully
    /// connected). Used only for the `min_percent_connected` health check.
    #[must_use]
    pub fn percent_connected(&self) -> f64 {
        if self.total_weight == 0 {
            return 1.0;
        }
        // Health heuristic only — float is permitted here (`specs/06` §2).
        (self.connected_weight as f64) / (self.total_weight as f64)
    }

    /// Samples one currently-connected validator using `source`, weighted by
    /// connected stake. Returns `None` if no validator is connected.
    ///
    /// Uses the M0 deterministic weighted-without-replacement sampler over the
    /// `NodeId`-sorted connected snapshot, so the draw is reproducible for a fixed
    /// `(connected set, source)`.
    #[must_use]
    pub fn sample_one(&self, source: Box<dyn Source>) -> Option<NodeId> {
        use ava_utils::sampler::weighted_without_replacement::{
            WeightedWithoutReplacement, WeightedWithoutReplacementGeneric,
        };

        let connected: Vec<(NodeId, u64)> = self
            .connected
            .iter()
            .filter(|(_, w)| **w > 0)
            .map(|(n, w)| (*n, *w))
            .collect();
        if connected.is_empty() {
            return None;
        }
        let weights: Vec<u64> = connected.iter().map(|(_, w)| *w).collect();

        // The generic sampler owns its source, so move `source` in directly.
        let mut sampler = WeightedWithoutReplacementGeneric::new(source);
        sampler.initialize(&weights).ok()?;
        let idx = sampler.sample(1)?;
        idx.first().map(|i| connected[*i].0)
    }
}
