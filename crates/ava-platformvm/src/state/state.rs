// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The persisted P-Chain [`State`] base (`vms/platformvm/state/state.go`,
//! specs 08 §3.1/§3.2/§3.5).
//!
//! `State` is the bottom of the diff stack: every accepted block's
//! [`Diff`](super::diff::Diff) is ultimately applied down to a `State`. It stores
//! the flat-KV spaces of §3.2 over a base [`Database`], each behind an LRU front
//! cache.
//!
//! ## What is persisted vs. in-memory (as-built, M4.13)
//!
//! - **Byte-valued spaces** (UTXOs, reward UTXOs, subnet owners/managers, the set
//!   of subnets, per-subnet chains) are written through to their
//!   [`PrefixDb`](ava_database::PrefixDb) keyspace with an LRU front cache — this
//!   is what the RocksDB `state_roundtrip` conformance test exercises.
//! - **Scalar singletons** (timestamp, supply, fee state, L1 excess, accrued
//!   fees) are kept as in-memory fields and written through to the
//!   `singletonDB`.
//! - **Stakers / L1 validators** are kept in the in-memory [`Stakers`] /
//!   validation-ID map mirroring Go's `baseStakers` (§3.3). Flushing them to the
//!   disk staker sublists + the weight/pk-diff iterators is the block acceptor's
//!   job (M4.14 / M4.20); the prefix handles are created here so those tasks can
//!   build on them.
//!
//! The `weightDiffDB` / `pkDiffDB` handles are created but unused here — their
//! byte-exact iterators are M4.14.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_database::error::Error as DbError;
use ava_database::{Database, KeyValueDeleter, KeyValueReader, KeyValueWriter, PrefixDb};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use parking_lot::Mutex;

use crate::error::{Error, Result};
use crate::state::chain::{Chain, UtxoBytes};
use crate::state::l1_validator::L1Validator;
use crate::state::prefixes;
use crate::state::staker::Staker;
use crate::state::stakers::Stakers;
use crate::txs::fee::gas::GasState;

/// The default per-space LRU capacity (Go uses a handful of fixed cache sizes;
/// a single shared cap is enough for parity at the trait level).
const CACHE_SIZE: usize = 8_192;

/// A small `Mutex<lru::LruCache>` front cache over a byte-valued prefix space.
///
/// `parking_lot::Mutex` (already a crate dep) guards the non-`Sync`
/// [`lru::LruCache`]; the cache holds the most-recently-touched `key → value`
/// pairs so repeated reads do not hit the base DB.
struct Cache {
    inner: Mutex<lru::LruCache<Vec<u8>, Vec<u8>>>,
}

impl Cache {
    fn new() -> Self {
        // CACHE_SIZE is a non-zero const; the unwrap is unreachable but kept
        // panic-free via a saturating fallback to 1.
        let cap = NonZeroUsize::new(CACHE_SIZE).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Mutex::new(lru::LruCache::new(cap)),
        }
    }

    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.inner.lock().get(key).cloned()
    }

    fn put(&self, key: Vec<u8>, value: Vec<u8>) {
        self.inner.lock().put(key, value);
    }

    fn pop(&self, key: &[u8]) {
        self.inner.lock().pop(key);
    }
}

/// A byte-valued prefix space (a [`PrefixDb`] + an LRU front cache).
struct ByteSpace<D: Database> {
    db: PrefixDb<D>,
    cache: Cache,
}

impl<D: Database> ByteSpace<D> {
    fn new(prefix: &[u8], base: &Arc<D>) -> Self {
        Self {
            db: PrefixDb::new_arc(prefix, Arc::clone(base)),
            cache: Cache::new(),
        }
    }

