// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`genesis_validator_set`] — the real genesis validator set (`NodeId` + BLS
//! public key + weight) that the proposervm windower needs to agree with Go on
//! proposer-window order for a network (specs 23 §3.2/§3.3,
//! `platformvm/genesis.go::genesis.New`).
//!
//! Weights are **not** a flat JSON field: Go derives each genesis validator's
//! stake by splitting the initially-staked allocations across the stakers
//! (`splitAllocations`) and summing the resulting unlock-schedule amounts —
//! exactly the §3.2/§3.3 computation `build.rs::from_config` performs when it
//! builds each `PermissionlessValidator.staked` (whose summed amount becomes
//! `platformvm/genesis.New`'s per-validator `weight`). This module reuses that
//! same split (`crate::split::split_allocations`) rather than re-deriving
//! weights by hand, so its output is byte-for-byte consistent with the built
//! P-Chain genesis (see the `genesis_validator_set_matches_local_genesis_stakers`
//! golden test below, which cross-checks against the actual genesis bytes).

use std::collections::HashSet;

use ava_crypto::bls::PublicKey;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;

use crate::config::Config;
use crate::error::{GenesisError, Result};
use crate::split::split_allocations;

/// One genesis validator's `NodeId` + BLS public key + weight — the
/// projection [`genesis_validator_set`] derives from a [`Config`]'s
/// `initial_stakers` (specs 23 §3.2/§3.3).
///
/// No `Debug`/`PartialEq`: [`PublicKey`] derives neither (mirrors
/// `ava_validators::validator::{Validator, GetValidatorOutput}`, which carry
/// the same BLS key type and intentionally derive only `Clone`).
#[derive(Clone)]
pub struct GenesisValidatorEntry {
    /// The validating node's id (`Staker.node_id`).
    pub node_id: NodeId,
    /// The node's BLS public key, parsed from `Staker.signer`'s
    /// `ProofOfPossession`; `None` for a legacy (pre-BLS) staker (mirrors
    /// `signer.Signer.Key()`, empty for `signer.Empty`).
    pub public_key: Option<PublicKey>,
    /// The staker's genesis weight: the sum of the unlock-schedule amounts in
    /// the bucket `split_allocations` assigns this staker (`build.rs` §3.3 —
    /// `platformvm/genesis.New`'s per-validator `weight`). Never zero for a
    /// validly-configured genesis (`platformvm/genesis.New` rejects a
    /// zero-weight validator as `errValidatorHasNoWeight`).
    pub weight: u64,
}

