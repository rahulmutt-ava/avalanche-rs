// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::windower_schedule` — the gonum-MT compatibility gate (CONFIRMS R1).
//!
//! For a fixed validator set (NodeId-sorted, empty-NodeID dropped) and many
//! `(block_height, slot, p_chain_height)` tuples, assert that the Rust windower
//! reproduces the captured Go `NodeId` orderings **bit-exactly**:
//!
//! - `ExpectedProposer` (post-Durango, `MT19937_64`, seed
//!   `chain_source ^ height ^ reverse_bits64(slot)`).
//! - `Proposers` / `Delay` (pre-Durango, 32-bit `MT19937`, seed
//!   `chain_source ^ height`).
//!
//! Vectors: `tests/vectors/proposervm/windower/windower.json`, produced by a
//! scratch Go program against `vms/proposervm/proposer`. See `tests/PORTING.md`.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::time::Duration;

use ava_proposervm::proposer::windower::{
    ValidatorData, Windower, chain_source, delay_for, expected_proposer_from, proposers_from,
};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Vectors {
    chain_id: String,
    chain_source: u64,
    validators: Vec<Val>,
    expected: Vec<ExpCase>,
    proposers: Vec<ProCase>,
    delays: Vec<DelayCase>,
}

#[derive(Debug, Deserialize)]
struct Val {
    node_id: String,
    weight: u64,
}

#[derive(Debug, Deserialize)]
struct ExpCase {
    block_height: u64,
    #[allow(dead_code)]
    p_chain_height: u64,
    slot: u64,
    node_id: String,
}

#[derive(Debug, Deserialize)]
struct ProCase {
    block_height: u64,
    #[allow(dead_code)]
    p_chain_height: u64,
    max_windows: u64,
    node_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DelayCase {
    block_height: u64,
    #[allow(dead_code)]
    p_chain_height: u64,
    #[allow(dead_code)]
    max_windows: u64,
    node_id: String,
    delay_nanos: i64,
}

fn load() -> Vectors {
    let raw = include_str!("vectors/proposervm/windower/windower.json");
    serde_json::from_str(raw).expect("parse windower.json")
}

/// Build the NodeId-sorted [`ValidatorData`] slice (the empty NodeID is not in
/// the vector's `validators` list — Go drops it before capture).
fn validator_data(v: &Vectors) -> Vec<ValidatorData> {
    let mut out: Vec<ValidatorData> = v
        .validators
        .iter()
        .map(|val| ValidatorData {
            id: NodeId::from_str(&val.node_id).expect("parse node id"),
            weight: val.weight,
        })
        .collect();
    out.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    out
}

#[test]
fn chain_source_matches_go() {
    let v = load();
    let chain = ava_types::id::Id::from_str(&v.chain_id).expect("parse chain id");
    assert_eq!(chain_source(chain), v.chain_source, "chain_source");
}

#[test]
fn expected_proposer_matches_go() {
    let v = load();
    let validators = validator_data(&v);
    let cs = v.chain_source;
    for c in &v.expected {
        let got = expected_proposer_from(cs, &validators, c.block_height, c.slot)
            .expect("expected_proposer");
        assert_eq!(
            got.to_string(),
            c.node_id,
            "ExpectedProposer(height={}, slot={})",
            c.block_height,
            c.slot
        );
    }
}

#[test]
fn proposers_and_delay_match_go() {
    let v = load();
    let validators = validator_data(&v);
    let cs = v.chain_source;
    for c in &v.proposers {
        let got =
            proposers_from(cs, &validators, c.block_height, c.max_windows).expect("proposers");
        let got_strs: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(
            got_strs, c.node_ids,
            "Proposers(height={}, max_windows={})",
            c.block_height, c.max_windows
        );
    }

    for c in &v.delays {
        // Reconstruct the pre-Durango delay from the (deterministic) proposer
        // list, matching Go `Delay`. The empty NodeID delay is the cap.
        let node = NodeId::from_str(&c.node_id).expect("parse node id");
        let delay = if node == NodeId::EMPTY {
            Duration::from_secs(5) * u32::try_from(c.max_windows).unwrap()
        } else {
            let proposers =
                proposers_from(cs, &validators, c.block_height, c.max_windows).expect("proposers");
            delay_for(&proposers, node)
        };
        assert_eq!(
            i64::try_from(delay.as_nanos()).unwrap(),
            c.delay_nanos,
            "Delay(height={}, node={})",
            c.block_height,
            c.node_id
        );
    }
}

/// A fixed [`ValidatorState`] returning the captured set at every height/subnet,
/// **plus** an empty-NodeID validator that the windower must drop.
struct FixedState {
    set: BTreeMap<NodeId, GetValidatorOutput>,
}

#[async_trait::async_trait]
impl ValidatorState for FixedState {
    async fn get_minimum_height(&self) -> ava_validators::Result<u64> {
        Ok(0)
    }
    async fn get_current_height(&self) -> ava_validators::Result<u64> {
        Ok(0)
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
        Ok((BTreeMap::new(), 0))
    }
    async fn get_warp_validator_sets(
        &self,
        _height: u64,
    ) -> ava_validators::Result<HashMap<Id, WarpSet>> {
        Ok(HashMap::new())
    }
}

/// The async `Windower` path (including the empty-NodeID drop) reproduces the
/// same Go orderings as the pure-sync cores.
#[tokio::test]
async fn async_windower_matches_go() {
    let v = load();
    let chain = Id::from_str(&v.chain_id).expect("chain id");

    let mut set = BTreeMap::new();
    for val in &v.validators {
        let id = NodeId::from_str(&val.node_id).expect("node id");
        set.insert(
            id,
            GetValidatorOutput {
                node_id: id,
                public_key: None,
                weight: val.weight,
            },
        );
    }
    // Inject an empty-NodeID validator with a large weight; it MUST be dropped,
    // otherwise the orderings would diverge from Go.
    set.insert(
        NodeId::EMPTY,
        GetValidatorOutput {
            node_id: NodeId::EMPTY,
            public_key: None,
            weight: 999,
        },
    );

    let windower = Windower::new(FixedState { set }, Id::EMPTY, chain);

    for c in &v.expected {
        let got = windower
            .expected_proposer(c.block_height, 1, c.slot)
            .await
            .expect("expected_proposer");
        assert_eq!(got.to_string(), c.node_id, "async ExpectedProposer");
    }

    for c in &v.proposers {
        let got = windower
            .proposers(c.block_height, 1, c.max_windows)
            .await
            .expect("proposers");
        let got_strs: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(got_strs, c.node_ids, "async Proposers");
    }
}
