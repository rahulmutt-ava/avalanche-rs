// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The in-memory [`Diff`] overlay (`vms/platformvm/state/diff.go`, specs 08 §3.1).
//!
//! A `Diff` is a layered, in-memory overlay over a parent [`Chain`] resolved
//! through a [`Versions`] by block ID. Reads consult the overlay first and fall
//! through to the parent; mutations are recorded only in the overlay.
//! [`Diff::apply`] flushes the overlay onto a base `Chain` (the bottom of the
//! diff stack is [`State`](super::state::State)).
//!
//! This is the versioned/diff model the block executor uses: every accepted
//! block has an associated `Diff`; on `Accept` the diff chain is applied down to
//! `State` and committed.
//!
//! ## Staker overlay
//!
//! The base/diff staker overlay lives here (Go `diffStakers`): pending/current
//! validator puts and deletes are recorded as overlay operations and replayed in
//! [`apply`](Diff::apply). UTXO and scalar overlays follow the same model.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::{Error, Result};
use crate::state::chain::{Chain, UtxoBytes, Versions};
use crate::state::l1_validator::L1Validator;
use crate::state::staker::Staker;
use crate::txs::fee::gas::GasState;

/// A pending overlay op on a staker set.
#[derive(Clone, Debug)]
enum StakerOp {
    PutValidator(Staker),
    DeleteValidator(Staker),
    PutDelegator(Staker),
    DeleteDelegator(Staker),
}

/// A pending overlay op on the UTXO set.
#[derive(Clone, Debug)]
enum UtxoOp {
    Add(UtxoBytes),
    Delete,
}

/// The in-memory diff overlay over a parent [`Chain`] (`state.diff`).
pub struct Diff {
    /// The parent state view (resolved through [`Versions`] at construction).
    parent: Arc<dyn Chain>,

    // ----- scalar overlays (`None` ⇒ inherit from parent) -----
    timestamp: Option<SystemTime>,
    fee_state: Option<GasState>,
    l1_validator_excess: Option<u64>,
    accrued_fees: Option<u64>,
    supply: BTreeMap<Id, u64>,

    // ----- UTXO overlay -----
    utxos: BTreeMap<Id, UtxoOp>,

    // ----- staker overlays (replayed in order) -----
    current_ops: Vec<StakerOp>,
    pending_ops: Vec<StakerOp>,
    /// Point-lookup overlay for current validators by `(subnet, node)`.
    current_validators: BTreeMap<(Id, NodeId), Option<Staker>>,

    // ----- L1 validators -----
    l1_validators: BTreeMap<Id, L1Validator>,

    // ----- subnets / chains / owners / managers / reward utxos -----
    added_subnets: Vec<Id>,
    subnet_owners: BTreeMap<Id, Vec<u8>>,
    subnet_managers: BTreeMap<Id, Vec<u8>>,
    added_chains: BTreeMap<Id, Vec<Id>>,
    reward_utxos: BTreeMap<Id, Vec<UtxoBytes>>,

    // ----- tx store overlay -----
    txs: BTreeMap<Id, Vec<u8>>,
}

impl Diff {
    /// Builds a `Diff` over the parent block `parent_id`, resolved through
    /// `versions`.
    ///
    /// # Errors
    /// Returns [`Error::Database`] wrapping `database.ErrNotFound` when
    /// `versions` cannot resolve `parent_id` (Go `state.NewDiff` returns
    /// `ErrMissingParentState`).
    pub fn new(parent_id: Id, versions: &dyn Versions) -> Result<Self> {
        let parent = versions
            .get_state(parent_id)
            .ok_or(Error::Database(ava_database::error::Error::NotFound))?;
        Ok(Self {
            parent,
            timestamp: None,
            fee_state: None,
            l1_validator_excess: None,
            accrued_fees: None,
            supply: BTreeMap::new(),
            utxos: BTreeMap::new(),
            current_ops: Vec::new(),
            pending_ops: Vec::new(),
            current_validators: BTreeMap::new(),
            l1_validators: BTreeMap::new(),
            added_subnets: Vec::new(),
            subnet_owners: BTreeMap::new(),
            subnet_managers: BTreeMap::new(),
            added_chains: BTreeMap::new(),
            reward_utxos: BTreeMap::new(),
            txs: BTreeMap::new(),
        })
    }

