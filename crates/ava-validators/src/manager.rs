// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`ValidatorManager`] trait and a default in-memory implementation.
//!
//! Port of `snow/validators/manager.go`. The manager owns one [`Set`] per subnet
//! behind a `std::sync::Mutex` (Go guards every subnet `Set` under one `RWMutex`).
//! Poll sampling (`sample`) is **non-deterministic** — it seeds the M0
//! [`Mt19937_64`] from coarse OS entropy, because *which* validators we ask does
//! not affect *which* block is decided (`specs/06-consensus.md` §6.2). The
//! windower, which needs the seeded deterministic stream, calls [`Set::sample`]
//! directly with its own gonum-MT source.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ava_crypto::bls::PublicKey;
use ava_utils::rng::{Mt19937_64, Source};

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::{Error, Result};
use crate::set::Set;
use crate::validator::Validator;

/// A subscriber notified on validator weight changes within a subnet
/// (Go `validators.SetCallbackListener`).
pub trait ManagerCallbackListener: Send + Sync {
    /// A validator was added with the given starting weight.
    fn on_validator_added(&self, subnet: Id, node_id: NodeId, weight: u64);
    /// A validator was fully removed.
    fn on_validator_removed(&self, subnet: Id, node_id: NodeId, weight: u64);
    /// A validator's weight changed from `old` to `new`.
    fn on_weight_changed(&self, subnet: Id, node_id: NodeId, old: u64, new: u64);
}

/// Per-subnet validator management with deterministic-input sampling
/// (Go `validators.Manager`).
pub trait ValidatorManager: Send + Sync {
    /// Adds a new staker to `subnet`.
    ///
    /// # Errors
    /// Propagates [`Set::add_staker`] errors (duplicate / zero weight).
    fn add_staker(
        &self,
        subnet: Id,
        node: NodeId,
        pk: Option<PublicKey>,
        tx: Id,
        weight: u64,
    ) -> Result<()>;

    /// Adds `weight` to a validator (creating a weight-only entry if absent).
    ///
    /// # Errors
    /// Propagates [`Set::add_weight`] errors (zero weight / overflow).
    fn add_weight(&self, subnet: Id, node: NodeId, weight: u64) -> Result<()>;

    /// Removes `weight` from a validator.
    ///
    /// # Errors
    /// Propagates [`Set::remove_weight`] errors (underflow / absent).
    fn remove_weight(&self, subnet: Id, node: NodeId, weight: u64) -> Result<()>;

    /// Returns a validator's weight (0 if absent).
    fn get_weight(&self, subnet: Id, node: NodeId) -> u64;

    /// Returns a validator record, if present.
    fn get_validator(&self, subnet: Id, node: NodeId) -> Option<Validator>;

    /// Returns the subnet's validator ids in `NodeId`-ascending order.
    fn get_validator_ids(&self, subnet: Id) -> Vec<NodeId>;

    /// Sums the weight of the supplied subset.
    ///
    /// # Errors
    /// [`Error::WeightOverflow`] on sum overflow.
    fn subset_weight(&self, subnet: Id, ids: &HashSet<NodeId>) -> Result<u64>;

    /// Sums the subnet's total weight.
    ///
    /// # Errors
    /// [`Error::WeightOverflow`] on sum overflow.
    fn total_weight(&self, subnet: Id) -> Result<u64>;

    /// Number of validators in the subnet.
    fn num_validators(&self, subnet: Id) -> usize;

    /// Number of subnets currently tracked.
    fn num_subnets(&self) -> usize;

    /// Non-deterministic weighted-without-replacement poll sample of `size`
    /// validators from the `NodeId`-sorted snapshot.
    ///
    /// # Errors
    /// [`Error::MissingValidators`] / [`Error::InsufficientValidators`] /
    /// [`Error::WeightOverflow`].
    fn sample(&self, subnet: Id, size: usize) -> Result<Vec<NodeId>>;

    /// Registers a callback listener for the subnet.
    fn register_callback_listener(&self, subnet: Id, l: Arc<dyn ManagerCallbackListener>);
}

/// A coarse OS-entropy seed for poll sampling — `SystemTime` nanos XOR a
/// per-process monotonically-increasing counter. Adequate because poll sampling
/// is off the consensus-decision path (`specs/06` §6.2).
static SAMPLE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn os_seeded_source() -> Box<dyn Source> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u64, |d| d.as_nanos() as u64);
    let counter = SAMPLE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut mt = Mt19937_64::new();
    mt.seed(nanos ^ counter.rotate_left(32));
    Box::new(mt)
}

/// Default in-memory [`ValidatorManager`] backed by one [`Set`] per subnet.
#[derive(Default)]
pub struct DefaultManager {
    inner: Mutex<ManagerState>,
}

#[derive(Default)]
struct ManagerState {
    subnets: HashMap<Id, Set>,
    listeners: HashMap<Id, Vec<Arc<dyn ManagerCallbackListener>>>,
}

