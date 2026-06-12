// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`ValidatorState`] trait and its associated output types.
//!
//! Port of `snow/validators/state.go`. The determinism-binding signature is
//! [`ValidatorState::get_validator_set`], which returns a
//! `BTreeMap<NodeId, GetValidatorOutput>` — iterating it is `NodeId`-ascending, the
//! order the proposervm windower samples over (`specs/06-consensus.md` §6.1).

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use ava_crypto::bls::PublicKey;

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::Result;
use crate::validator::GetValidatorOutput;

/// The richer per-validator projection returned by `get_current_validator_set`
/// (Go `validators.GetCurrentValidatorOutput`).
#[derive(Clone)]
pub struct GetCurrentValidatorOutput {
    /// The validation id (the staking tx / L1 validation id).
    pub validation_id: Id,
    /// The validating node's id.
    pub node_id: NodeId,
    /// The node's BLS public key, or `None`.
    pub public_key: Option<PublicKey>,
    /// The validator's weight.
    pub weight: u64,
    /// The block height at which the validator started validating.
    pub start_time: u64,
    /// Minimum nonce of the validator's balance-update messages (ACP-77).
    pub min_nonce: u64,
    /// Whether the validator is currently active (positive balance).
    pub is_active: bool,
    /// Whether this is an L1 (subnet-only) validator.
    pub is_l1_validator: bool,
}

/// A warp-signing validator set for a single subnet at a height
/// (Go `validators.WarpSet`).
#[derive(Clone, Default)]
pub struct WarpSet {
    /// Validators eligible to sign, in `NodeId`-canonical order.
    pub validators: Vec<GetValidatorOutput>,
    /// The set's total weight.
    pub total_weight: u64,
}

/// The P-Chain-backed validator state queried by consensus/warp/uptime
/// (Go `validators.State`).
///
/// The `get_validator_set` return type is binding: it is a `BTreeMap` so that
/// iteration is canonically `NodeId`-ascending (the windower determinism contract).
#[async_trait]
pub trait ValidatorState: Send + Sync {
    /// The minimum P-Chain height that can be queried.
    async fn get_minimum_height(&self) -> Result<u64>;

    /// The current P-Chain height.
    async fn get_current_height(&self) -> Result<u64>;

    /// The subnet a chain belongs to.
    async fn get_subnet_id(&self, chain: Id) -> Result<Id>;

    /// The validator set of `subnet` at `height`, keyed by `NodeId` (ascending).
    async fn get_validator_set(
        &self,
        height: u64,
        subnet: Id,
    ) -> Result<BTreeMap<NodeId, GetValidatorOutput>>;

    /// The current validator set of `subnet` keyed by validation id, plus the
    /// height it was read at.
    async fn get_current_validator_set(
        &self,
        subnet: Id,
    ) -> Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)>;

    /// The warp-signing validator sets at `height`, keyed by subnet id.
    async fn get_warp_validator_sets(&self, height: u64) -> Result<HashMap<Id, WarpSet>>;
}

/// Delegating impl so a shared handle (`Arc<S>`) is itself a
/// [`ValidatorState`] — Go passes `validators.State` by interface reference;
/// the proposervm API service (M8.22) holds such a shared handle alongside the
/// windower.
#[async_trait]
impl<T: ValidatorState + ?Sized> ValidatorState for std::sync::Arc<T> {
    async fn get_minimum_height(&self) -> Result<u64> {
        (**self).get_minimum_height().await
    }

    async fn get_current_height(&self) -> Result<u64> {
        (**self).get_current_height().await
    }

    async fn get_subnet_id(&self, chain: Id) -> Result<Id> {
        (**self).get_subnet_id(chain).await
    }

    async fn get_validator_set(
        &self,
        height: u64,
        subnet: Id,
    ) -> Result<BTreeMap<NodeId, GetValidatorOutput>> {
        (**self).get_validator_set(height, subnet).await
    }

    async fn get_current_validator_set(
        &self,
        subnet: Id,
    ) -> Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        (**self).get_current_validator_set(subnet).await
    }

    async fn get_warp_validator_sets(&self, height: u64) -> Result<HashMap<Id, WarpSet>> {
        (**self).get_warp_validator_sets(height).await
    }
}