    /// Reads `key`, consulting the LRU cache first. `NotFound` is propagated as
    /// [`Error::Database`].
    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        if let Some(v) = self.cache.get(key) {
            return Ok(v);
        }
        let v = self.db.get(key)?;
        self.cache.put(key.to_vec(), v.clone());
        Ok(v)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.db.put(key, value)?;
        self.cache.put(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<()> {
        self.db.delete(key)?;
        self.cache.pop(key);
        Ok(())
    }
}

/// The persisted P-Chain state base (`state.State`).
///
/// Generic over the base [`Database`] backend so the same code serves the
/// in-memory `MemDb` (tests, bootstrap) and the on-disk `RocksDb` (production).
pub struct State<D: Database> {
    // ----- byte-valued, persisted spaces (LRU-fronted) -----
    utxos: ByteSpace<D>,
    reward_utxos: PrefixDb<D>,
    subnets: ByteSpace<D>,
    subnet_owners: ByteSpace<D>,
    subnet_managers: ByteSpace<D>,
    chains: PrefixDb<D>,
    singletons: ByteSpace<D>,
    /// Handles created for M4.14 (weight/pk-diff iterators); unused here.
    _weight_diffs: PrefixDb<D>,
    _pk_diffs: PrefixDb<D>,
    _blocks: PrefixDb<D>,
    _block_ids: PrefixDb<D>,
    _txs: PrefixDb<D>,

    // ----- scalar singletons (in-memory, written through to `singletons`) -----
    timestamp: SystemTime,
    fee_state: GasState,
    l1_validator_excess: u64,
    accrued_fees: u64,
    supply: BTreeMap<Id, u64>,

    // ----- in-memory staker / L1-validator collections -----
    current: Stakers,
    pending: Stakers,
    l1_validators: BTreeMap<Id, L1Validator>,

    // ----- in-memory reward-utxo accumulator (keyed by staker tx id) -----
    reward_utxo_index: BTreeMap<Id, Vec<UtxoBytes>>,
    subnet_ids: Vec<Id>,
    chain_index: BTreeMap<Id, Vec<Id>>,
}

impl<D: Database> State<D> {
    /// Builds a `State` over `base`, wiring every §3.2 prefix space.
    ///
    /// # Errors
    /// Returns an error if a prefix space cannot be initialized.
    pub fn new(base: D) -> Result<Self> {
        let base = Arc::new(base);

        let validators = PrefixDb::new_arc(prefixes::VALIDATORS_PREFIX, Arc::clone(&base));
        let l1_parent = PrefixDb::new_arc(prefixes::L1_VALIDATORS_PREFIX, Arc::clone(&base));

        Ok(Self {
            utxos: ByteSpace::new(prefixes::UTXO_PREFIX, &base),
            reward_utxos: PrefixDb::new_arc(prefixes::REWARD_UTXOS_PREFIX, Arc::clone(&base)),
            subnets: ByteSpace::new(prefixes::SUBNET_PREFIX, &base),
            subnet_owners: ByteSpace::new(prefixes::SUBNET_OWNER_PREFIX, &base),
            subnet_managers: ByteSpace::new(prefixes::SUBNET_MANAGER_PREFIX, &base),
            chains: PrefixDb::new_arc(prefixes::CHAIN_PREFIX, Arc::clone(&base)),
            singletons: ByteSpace::new(prefixes::SINGLETON_PREFIX, &base),
            _weight_diffs: l1_parent.join(prefixes::WEIGHT_DIFF_PREFIX),
            _pk_diffs: l1_parent.join(prefixes::PK_DIFF_PREFIX),
            _blocks: PrefixDb::new_arc(prefixes::BLOCK_PREFIX, Arc::clone(&base)),
            _block_ids: PrefixDb::new_arc(prefixes::BLOCK_ID_PREFIX, Arc::clone(&base)),
            _txs: validators.join(prefixes::TX_PREFIX),

            timestamp: UNIX_EPOCH,
            fee_state: GasState::default(),
            l1_validator_excess: 0,
            accrued_fees: 0,
            supply: BTreeMap::new(),

            current: Stakers::new(),
            pending: Stakers::new(),
            l1_validators: BTreeMap::new(),

            reward_utxo_index: BTreeMap::new(),
            subnet_ids: Vec::new(),
            chain_index: BTreeMap::new(),
        })
    }