    /// Flushes this overlay onto `base` (Go `diff.Apply`).
    ///
    /// Mutations are replayed in a deterministic order: scalars, UTXOs, stakers
    /// (current then pending, in recorded order), L1 validators, then
    /// subnets/chains/owners/managers/reward-UTXOs.
    ///
    /// # Errors
    /// Propagates any fallible base mutation (current/pending validator puts,
    /// L1-validator puts).
    pub fn apply(&self, base: &mut dyn Chain) -> Result<()> {
        if let Some(t) = self.timestamp {
            base.set_timestamp(t);
        }
        if let Some(s) = self.fee_state {
            base.set_fee_state(s);
        }
        if let Some(e) = self.l1_validator_excess {
            base.set_l1_validator_excess(e);
        }
        if let Some(v) = self.accrued_fees {
            base.set_accrued_fees(v);
        }
        for (&subnet, &supply) in &self.supply {
            base.set_current_supply(subnet, supply);
        }

        for (&id, op) in &self.utxos {
            match op {
                UtxoOp::Add(b) => base.add_utxo(id, b.clone()),
                UtxoOp::Delete => base.delete_utxo(id),
            }
        }

        for op in &self.current_ops {
            match op {
                StakerOp::PutValidator(s) => base.put_current_validator(s.clone())?,
                StakerOp::DeleteValidator(s) => base.delete_current_validator(s),
                StakerOp::PutDelegator(s) => base.put_current_delegator(s.clone()),
                StakerOp::DeleteDelegator(s) => base.delete_current_delegator(s),
            }
        }
        for op in &self.pending_ops {
            match op {
                StakerOp::PutValidator(s) => base.put_pending_validator(s.clone())?,
                StakerOp::DeleteValidator(s) => base.delete_pending_validator(s),
                StakerOp::PutDelegator(s) => base.put_pending_delegator(s.clone()),
                StakerOp::DeleteDelegator(s) => base.delete_pending_delegator(s),
            }
        }

        for v in self.l1_validators.values() {
            base.put_l1_validator(v.clone())?;
        }

        for &subnet in &self.added_subnets {
            base.add_subnet(subnet);
        }
        for (&subnet, owner) in &self.subnet_owners {
            base.set_subnet_owner(subnet, owner.clone());
        }
        for (&subnet, manager) in &self.subnet_managers {
            base.set_subnet_manager(subnet, manager.clone());
        }
        for (&subnet, chains) in &self.added_chains {
            for &chain in chains {
                base.add_chain(subnet, chain);
            }
        }
        for (&tx_id, utxos) in &self.reward_utxos {
            for u in utxos {
                base.add_reward_utxo(tx_id, u.clone());
            }
        }
        for (&tx_id, bytes) in &self.txs {
            base.add_tx(tx_id, bytes.clone());
        }
        Ok(())
    }
}

impl Chain for Diff {
    fn timestamp(&self) -> SystemTime {
        self.timestamp.unwrap_or_else(|| self.parent.timestamp())
    }

    fn set_timestamp(&mut self, t: SystemTime) {
        self.timestamp = Some(t);
    }

    fn current_supply(&self, subnet: Id) -> Result<u64> {
        match self.supply.get(&subnet) {
            Some(&v) => Ok(v),
            None => self.parent.current_supply(subnet),
        }
    }

    fn set_current_supply(&mut self, subnet: Id, supply: u64) {
        self.supply.insert(subnet, supply);
    }

    fn fee_state(&self) -> GasState {
        self.fee_state.unwrap_or_else(|| self.parent.fee_state())
    }

    fn set_fee_state(&mut self, s: GasState) {
        self.fee_state = Some(s);
    }

    fn l1_validator_excess(&self) -> u64 {
        self.l1_validator_excess
            .unwrap_or_else(|| self.parent.l1_validator_excess())
    }

    fn set_l1_validator_excess(&mut self, excess: u64) {
        self.l1_validator_excess = Some(excess);
    }

    fn accrued_fees(&self) -> u64 {
        self.accrued_fees
            .unwrap_or_else(|| self.parent.accrued_fees())
    }

    fn set_accrued_fees(&mut self, v: u64) {
        self.accrued_fees = Some(v);
    }

    fn get_utxo(&self, id: Id) -> Result<UtxoBytes> {
        match self.utxos.get(&id) {
            Some(UtxoOp::Add(b)) => Ok(b.clone()),
            Some(UtxoOp::Delete) => Err(Error::Database(ava_database::error::Error::NotFound)),
            None => self.parent.get_utxo(id),
        }
    }

    fn add_utxo(&mut self, id: Id, utxo: UtxoBytes) {
        self.utxos.insert(id, UtxoOp::Add(utxo));
    }

    fn delete_utxo(&mut self, id: Id) {
        self.utxos.insert(id, UtxoOp::Delete);
    }

    fn get_current_validator(&self, subnet: Id, node: NodeId) -> Result<Staker> {
        match self.current_validators.get(&(subnet, node)) {
            Some(Some(s)) => Ok(s.clone()),
            Some(None) => Err(Error::Database(ava_database::error::Error::NotFound)),
            None => self.parent.get_current_validator(subnet, node),
        }
    }

    fn put_current_validator(&mut self, s: Staker) -> Result<()> {
        if !s.priority.is_current() {
            return Err(Error::WrongTxType);
        }
        self.current_validators
            .insert((s.subnet_id, s.node_id), Some(s.clone()));
        self.current_ops.push(StakerOp::PutValidator(s));
        Ok(())
    }

    fn delete_current_validator(&mut self, s: &Staker) {
        self.current_validators
            .insert((s.subnet_id, s.node_id), None);
        self.current_ops.push(StakerOp::DeleteValidator(s.clone()));
    }

    fn put_current_delegator(&mut self, s: Staker) {
        self.current_ops.push(StakerOp::PutDelegator(s));
    }

