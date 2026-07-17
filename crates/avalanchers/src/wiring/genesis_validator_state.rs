// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`GenesisValidatorState`] — the network-path proposervm windower's
//! [`ValidatorState`], backed by a network's real genesis validator set
//! (`NodeId` + BLS key + weight, [`ava_genesis::genesis_validator_set`])
//! instead of a synthetic self-plus-beacons set at a flat weight of 1.
//!
//! For the Rust node's proposer windows to agree with the Go network (specs
//! 06 §6.1 — the windower samples deterministically over the height's
//! validator set), the windower must see the SAME validator set Go derives
//! from the same genesis. `chains::FixedState` (self + explicit bootstrap
//! beacons at weight 1) is right for the in-process loopback boot (no real
//! genesis stakers exist there); [`GenesisValidatorState`] is the network-path
//! replacement — same trait-impl shape (height-invariant, `chains.rs:145-181`
//! mirrored below) but backed by the real genesis set.
//!
//! Dep-graph note: `ava_validators::validator::GetValidatorOutput::public_key`
//! is `Option<ava_crypto::bls::PublicKey>` — `ava_crypto` (not `ava_validators`
//! itself) owns the `PublicKey` type. `ava-genesis` already depends on
//! `ava-crypto` (for address/hashing), so
//! [`ava_genesis::genesis_validator_set`] returns already-parsed
//! `ava_crypto::bls::PublicKey`s directly; no raw-byte hand-off + adapter-side
//! parsing is needed, and no new `ava-genesis` → `ava-validators` dependency
//! edge is introduced.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;

use ava_genesis::GenesisError;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;

/// A height-invariant [`ValidatorState`] backed by a network's real genesis
/// validator set — the windower state the network-path chain boot
/// (`chains::boot_chain_with_sender`'s `OutboundSender` path) uses so
/// proposer-window order agrees with Go.
#[derive(Clone)]
pub struct GenesisValidatorState {
    set: BTreeMap<NodeId, GetValidatorOutput>,
}

impl GenesisValidatorState {
    /// Builds the state from a genesis [`ava_genesis::Config`]'s
    /// `initial_stakers` (`ava_genesis::genesis_validator_set`).
    ///
    /// # Errors
    /// Propagates [`ava_genesis::genesis_validator_set`]'s error (a malformed
    /// BLS public key or a per-staker weight-sum overflow).
    pub fn new(config: &ava_genesis::Config) -> Result<Self, GenesisError> {
        let entries = ava_genesis::genesis_validator_set(config)?;
        let mut set = BTreeMap::new();
        for entry in entries {
            set.insert(
                entry.node_id,
                GetValidatorOutput {
                    node_id: entry.node_id,
                    public_key: entry.public_key,
                    weight: entry.weight,
                },
            );
        }
        Ok(Self { set })
    }

    /// Builds the state for `network_id`'s genesis config
    /// (`ava_genesis::get_config`) — the constructor the network-path chain
    /// boot calls (Mainnet/Fuji/Local resolve to their embedded config; other
    /// ids fall back to the Local template, per `ava_genesis::get_config`).
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn from_network(network_id: u32) -> Result<Self, GenesisError> {
        Self::new(&ava_genesis::get_config(network_id))
    }
}

#[async_trait]
impl ValidatorState for GenesisValidatorState {
    async fn get_minimum_height(&self) -> ava_validators::Result<u64> {
        Ok(0)
    }

    async fn get_current_height(&self) -> ava_validators::Result<u64> {
        Ok(1)
    }

    async fn get_subnet_id(&self, _chain: Id) -> ava_validators::Result<Id> {
        Ok(Id::EMPTY)
    }

    async fn get_validator_set(
        &self,
        _height: u64,
        _subnet: Id,
    ) -> ava_validators::Result<BTreeMap<NodeId, GetValidatorOutput>> {
        Ok(self.set.clone())
    }

    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> ava_validators::Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        Ok((BTreeMap::new(), 1))
    }

    async fn get_warp_validator_sets(
        &self,
        _height: u64,
    ) -> ava_validators::Result<HashMap<Id, WarpSet>> {
        Ok(HashMap::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Local genesis has exactly 5 initial stakers; the windower state
    /// exposes all 5 at every height (a local net returns the same set at
    /// every height — there is no historical validator-set diffing here, only
    /// the fixed genesis snapshot), and reports `current_height == 1`,
    /// `minimum_height == 0` (mirroring `chains::FixedState`).
    #[tokio::test]
    async fn genesis_validator_state_is_height_invariant() {
        let state = GenesisValidatorState::from_network(ava_types::constants::LOCAL_ID)
            .expect("genesis validator state for Local");

        assert_eq!(state.get_current_height().await.expect("current height"), 1);
        assert_eq!(state.get_minimum_height().await.expect("minimum height"), 0);

        for height in [0u64, 999u64] {
            let set = state
                .get_validator_set(height, Id::EMPTY)
                .await
                .expect("validator set");
            assert_eq!(
                set.len(),
                5,
                "local genesis has 5 validators at height {height}"
            );
            for (node_id, output) in &set {
                assert_eq!(*node_id, output.node_id);
                assert!(output.public_key.is_some(), "local stakers carry a BLS key");
                assert!(output.weight > 0, "genesis weight must not be flat/zero");
            }
        }

        // Height-invariance: the two heights above return the identical set.
        let at_0 = state
            .get_validator_set(0, Id::EMPTY)
            .await
            .expect("set at height 0");
        let at_999 = state
            .get_validator_set(999, Id::EMPTY)
            .await
            .expect("set at height 999");
        let ids_0: std::collections::BTreeSet<_> = at_0.keys().copied().collect();
        let ids_999: std::collections::BTreeSet<_> = at_999.keys().copied().collect();
        assert_eq!(ids_0, ids_999, "the same 5-node set at every height");

        assert_eq!(
            state.get_subnet_id(Id::EMPTY).await.expect("subnet id"),
            Id::EMPTY
        );
        let (current, height) = state
            .get_current_validator_set(Id::EMPTY)
            .await
            .expect("current validator set");
        assert!(current.is_empty());
        assert_eq!(height, 1);
        let warp = state
            .get_warp_validator_sets(0)
            .await
            .expect("warp validator sets");
        assert!(warp.is_empty());
    }
}