    /// The supply singleton key for `subnet`: the literal key for the Primary
    /// Network, else the key suffixed by the subnet id.
    fn supply_key(subnet: Id) -> Vec<u8> {
        if subnet == Id::EMPTY {
            prefixes::CURRENT_SUPPLY_KEY.to_vec()
        } else {
            let mut k = prefixes::CURRENT_SUPPLY_KEY.to_vec();
            k.extend_from_slice(subnet.as_bytes());
            k
        }
    }

    fn write_u64_singleton(&self, key: &[u8], v: u64) {
        // Singleton writes never fail observably for the in-memory mirror; a DB
        // error is swallowed here because the in-memory field is authoritative
        // for reads (matching Go, which treats these as cached fields flushed at
        // commit time).
        let _ = self.singletons.put(key, &v.to_be_bytes());
    }
}

impl<D: Database> Chain for State<D> {
    fn timestamp(&self) -> SystemTime {
        self.timestamp
    }

    fn set_timestamp(&mut self, t: SystemTime) {
        self.timestamp = t;
        let secs = t
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        self.write_u64_singleton(prefixes::TIMESTAMP_KEY, secs);
    }

    fn current_supply(&self, subnet: Id) -> Result<u64> {
        Ok(self.supply.get(&subnet).copied().unwrap_or(0))
    }

    fn set_current_supply(&mut self, subnet: Id, supply: u64) {
        self.supply.insert(subnet, supply);
        let key = Self::supply_key(subnet);
        self.write_u64_singleton(&key, supply);
    }

    fn fee_state(&self) -> GasState {
        self.fee_state
    }

    fn set_fee_state(&mut self, s: GasState) {
        self.fee_state = s;
        let mut buf = [0u8; 16];
        let (a, b) = buf.split_at_mut(8);
        a.copy_from_slice(&s.capacity.to_be_bytes());
        b.copy_from_slice(&s.excess.to_be_bytes());
        let _ = self.singletons.put(prefixes::FEE_STATE_KEY, &buf);
    }

    fn l1_validator_excess(&self) -> u64 {
        self.l1_validator_excess
    }

    fn set_l1_validator_excess(&mut self, excess: u64) {
        self.l1_validator_excess = excess;
        self.write_u64_singleton(prefixes::L1_VALIDATOR_EXCESS_KEY, excess);
    }

    fn accrued_fees(&self) -> u64 {
        self.accrued_fees
    }

    fn set_accrued_fees(&mut self, v: u64) {
        self.accrued_fees = v;
        self.write_u64_singleton(prefixes::ACCRUED_FEES_KEY, v);
    }

    fn get_utxo(&self, id: Id) -> Result<UtxoBytes> {
        self.utxos.get(id.as_bytes())
    }

    fn add_utxo(&mut self, id: Id, utxo: UtxoBytes) {
        let _ = self.utxos.put(id.as_bytes(), &utxo);
    }

    fn delete_utxo(&mut self, id: Id) {
        let _ = self.utxos.delete(id.as_bytes());
    }

    fn get_current_validator(&self, subnet: Id, node: NodeId) -> Result<Staker> {
        self.current
            .get_validator(subnet, node)
            .cloned()
            .ok_or(Error::Database(DbError::NotFound))
    }

    fn put_current_validator(&mut self, s: Staker) -> Result<()> {
        if !s.priority.is_current() {
            return Err(Error::WrongTxType);
        }
        self.current.put_validator(s);
        Ok(())
    }

    fn delete_current_validator(&mut self, s: &Staker) {
        self.current.delete_validator(s);
    }

    fn put_current_delegator(&mut self, s: Staker) {
        self.current.put_delegator(s);
    }

    fn delete_current_delegator(&mut self, s: &Staker) {
        self.current.delete_delegator(s);
    }