    fn delete_current_delegator(&mut self, s: &Staker) {
        self.current_ops.push(StakerOp::DeleteDelegator(s.clone()));
    }

    fn current_stakers(&self) -> Vec<Staker> {
        // Overlay-aware enumeration is not needed by the M4.13 consumers; the
        // executor (M4.16+) walks the iterator through dedicated helpers. Expose
        // the parent's view as the base; recorded ops are applied on flush.
        self.parent.current_stakers()
    }

    fn put_pending_validator(&mut self, s: Staker) -> Result<()> {
        if !s.priority.is_pending() {
            return Err(Error::WrongTxType);
        }
        self.pending_ops.push(StakerOp::PutValidator(s));
        Ok(())
    }

    fn delete_pending_validator(&mut self, s: &Staker) {
        self.pending_ops.push(StakerOp::DeleteValidator(s.clone()));
    }

    fn put_pending_delegator(&mut self, s: Staker) {
        self.pending_ops.push(StakerOp::PutDelegator(s));
    }

    fn delete_pending_delegator(&mut self, s: &Staker) {
        self.pending_ops.push(StakerOp::DeleteDelegator(s.clone()));
    }

    fn pending_stakers(&self) -> Vec<Staker> {
        self.parent.pending_stakers()
    }

    fn get_l1_validator(&self, validation_id: Id) -> Result<L1Validator> {
        match self.l1_validators.get(&validation_id) {
            Some(v) => Ok(v.clone()),
            None => self.parent.get_l1_validator(validation_id),
        }
    }

    fn put_l1_validator(&mut self, v: L1Validator) -> Result<()> {
        self.l1_validators.insert(v.validation_id, v);
        Ok(())
    }

    fn weight_of_l1_validators(&self, subnet: Id) -> Result<u64> {
        // Sum the overlay's L1 validators for this subnet over the parent total.
        let mut total = self.parent.weight_of_l1_validators(subnet)?;
        for v in self.l1_validators.values() {
            if v.subnet_id == subnet {
                total = total.checked_add(v.weight).ok_or(Error::Overflow)?;
            }
        }
        Ok(total)
    }

    fn active_l1_validators(&self) -> Vec<L1Validator> {
        // Overlay-aware active set: take the parent's active validators, drop any
        // shadowed by an overlay entry, then add the overlay's own active ones.
        let mut by_id: BTreeMap<Id, L1Validator> = BTreeMap::new();
        for v in self.parent.active_l1_validators() {
            by_id.insert(v.validation_id, v);
        }
        for (&id, v) in &self.l1_validators {
            if v.is_active() {
                by_id.insert(id, v.clone());
            } else {
                by_id.remove(&id);
            }
        }
        let mut out: Vec<L1Validator> = by_id.into_values().collect();
        out.sort_by(L1Validator::compare);
        out
    }

    fn subnets(&self) -> Vec<Id> {
        let mut out = self.parent.subnets();
        for &s in &self.added_subnets {
            if !out.contains(&s) {
                out.push(s);
            }
        }
        out
    }

    fn add_subnet(&mut self, subnet: Id) {
        if !self.added_subnets.contains(&subnet) {
            self.added_subnets.push(subnet);
        }
    }

    fn get_subnet_owner(&self, subnet: Id) -> Result<Vec<u8>> {
        match self.subnet_owners.get(&subnet) {
            Some(o) => Ok(o.clone()),
            None => self.parent.get_subnet_owner(subnet),
        }
    }

    fn set_subnet_owner(&mut self, subnet: Id, owner: Vec<u8>) {
        self.subnet_owners.insert(subnet, owner);
    }

    fn get_subnet_manager(&self, subnet: Id) -> Result<Vec<u8>> {
        match self.subnet_managers.get(&subnet) {
            Some(m) => Ok(m.clone()),
            None => self.parent.get_subnet_manager(subnet),
        }
    }

    fn set_subnet_manager(&mut self, subnet: Id, manager: Vec<u8>) {
        self.subnet_managers.insert(subnet, manager);
    }

    fn chains(&self, subnet: Id) -> Vec<Id> {
        let mut out = self.parent.chains(subnet);
        if let Some(added) = self.added_chains.get(&subnet) {
            for &c in added {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        out
    }

    fn add_chain(&mut self, subnet: Id, chain: Id) {
        self.added_chains.entry(subnet).or_default().push(chain);
    }

    fn get_reward_utxos(&self, tx_id: Id) -> Vec<UtxoBytes> {
        let mut out = self.parent.get_reward_utxos(tx_id);
        if let Some(added) = self.reward_utxos.get(&tx_id) {
            out.extend(added.iter().cloned());
        }
        out
    }

    fn add_reward_utxo(&mut self, tx_id: Id, utxo: UtxoBytes) {
        self.reward_utxos.entry(tx_id).or_default().push(utxo);
    }

    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>> {
        match self.txs.get(&tx_id) {
            Some(b) => Ok(b.clone()),
            None => self.parent.get_tx(tx_id),
        }
    }

    fn add_tx(&mut self, tx_id: Id, tx_bytes: Vec<u8>) {
        self.txs.insert(tx_id, tx_bytes);
    }
}
