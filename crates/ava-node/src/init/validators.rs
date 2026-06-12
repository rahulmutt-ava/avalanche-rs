// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 14 (specs/12 §2.2): the validators manager, wrapped in the
//! overridden manager when sybil protection is disabled (port of Go
//! `node/overridden_manager.go`).

use std::collections::HashSet;
use std::sync::Arc;

use ava_crypto::bls::PublicKey;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::validator::Validator;
use ava_validators::{DefaultManager, ManagerCallbackListener, ValidatorManager};

/// A [`ValidatorManager`] that pins every subnet-scoped call to one subnet
/// (Go `overriddenManager`): with sybil protection off, every subnet shares
/// the primary network's connection-derived validator set.
pub struct OverriddenManager {
    subnet_id: Id,
    inner: Arc<dyn ValidatorManager>,
}

impl OverriddenManager {
    /// Wrap `inner`, overriding every subnet argument with `subnet_id`.
    #[must_use]
    pub fn new(subnet_id: Id, inner: Arc<dyn ValidatorManager>) -> Self {
        Self { subnet_id, inner }
    }
}

impl ValidatorManager for OverriddenManager {
    fn add_staker(
        &self,
        _subnet: Id,
        node: NodeId,
        pk: Option<PublicKey>,
        tx: Id,
        weight: u64,
    ) -> ava_validators::Result<()> {
        self.inner.add_staker(self.subnet_id, node, pk, tx, weight)
    }

    fn add_weight(&self, _subnet: Id, node: NodeId, weight: u64) -> ava_validators::Result<()> {
        self.inner.add_weight(self.subnet_id, node, weight)
    }

    fn remove_weight(&self, _subnet: Id, node: NodeId, weight: u64) -> ava_validators::Result<()> {
        self.inner.remove_weight(self.subnet_id, node, weight)
    }

    fn get_weight(&self, _subnet: Id, node: NodeId) -> u64 {
        self.inner.get_weight(self.subnet_id, node)
    }

    fn get_validator(&self, _subnet: Id, node: NodeId) -> Option<Validator> {
        self.inner.get_validator(self.subnet_id, node)
    }

    fn get_validator_ids(&self, _subnet: Id) -> Vec<NodeId> {
        self.inner.get_validator_ids(self.subnet_id)
    }

    fn subset_weight(&self, _subnet: Id, ids: &HashSet<NodeId>) -> ava_validators::Result<u64> {
        self.inner.subset_weight(self.subnet_id, ids)
    }

    fn total_weight(&self, _subnet: Id) -> ava_validators::Result<u64> {
        self.inner.total_weight(self.subnet_id)
    }

    fn num_validators(&self, _subnet: Id) -> usize {
        self.inner.num_validators(self.subnet_id)
    }

    fn num_subnets(&self) -> usize {
        self.inner.num_subnets()
    }

    fn sample(&self, _subnet: Id, size: usize) -> ava_validators::Result<Vec<NodeId>> {
        self.inner.sample(self.subnet_id, size)
    }

    fn register_callback_listener(&self, _subnet: Id, l: Arc<dyn ManagerCallbackListener>) {
        self.inner.register_callback_listener(self.subnet_id, l);
    }
}

/// Step 14: `validators.NewManager()`, wrapped in [`OverriddenManager`] over
/// the primary network when sybil protection is off (Go warns
/// `"sybil control is not enforced"`).
#[must_use]
pub fn new_validators(
    sybil_protection_enabled: bool,
    primary_network_id: Id,
) -> Arc<dyn ValidatorManager> {
    let base: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
    if sybil_protection_enabled {
        return base;
    }
    tracing::warn!("sybil control is not enforced");
    Arc::new(OverriddenManager::new(primary_network_id, base))
}