/// `genesis.FromConfig` §3.2/§3.3 — the genesis validator set: one
/// [`GenesisValidatorEntry`] per `config.initial_stakers`, with weights
/// derived by the SAME allocation split `build.rs::from_config` uses to build
/// each `PermissionlessValidator.staked` (`split_allocations(&skipped_allocations,
/// initial_stakers.len())`, summed per bucket) — **not** a flat weight.
///
/// # Errors
/// [`GenesisError::StakeOverflow`] if a per-staker weight sum overflows `u64`
/// (mirrors `platformvm/genesis.New`'s checked weight accumulation);
/// propagates [`GenesisError::Platform`] if a staker's `ProofOfPossession`
/// carries a malformed BLS public key.
pub fn genesis_validator_set(config: &Config) -> Result<Vec<GenesisValidatorEntry>> {
    // §3.2 — the allocations skipped from the general UTXO set because their
    // address is initially staked (identical to `build.rs::from_config`'s
    // §3.2 filter — this is `split_allocations`'s required input).
    let initially_staked: HashSet<ShortId> = config.initial_staked_funds.iter().copied().collect();
    let mut skipped_allocations = Vec::new();
    for allocation in &config.allocations {
        if initially_staked.contains(&allocation.avax_addr) {
            skipped_allocations.push(allocation.clone());
        }
    }

    // §3.3 — split those allocations across the initial stakers (the exact
    // helper `build.rs` calls; NOT re-derived here).
    let all_node_allocations =
        split_allocations(&skipped_allocations, config.initial_stakers.len());

    let mut out = Vec::with_capacity(config.initial_stakers.len());
    for (i, staker) in config.initial_stakers.iter().enumerate() {
        let node_allocations = all_node_allocations.get(i).map_or(&[][..], Vec::as_slice);

        let mut weight: u64 = 0;
        for allocation in node_allocations {
            for unlock in &allocation.unlock_schedule {
                weight = weight
                    .checked_add(unlock.amount)
                    .ok_or(GenesisError::StakeOverflow)?;
            }
        }

        // `signer.Signer.Key()` — `None` for a legacy (no-BLS) staker.
        let public_key = match &staker.signer {
            Some(pop) => Some(pop.key()?),
            None => None,
        };

        out.push(GenesisValidatorEntry {
            node_id: staker.node_id,
            public_key,
            weight,
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use ava_platformvm::txs::UnsignedTx as PUnsignedTx;

    use crate::build::from_config;
    use crate::config::UNMODIFIED_LOCAL_CONFIG;

    use super::*;

    /// The local genesis has exactly 5 initial stakers, each with a BLS key
    /// and a nonzero weight, whose `NodeId`s match `initial_stakers` and whose
    /// weight matches the byte-level ground truth: the `Validator.wght` the
    /// built P-Chain genesis bytes actually carry for that node id
    /// (`platformvm/genesis.New`, via `build.rs::from_config`). This proves
    /// [`genesis_validator_set`] is not re-deriving/guessing weights — it
    /// reproduces exactly what lands in the genesis bytes.
    #[test]
    fn genesis_validator_set_matches_local_genesis_stakers() {
        let cfg = &*UNMODIFIED_LOCAL_CONFIG;
        let set = genesis_validator_set(cfg).expect("genesis validator set");

        // The local genesis has exactly 5 initial stakers.
        assert_eq!(set.len(), 5, "local genesis stakes 5 validators");

        // Every entry carries the staker's NodeID and a BLS public key.
        for e in &set {
            assert!(
                e.public_key.is_some(),
                "genesis staker {} has a BLS key",
                e.node_id
            );
            assert!(
                e.weight > 0,
                "genesis staker {} has nonzero weight",
                e.node_id
            );
        }

        // The NodeIDs equal the genesis initial_stakers' NodeIDs (order-independent).
        let got: BTreeSet<_> = set.iter().map(|e| e.node_id).collect();
        let want: BTreeSet<_> = cfg.initial_stakers.iter().map(|s| s.node_id).collect();
        assert_eq!(got, want, "node ids match genesis stakers");

        // Weight parity: cross-check each entry's weight against the actual
        // `Validator.wght` the built P-Chain genesis bytes carry for that node
        // id — the split_allocations-derived stake, NOT a flat 1.
        let (p_bytes, _asset_id) = from_config(cfg).expect("from_config");
        let genesis = ava_platformvm::genesis::parse(&p_bytes).expect("parse p-chain genesis");
        let mut want_weights: BTreeMap<NodeId, u64> = BTreeMap::new();
        for tx in &genesis.validators {
            match &tx.unsigned {
                PUnsignedTx::AddValidator(v) => {
                    want_weights.insert(v.validator.node_id, v.validator.wght);
                }
                PUnsignedTx::AddPermissionlessValidator(v) => {
                    want_weights.insert(v.validator.node_id, v.validator.wght);
                }
                _ => {}
            }
        }
        for e in &set {
            assert_eq!(
                e.weight,
                *want_weights
                    .get(&e.node_id)
                    .unwrap_or_else(|| panic!("no genesis validator tx for {}", e.node_id)),
                "weight parity for {} (not a flat 1)",
                e.node_id
            );
            assert_ne!(
                e.weight, 1,
                "genesis weight must not be the flat placeholder"
            );
        }
    }

    /// A staker with no `signer` (legacy, pre-BLS) yields `public_key: None`;
    /// weight is still derived from the split (specs 23 §3.3 — the BLS key is
    /// orthogonal to the stake split). Mainnet predates BLS signers.
    #[test]
    fn genesis_validator_set_mainnet_has_no_bls_keys() {
        let set = genesis_validator_set(&crate::config::MAINNET_CONFIG).expect("mainnet set");
        assert!(!set.is_empty(), "mainnet stakes at least one validator");
        assert!(
            set.iter().all(|e| e.public_key.is_none()),
            "mainnet initial stakers predate BLS signers"
        );
        assert!(
            set.iter().all(|e| e.weight > 0),
            "every mainnet genesis staker has nonzero weight"
        );
    }
}
