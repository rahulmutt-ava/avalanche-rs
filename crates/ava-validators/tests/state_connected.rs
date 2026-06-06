// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M3.7 tests: the `BTreeMap` determinism contract on `get_validator_set`, the
//! cached/locked adapters, and the connected-validators connectivity ratio.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use ava_types::id::Id;
use ava_types::node_id::{NODE_ID_LEN, NodeId};
use ava_validators::connected::ConnectedValidators;
use ava_validators::error::Result;
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::state_adapters::{CachedState, LockedState};
use ava_validators::validator::GetValidatorOutput;

fn node(i: u32) -> NodeId {
    let mut b = [0u8; NODE_ID_LEN];
    b[16..20].copy_from_slice(&i.to_be_bytes());
    NodeId::from(b)
}

/// A fake `ValidatorState` returning a fixed validator set; counts calls so the
/// cache adapter's memoization can be observed.
struct FakeState {
    calls: AtomicUsize,
}

#[async_trait]
impl ValidatorState for FakeState {
    async fn get_minimum_height(&self) -> Result<u64> {
        Ok(0)
    }
    async fn get_current_height(&self) -> Result<u64> {
        Ok(42)
    }
    async fn get_subnet_id(&self, _chain: Id) -> Result<Id> {
        Ok(Id::default())
    }
    async fn get_validator_set(
        &self,
        _height: u64,
        _subnet: Id,
    ) -> Result<BTreeMap<NodeId, GetValidatorOutput>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut out = BTreeMap::new();
        // Insert out of insertion order; BTreeMap reorders to NodeId-ascending.
        for i in [4u32, 1, 3, 2] {
            out.insert(
                node(i),
                GetValidatorOutput {
                    node_id: node(i),
                    public_key: None,
                    weight: u64::from(i),
                },
            );
        }
        Ok(out)
    }
    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        Ok((BTreeMap::new(), 42))
    }
    async fn get_warp_validator_sets(&self, _height: u64) -> Result<HashMap<Id, WarpSet>> {
        Ok(HashMap::new())
    }
}

#[tokio::test]
async fn validator_set_is_sorted() {
    let state = FakeState {
        calls: AtomicUsize::new(0),
    };
    let set = state.get_validator_set(0, Id::default()).await.unwrap();

    // Iterating the BTreeMap is NodeId-ascending — the windower determinism
    // contract.
    let ids: Vec<NodeId> = set.keys().copied().collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    assert_eq!(ids, vec![node(1), node(2), node(3), node(4)]);
}

#[tokio::test]
async fn cached_state_memoizes_validator_set() {
    let fake = Arc::new(FakeState {
        calls: AtomicUsize::new(0),
    });
    let cached = CachedState::new(fake.clone(), 8);

    let a = cached.get_validator_set(7, Id::default()).await.unwrap();
    let b = cached.get_validator_set(7, Id::default()).await.unwrap();
    assert_eq!(a.len(), 4);
    assert_eq!(b.len(), 4);
    // The inner state was hit only once for the repeated (height, subnet) key.
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);

    // A different height misses the cache.
    let _ = cached.get_validator_set(8, Id::default()).await.unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);

    // The cached map is still NodeId-ascending.
    let ids: Vec<NodeId> = a.keys().copied().collect();
    assert_eq!(ids, vec![node(1), node(2), node(3), node(4)]);
}

#[tokio::test]
async fn locked_state_forwards() {
    let fake = Arc::new(FakeState {
        calls: AtomicUsize::new(0),
    });
    let locked = LockedState::new(fake);
    assert_eq!(locked.get_current_height().await.unwrap(), 42);
    let set = locked.get_validator_set(1, Id::default()).await.unwrap();
    assert_eq!(set.len(), 4);
}

#[test]
fn connected_tracker_min_percent() {
    let mut tracker = ConnectedValidators::new();
    // Empty subnet is treated as fully connected.
    assert!((tracker.percent_connected() - 1.0).abs() < f64::EPSILON);

    // Total weight 100 across four validators.
    tracker.add_validator(node(1), None, 10).unwrap();
    tracker.add_validator(node(2), None, 20).unwrap();
    tracker.add_validator(node(3), None, 30).unwrap();
    tracker.add_validator(node(4), None, 40).unwrap();
    assert_eq!(tracker.total_weight(), 100);

    // Nothing connected yet.
    assert!(tracker.percent_connected().abs() < f64::EPSILON);

    // Connect 30 of 100 stake → 0.30.
    tracker.connect(node(1)).unwrap();
    tracker.connect(node(2)).unwrap();
    assert_eq!(tracker.connected_weight(), 30);
    assert!((tracker.percent_connected() - 0.30).abs() < 1e-9);
    assert_eq!(tracker.num_connected(), 2);

    // Connect the rest → fully connected.
    tracker.connect(node(3)).unwrap();
    tracker.connect(node(4)).unwrap();
    assert!((tracker.percent_connected() - 1.0).abs() < 1e-9);

    // Disconnect 40 → 0.60.
    tracker.disconnect(node(4)).unwrap();
    assert_eq!(tracker.connected_weight(), 60);
    assert!((tracker.percent_connected() - 0.60).abs() < 1e-9);

    // Idempotent disconnect of an already-gone node is a no-op.
    tracker.disconnect(node(4)).unwrap();
    assert_eq!(tracker.connected_weight(), 60);
}