impl DefaultManager {
    /// Creates an empty manager.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn notify_added(state: &ManagerState, subnet: Id, node: NodeId, weight: u64) {
        if let Some(ls) = state.listeners.get(&subnet) {
            for l in ls {
                l.on_validator_added(subnet, node, weight);
            }
        }
    }

    fn notify_removed(state: &ManagerState, subnet: Id, node: NodeId, weight: u64) {
        if let Some(ls) = state.listeners.get(&subnet) {
            for l in ls {
                l.on_validator_removed(subnet, node, weight);
            }
        }
    }

    fn notify_changed(state: &ManagerState, subnet: Id, node: NodeId, old: u64, new: u64) {
        if let Some(ls) = state.listeners.get(&subnet) {
            for l in ls {
                l.on_weight_changed(subnet, node, old, new);
            }
        }
    }
}

impl ValidatorManager for DefaultManager {
    fn add_staker(
        &self,
        subnet: Id,
        node: NodeId,
        pk: Option<PublicKey>,
        tx: Id,
        weight: u64,
    ) -> Result<()> {
        // Lock poisoning is unrecoverable here; treat it as missing validators.
        let mut state = self.inner.lock().map_err(|_| Error::MissingValidators)?;
        let set = state.subnets.entry(subnet).or_default();
        set.add_staker(Validator {
            node_id: node,
            public_key: pk,
            tx_id: tx,
            weight,
        })?;
        Self::notify_added(&state, subnet, node, weight);
        Ok(())
    }

    fn add_weight(&self, subnet: Id, node: NodeId, weight: u64) -> Result<()> {
        let mut state = self.inner.lock().map_err(|_| Error::MissingValidators)?;
        let set = state.subnets.entry(subnet).or_default();
        let old = set.get_weight(node);
        set.add_weight(node, weight)?;
        let new = set.get_weight(node);
        if old == 0 {
            Self::notify_added(&state, subnet, node, new);
        } else {
            Self::notify_changed(&state, subnet, node, old, new);
        }
        Ok(())
    }

    fn remove_weight(&self, subnet: Id, node: NodeId, weight: u64) -> Result<()> {
        let mut state = self.inner.lock().map_err(|_| Error::MissingValidators)?;
        let set = match state.subnets.get_mut(&subnet) {
            Some(s) => s,
            None => {
                return Err(Error::WeightUnderflow {
                    requested: weight,
                    present: 0,
                });
            }
        };
        let old = set.get_weight(node);
        set.remove_weight(node, weight)?;
        let new = set.get_weight(node);
        if new == 0 {
            Self::notify_removed(&state, subnet, node, old);
        } else {
            Self::notify_changed(&state, subnet, node, old, new);
        }
        Ok(())
    }

    fn get_weight(&self, subnet: Id, node: NodeId) -> u64 {
        self.inner
            .lock()
            .ok()
            .and_then(|s| s.subnets.get(&subnet).map(|set| set.get_weight(node)))
            .unwrap_or(0)
    }

    fn get_validator(&self, subnet: Id, node: NodeId) -> Option<Validator> {
        self.inner.lock().ok().and_then(|s| {
            s.subnets
                .get(&subnet)
                .and_then(|set| set.get_validator(node))
        })
    }

    fn get_validator_ids(&self, subnet: Id) -> Vec<NodeId> {
        self.inner
            .lock()
            .ok()
            .and_then(|s| s.subnets.get(&subnet).map(Set::get_validator_ids))
            .unwrap_or_default()
    }

    fn subset_weight(&self, subnet: Id, ids: &HashSet<NodeId>) -> Result<u64> {
        let state = self.inner.lock().map_err(|_| Error::MissingValidators)?;
        match state.subnets.get(&subnet) {
            Some(set) => set.subset_weight(ids),
            None => Ok(0),
        }
    }

    fn total_weight(&self, subnet: Id) -> Result<u64> {
        let state = self.inner.lock().map_err(|_| Error::MissingValidators)?;
        match state.subnets.get(&subnet) {
            Some(set) => set.total_weight(),
            None => Ok(0),
        }
    }

    fn num_validators(&self, subnet: Id) -> usize {
        self.inner
            .lock()
            .ok()
            .and_then(|s| s.subnets.get(&subnet).map(Set::len))
            .unwrap_or(0)
    }

    fn num_subnets(&self) -> usize {
        self.inner.lock().map_or(0, |s| s.subnets.len())
    }

    fn sample(&self, subnet: Id, size: usize) -> Result<Vec<NodeId>> {
        let state = self.inner.lock().map_err(|_| Error::MissingValidators)?;
        let set = state.subnets.get(&subnet).ok_or(Error::MissingValidators)?;
        set.sample(size, os_seeded_source())
    }

    fn register_callback_listener(&self, subnet: Id, l: Arc<dyn ManagerCallbackListener>) {
        if let Ok(mut state) = self.inner.lock() {
            match state.listeners.entry(subnet) {
                Entry::Occupied(mut e) => e.get_mut().push(l),
                Entry::Vacant(e) => {
                    e.insert(vec![l]);
                }
            }
        }
    }
}