    fn current_stakers(&self) -> Vec<Staker> {
        self.current.to_vec()
    }

    fn put_pending_validator(&mut self, s: Staker) -> Result<()> {
        if !s.priority.is_pending() {
            return Err(Error::WrongTxType);
        }
        self.pending.put_validator(s);
        Ok(())
    }

    fn delete_pending_validator(&mut self, s: &Staker) {
        self.pending.delete_validator(s);
    }

    fn put_pending_delegator(&mut self, s: Staker) {
        self.pending.put_delegator(s);
    }

    fn delete_pending_delegator(&mut self, s: &Staker) {
        self.pending.delete_delegator(s);
    }

    fn pending_stakers(&self) -> Vec<Staker> {
        self.pending.to_vec()
    }

    fn get_l1_validator(&self, validation_id: Id) -> Result<L1Validator> {
        self.l1_validators
            .get(&validation_id)
            .cloned()
            .ok_or(Error::Database(DbError::NotFound))
    }

    fn put_l1_validator(&mut self, v: L1Validator) -> Result<()> {
        self.l1_validators.insert(v.validation_id, v);
        Ok(())
    }

    fn weight_of_l1_validators(&self, subnet: Id) -> Result<u64> {
        let mut total: u64 = 0;
        for v in self.l1_validators.values() {
            if v.subnet_id == subnet {
                total = total.checked_add(v.weight).ok_or(Error::Overflow)?;
            }
        }
        Ok(total)
    }

    fn subnets(&self) -> Vec<Id> {
        self.subnet_ids.clone()
    }

    fn add_subnet(&mut self, subnet: Id) {
        if !self.subnet_ids.contains(&subnet) {
            self.subnet_ids.push(subnet);
            let _ = self.subnets.put(subnet.as_bytes(), &[]);
        }
    }

    fn get_subnet_owner(&self, subnet: Id) -> Result<Vec<u8>> {
        self.subnet_owners.get(subnet.as_bytes())
    }

    fn set_subnet_owner(&mut self, subnet: Id, owner: Vec<u8>) {
        let _ = self.subnet_owners.put(subnet.as_bytes(), &owner);
    }

    fn get_subnet_manager(&self, subnet: Id) -> Result<Vec<u8>> {
        self.subnet_managers.get(subnet.as_bytes())
    }

    fn set_subnet_manager(&mut self, subnet: Id, manager: Vec<u8>) {
        let _ = self.subnet_managers.put(subnet.as_bytes(), &manager);
    }

    fn chains(&self, subnet: Id) -> Vec<Id> {
        self.chain_index.get(&subnet).cloned().unwrap_or_default()
    }

    fn add_chain(&mut self, subnet: Id, chain: Id) {
        let list = self.chain_index.entry(subnet).or_default();
        if !list.contains(&chain) {
            list.push(chain);
        }
        // Persist the membership marker (chain id under the subnet sub-space).
        let space = self.chains.join(subnet.as_bytes());
        let _ = space.put(chain.as_bytes(), &[]);
    }

    fn get_reward_utxos(&self, tx_id: Id) -> Vec<UtxoBytes> {
        self.reward_utxo_index
            .get(&tx_id)
            .cloned()
            .unwrap_or_default()
    }

    fn add_reward_utxo(&mut self, tx_id: Id, utxo: UtxoBytes) {
        // Persist under tx_id sub-space keyed by ordinal; mirror in memory.
        let list = self.reward_utxo_index.entry(tx_id).or_default();
        let idx = list.len();
        let space = self.reward_utxos.join(tx_id.as_bytes());
        let _ = space.put(&(idx as u64).to_be_bytes(), &utxo);
        list.push(utxo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ava_database::MemDb;

    #[test]
    fn empty_supply_is_zero() {
        let s = State::new(MemDb::new()).expect("state");
        assert_eq!(s.current_supply(Id::EMPTY).expect("supply"), 0);
    }
}
