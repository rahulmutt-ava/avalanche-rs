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

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_crypto::bls;
use ava_database::error::Error as DbError;
use ava_database::{
    Database, Iteratee, Iterator as _, KeyValueDeleter, KeyValueReader, KeyValueWriter, PrefixDb,
};
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use parking_lot::Mutex;

use crate::error::{Error, Result};
use crate::state::chain::{Chain, UtxoBytes};
use crate::state::diff_iterators::{PublicKeyDiffStore, WeightDiffStore};
use crate::state::l1_validator::L1Validator;
use crate::state::prefixes;
use crate::state::staker::Staker;
use crate::state::stakers::Stakers;
use crate::txs::Priority;
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
    /// The address → utxoID index nested under the UTXO space (Go
    /// `avax.utxoState.indexDB`): persisted as `addr(20) ‖ utxoID(32) → ()`,
    /// mirrored in [`Self::utxo_index`] for reads.
    utxo_index_db: PrefixDb<D>,
    reward_utxos: PrefixDb<D>,
    subnets: ByteSpace<D>,
    subnet_owners: ByteSpace<D>,
    subnet_managers: ByteSpace<D>,
    chains: PrefixDb<D>,
    singletons: ByteSpace<D>,
    /// Weight/pk-diff parent prefix spaces (M4.14 iterators; the M4.20 block
    /// acceptor writes diffs at the accepted height through these).
    weight_diff_parent: PrefixDb<D>,
    pk_diff_parent: PrefixDb<D>,
    /// Accepted-block byte store (`blockDB`), keyed by block id (M4.20).
    blocks: ByteSpace<D>,
    /// Height → accepted block-id index (`blockIDDB`) (M4.20).
    block_ids: ByteSpace<D>,
    /// Signed-tx byte store (`txDB`), keyed by tx id (M4.20).
    txs: ByteSpace<D>,
    /// Persisted current-staker sublist (`validator/current`), keyed by staker
    /// tx id → its encoded record. Written through by the `put_current_*` /
    /// `delete_current_*` acceptor path and rebuilt into [`Self::current`] by
    /// [`Self::load`] on restart (M9.15 advanced-tip resume — the validator-set
    /// half). The on-disk record is a migration concern, not a consensus/wire
    /// contract (specs 00 §4.4), so it uses a self-describing fixed layout.
    current_stakers_db: PrefixDb<D>,
    /// Persisted pending-staker sublist (`validator/pending`); the pending
    /// counterpart of [`Self::current_stakers_db`].
    pending_stakers_db: PrefixDb<D>,
    /// Persisted ACP-77 L1-validator sublist (`l1Validators/l1Validator`), keyed
    /// by `ValidationID` → the `GenesisCodec`-marshalled [`L1Validator`] value
    /// (the `ValidationID` is the key, not serialized — see
    /// [`L1Validator::marshal`]). Written through by the `put_l1_validator`
    /// acceptor path and rebuilt into [`Self::l1_validators`] by [`Self::load`]
    /// on restart (M9.15 advanced-tip resume — the L1-validator half).
    l1_validators_db: PrefixDb<D>,

    // ----- scalar singletons (in-memory, written through to `singletons`) -----
    timestamp: SystemTime,
    fee_state: GasState,
    l1_validator_excess: u64,
    accrued_fees: u64,
    supply: BTreeMap<Id, u64>,

    // ----- last-accepted block id + height singleton (M4.20) -----
    last_accepted: Id,
    height: u64,

    // ----- in-memory staker / L1-validator collections -----
    current: Stakers,
    pending: Stakers,
    l1_validators: BTreeMap<Id, L1Validator>,

    // ----- in-memory mutable validator staking-info (ACP-236 auto-renew) -----
    // Keyed by `(subnet, node)`. The on-disk codec-v2 persistence of these
    // fields lives in `ValidatorMetadata` (M4.11); the block acceptor's
    // metadata write path (M4.14 / M4.20) flushes them. Here they are tracked
    // in-memory alongside the `current` stakers so the auto-renew executor can
    // mutate them.
    staking_info: BTreeMap<(Id, NodeId), crate::state::metadata_validator::StakingInfo>,

    // ----- in-memory reward-utxo accumulator (keyed by staker tx id) -----
    reward_utxo_index: BTreeMap<Id, Vec<UtxoBytes>>,
    subnet_ids: Vec<Id>,
    chain_index: BTreeMap<Id, Vec<Id>>,

    // ----- in-memory address → utxoID index (mirrors `utxo_index_db`) -----
    utxo_index: BTreeMap<ShortId, BTreeSet<Id>>,

    // ----- in-memory block-id-at-height index (mirrors `block_ids`) -----
    block_id_index: BTreeMap<u64, Id>,

    // ----- base DB handle (retained for cheap read-only snapshots) -----
    base: Arc<D>,
}

impl<D: Database> State<D> {
    /// Builds a `State` over `base`, wiring every §3.2 prefix space.
    ///
    /// # Errors
    /// Returns an error if a prefix space cannot be initialized.
    pub fn new(base: D) -> Result<Self> {
        let base = Arc::new(base);

        let l1_parent = PrefixDb::new_arc(prefixes::L1_VALIDATORS_PREFIX, Arc::clone(&base));
        let validators_parent = PrefixDb::new_arc(prefixes::VALIDATORS_PREFIX, Arc::clone(&base));

        Ok(Self {
            utxos: ByteSpace::new(prefixes::UTXO_PREFIX, &base),
            utxo_index_db: PrefixDb::new_arc(prefixes::UTXO_PREFIX, Arc::clone(&base))
                .join(prefixes::UTXO_INDEX_PREFIX),
            reward_utxos: PrefixDb::new_arc(prefixes::REWARD_UTXOS_PREFIX, Arc::clone(&base)),
            subnets: ByteSpace::new(prefixes::SUBNET_PREFIX, &base),
            subnet_owners: ByteSpace::new(prefixes::SUBNET_OWNER_PREFIX, &base),
            subnet_managers: ByteSpace::new(prefixes::SUBNET_MANAGER_PREFIX, &base),
            chains: PrefixDb::new_arc(prefixes::CHAIN_PREFIX, Arc::clone(&base)),
            singletons: ByteSpace::new(prefixes::SINGLETON_PREFIX, &base),
            weight_diff_parent: l1_parent.join(prefixes::WEIGHT_DIFF_PREFIX),
            pk_diff_parent: l1_parent.join(prefixes::PK_DIFF_PREFIX),
            blocks: ByteSpace::new(prefixes::BLOCK_PREFIX, &base),
            block_ids: ByteSpace::new(prefixes::BLOCK_ID_PREFIX, &base),
            txs: ByteSpace::new(prefixes::TX_PREFIX, &base),
            current_stakers_db: validators_parent.join(prefixes::CURRENT_PREFIX),
            pending_stakers_db: validators_parent.join(prefixes::PENDING_PREFIX),
            l1_validators_db: l1_parent.join(prefixes::L1_VALIDATOR_PREFIX),

            timestamp: UNIX_EPOCH,
            fee_state: GasState::default(),
            l1_validator_excess: 0,
            accrued_fees: 0,
            supply: BTreeMap::new(),

            last_accepted: Id::EMPTY,
            height: 0,

            current: Stakers::new(),
            pending: Stakers::new(),
            l1_validators: BTreeMap::new(),

            staking_info: BTreeMap::new(),

            reward_utxo_index: BTreeMap::new(),
            subnet_ids: Vec::new(),
            chain_index: BTreeMap::new(),

            utxo_index: BTreeMap::new(),

            block_id_index: BTreeMap::new(),

            base,
        })
    }

    /// The persisted index key `addr(20) ‖ utxoID(32)` (Go `utxoState`'s
    /// index entry layout).
    fn utxo_index_key(addr: &ShortId, utxo_id: Id) -> Vec<u8> {
        let mut k = Vec::with_capacity(52);
        k.extend_from_slice(addr.as_bytes());
        k.extend_from_slice(utxo_id.as_bytes());
        k
    }

    /// `avax.UTXOReader.UTXOIDs(addr, previous, limit)` — up to `limit` UTXO
    /// ids referencing `addr`, **strictly greater than** `previous`, in
    /// ascending id order (the `getUTXOs` pagination contract,
    /// `vms/components/avax/utxo_state.go`).
    #[must_use]
    pub fn utxo_ids(&self, addr: &ShortId, previous: Id, limit: usize) -> Vec<Id> {
        let Some(set) = self.utxo_index.get(addr) else {
            return Vec::new();
        };
        set.iter()
            .filter(|id| **id > previous)
            .take(limit)
            .copied()
            .collect()
    }

    /// Index a typed UTXO's addresses (no-op for value bytes that do not
    /// decode — only canonically-marshaled UTXOs enter the set via
    /// [`Chain::add_utxo`]).
    fn index_utxo(&mut self, id: Id, bytes: &[u8]) {
        let Ok(utxo) = crate::utxo::Utxo::unmarshal(bytes) else {
            return;
        };
        for addr in crate::utxo::output_addresses(&utxo.out) {
            self.utxo_index.entry(*addr).or_default().insert(id);
            let _ = self.utxo_index_db.put(&Self::utxo_index_key(addr, id), &[]);
        }
    }

    /// Remove a deleted UTXO's index entries (reads the stored bytes first,
    /// mirroring Go's read-modify-delete in `utxoState.DeleteUTXO`).
    fn unindex_utxo(&mut self, id: Id) {
        let Ok(bytes) = self.utxos.get(id.as_bytes()) else {
            return;
        };
        let Ok(utxo) = crate::utxo::Utxo::unmarshal(&bytes) else {
            return;
        };
        for addr in crate::utxo::output_addresses(&utxo.out) {
            if let Some(set) = self.utxo_index.get_mut(addr) {
                set.remove(&id);
                if set.is_empty() {
                    self.utxo_index.remove(addr);
                }
            }
            let _ = self.utxo_index_db.delete(&Self::utxo_index_key(addr, id));
        }
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

    // ----- block store (`blockDB`) + height index (`blockIDDB`) (M4.20) -----

    /// `GetStatelessBlock` — the stored codec bytes of the accepted block `id`.
    ///
    /// # Errors
    /// Returns [`Error::Database`] wrapping `database.ErrNotFound` when the block
    /// is absent.
    pub fn get_block(&self, id: Id) -> Result<Vec<u8>> {
        self.blocks.get(id.as_bytes())
    }

    /// `AddStatelessBlock` — store an accepted block's codec `bytes` under its
    /// `id` and index its `height → id`.
    pub fn add_block(&mut self, id: Id, height: u64, bytes: &[u8]) {
        let _ = self.blocks.put(id.as_bytes(), bytes);
        let _ = self.block_ids.put(&height.to_be_bytes(), id.as_bytes());
        self.block_id_index.insert(height, id);
    }

    /// `GetBlockIDAtHeight` — the accepted block id at `height`, if any.
    #[must_use]
    pub fn get_block_id_at_height(&self, height: u64) -> Option<Id> {
        self.block_id_index.get(&height).copied()
    }

    // ----- last-accepted / height singleton (M4.20) -----

    /// `GetLastAccepted` — the id of the most-recently accepted block.
    #[must_use]
    pub fn last_accepted(&self) -> Id {
        self.last_accepted
    }

    /// `SetLastAccepted` — record the last-accepted block id (singleton).
    pub fn set_last_accepted(&mut self, id: Id) {
        self.last_accepted = id;
        let _ = self
            .singletons
            .put(prefixes::LAST_ACCEPTED_KEY, id.as_bytes());
    }

    /// `GetHeight` — the height of the most-recently accepted block.
    #[must_use]
    pub fn height(&self) -> u64 {
        self.height
    }

    /// `SetHeight` — record the last-accepted block height (singleton).
    pub fn set_height(&mut self, height: u64) {
        self.height = height;
        self.write_u64_singleton(prefixes::HEIGHT_KEY, height);
    }

    // ----- restart / recovery: resume persisted state from disk (M9.15) -----

    /// `IsInitialized` (cf. Go `state.shouldInit`) — whether this base DB already
    /// holds persisted state. Detected by the presence of the last-accepted
    /// singleton, which `Accept` writes in the same committed batch as the state
    /// diff (specs 27 §5.1), so it is the canonical "already seeded" sentinel: a
    /// fresh DB has no last-accepted, a recovered DB always does.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.singletons.get(prefixes::LAST_ACCEPTED_KEY).is_ok()
    }

    /// Resumes the persisted consensus pointer (last-accepted id + height), the
    /// scalar singletons (timestamp, supply, fee state, L1 excess, accrued fees),
    /// and the block-id-at-height index from the base DB into the in-memory
    /// caches — so a re-opened `State` resumes a previously-persisted (possibly
    /// advanced) tip instead of the genesis defaults `State::new` installs (specs
    /// 27 §5.1: "read `singleton→last accepted`; the base DB is the truth"). A
    /// no-op on a fresh, never-seeded DB (every singleton read misses).
    ///
    /// Also rebuilds the in-memory caches that mirror a write-through byte
    /// space: the current/pending stakers ([`Self::load_stakers`]), the ACP-77
    /// L1-validator set ([`Self::load_l1_validators`]), the subnet set
    /// ([`Self::load_subnets`]), the per-subnet chain index
    /// ([`Self::load_chains`]), and the address → utxo-id index
    /// ([`Self::load_utxo_index`]). Still NOT rebuilt: the reward-utxo index
    /// (keyed under hashed `reward_utxos.join(tx)` sub-spaces with no
    /// enumerable tx-id set on disk) — a separate follow-up.
    ///
    /// # Errors
    /// Returns [`Error::CorruptState`] if a persisted singleton or block-index
    /// entry is present but has an unexpected byte length, or [`Error::Database`]
    /// on an iterator error (never on a missing key).
    pub fn load(&mut self) -> Result<()> {
        if let Some(bytes) = self.read_singleton(prefixes::LAST_ACCEPTED_KEY)? {
            self.last_accepted = Id::from_slice(&bytes)
                .map_err(|_| Error::CorruptState("last-accepted id length"))?;
        }
        if let Some(h) = self.read_u64_singleton(prefixes::HEIGHT_KEY)? {
            self.height = h;
        }
        if let Some(secs) = self.read_u64_singleton(prefixes::TIMESTAMP_KEY)? {
            self.timestamp = UNIX_EPOCH
                .checked_add(Duration::from_secs(secs))
                .ok_or(Error::CorruptState("timestamp overflow"))?;
        }
        if let Some(excess) = self.read_u64_singleton(prefixes::L1_VALIDATOR_EXCESS_KEY)? {
            self.l1_validator_excess = excess;
        }
        if let Some(fees) = self.read_u64_singleton(prefixes::ACCRUED_FEES_KEY)? {
            self.accrued_fees = fees;
        }
        if let Some(buf) = self.read_singleton(prefixes::FEE_STATE_KEY)? {
            // 16 bytes: capacity(8 BE) ‖ excess(8 BE) — see `set_fee_state`.
            let buf: [u8; 16] = buf
                .as_slice()
                .try_into()
                .map_err(|_| Error::CorruptState("fee-state length"))?;
            let (cap, exc) = buf.split_at(8);
            self.fee_state = GasState {
                capacity: u64::from_be_bytes(
                    cap.try_into()
                        .map_err(|_| Error::CorruptState("fee-state capacity"))?,
                ),
                excess: u64::from_be_bytes(
                    exc.try_into()
                        .map_err(|_| Error::CorruptState("fee-state excess"))?,
                ),
            };
        }
        self.load_supply()?;
        self.load_block_id_index()?;
        self.load_stakers()?;
        self.load_l1_validators()?;
        self.load_subnets()?;
        self.load_chains()?;
        self.load_utxo_index()?;
        Ok(())
    }

    /// Reads a singleton's raw bytes, mapping a missing key to `None` (so a fresh
    /// DB resumes nothing) and propagating any other DB error.
    fn read_singleton(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.singletons.get(key) {
            Ok(v) => Ok(Some(v)),
            Err(Error::Database(DbError::NotFound)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Reads an 8-byte big-endian `u64` singleton (`None` if absent).
    fn read_u64_singleton(&self, key: &[u8]) -> Result<Option<u64>> {
        match self.read_singleton(key)? {
            Some(bytes) => {
                let arr: [u8; 8] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::CorruptState("u64 singleton length"))?;
                Ok(Some(u64::from_be_bytes(arr)))
            }
            None => Ok(None),
        }
    }

    /// Rebuilds the in-memory supply map from the persisted `current supply`
    /// singletons (the primary network's bare key plus any per-subnet suffixed
    /// keys).
    fn load_supply(&mut self) -> Result<()> {
        let prefix = prefixes::CURRENT_SUPPLY_KEY;
        let mut entries: Vec<(Id, u64)> = Vec::new();
        {
            let mut it = self
                .singletons
                .db
                .new_iterator_with_start_and_prefix(&[], prefix);
            while it.next() {
                let (Some(k), Some(v)) = (it.key(), it.value()) else {
                    continue;
                };
                // `k` is singleton-space-relative and starts with `prefix`; the
                // suffix is the subnet id (empty for the primary network).
                let suffix = k.get(prefix.len()..).unwrap_or(&[]);
                let subnet = if suffix.is_empty() {
                    Id::EMPTY
                } else {
                    Id::from_slice(suffix)
                        .map_err(|_| Error::CorruptState("supply subnet id length"))?
                };
                let amount: [u8; 8] = v
                    .try_into()
                    .map_err(|_| Error::CorruptState("supply value length"))?;
                entries.push((subnet, u64::from_be_bytes(amount)));
            }
            it.error().map_err(Error::Database)?;
            it.release();
        }
        for (subnet, amount) in entries {
            self.supply.insert(subnet, amount);
        }
        Ok(())
    }

    /// Rebuilds the in-memory `height → accepted block id` index from the
    /// persisted `blockIDDB` space.
    fn load_block_id_index(&mut self) -> Result<()> {
        let mut entries: Vec<(u64, Id)> = Vec::new();
        {
            let mut it = self
                .block_ids
                .db
                .new_iterator_with_start_and_prefix(&[], &[]);
            while it.next() {
                let (Some(k), Some(v)) = (it.key(), it.value()) else {
                    continue;
                };
                let height: [u8; 8] = k
                    .try_into()
                    .map_err(|_| Error::CorruptState("block-index height key length"))?;
                let id =
                    Id::from_slice(v).map_err(|_| Error::CorruptState("block-index id length"))?;
                entries.push((u64::from_be_bytes(height), id));
            }
            it.error().map_err(Error::Database)?;
            it.release();
        }
        for (height, id) in entries {
            self.block_id_index.insert(height, id);
        }
        Ok(())
    }

    /// Rebuilds the in-memory subnet set ([`Self::subnet_ids`]) from the
    /// persisted `subnets` byte space (each key is a 32-byte subnet id). The
    /// space is flat (not joined), so a single scan suffices.
    fn load_subnets(&mut self) -> Result<()> {
        let mut ids: Vec<Id> = Vec::new();
        {
            let mut it = self.subnets.db.new_iterator_with_start_and_prefix(&[], &[]);
            while it.next() {
                let Some(k) = it.key() else { continue };
                let id = Id::from_slice(k).map_err(|_| Error::CorruptState("subnet id length"))?;
                ids.push(id);
            }
            it.error().map_err(Error::Database)?;
            it.release();
        }
        for id in ids {
            if !self.subnet_ids.contains(&id) {
                self.subnet_ids.push(id);
            }
        }
        Ok(())
    }

    /// Rebuilds the in-memory `subnet → chains` index ([`Self::chain_index`])
    /// from the persisted `chains` space. Each subnet's chains live under the
    /// **hashed** sub-space `chains.join(subnet)` (the join compresses to a
    /// SHA-256 prefix, so the parent space is not flat-scannable), so this
    /// enumerates per subnet over the resumed [`Self::subnet_ids`] plus the
    /// primary network (genesis chains are recorded under it). Must run after
    /// [`Self::load_subnets`].
    fn load_chains(&mut self) -> Result<()> {
        let mut subnets: Vec<Id> = self.subnet_ids.clone();
        subnets.push(PRIMARY_NETWORK_ID);
        for subnet in subnets {
            let space = self.chains.join(subnet.as_bytes());
            let mut chain_ids: Vec<Id> = Vec::new();
            {
                let mut it = space.new_iterator_with_start_and_prefix(&[], &[]);
                while it.next() {
                    let Some(k) = it.key() else { continue };
                    let id =
                        Id::from_slice(k).map_err(|_| Error::CorruptState("chain id length"))?;
                    chain_ids.push(id);
                }
                it.error().map_err(Error::Database)?;
                it.release();
            }
            if !chain_ids.is_empty() {
                let list = self.chain_index.entry(subnet).or_default();
                for id in chain_ids {
                    if !list.contains(&id) {
                        list.push(id);
                    }
                }
            }
        }
        Ok(())
    }

    /// Rebuilds the in-memory address → utxo-id index ([`Self::utxo_index`])
    /// from the persisted `utxo_index_db` space (flat keys `addr(20) ‖
    /// utxoID(32)`, mirroring [`Self::utxo_index_key`]), so `getUTXOs` /
    /// [`Self::utxo_ids`] resolve after a restart.
    fn load_utxo_index(&mut self) -> Result<()> {
        let mut entries: Vec<(ShortId, Id)> = Vec::new();
        {
            let mut it = self
                .utxo_index_db
                .new_iterator_with_start_and_prefix(&[], &[]);
            while it.next() {
                let Some(k) = it.key() else { continue };
                let addr_bytes = k
                    .get(..20)
                    .ok_or(Error::CorruptState("utxo-index key length"))?;
                let id_bytes = k
                    .get(20..)
                    .ok_or(Error::CorruptState("utxo-index key length"))?;
                let addr = ShortId::from_slice(addr_bytes)
                    .map_err(|_| Error::CorruptState("utxo-index addr length"))?;
                let id = Id::from_slice(id_bytes)
                    .map_err(|_| Error::CorruptState("utxo-index id length"))?;
                entries.push((addr, id));
            }
            it.error().map_err(Error::Database)?;
            it.release();
        }
        for (addr, id) in entries {
            self.utxo_index.entry(addr).or_default().insert(id);
        }
        Ok(())
    }

    // ----- staker disk codec + resume (M9.15 advanced-tip resume) -----

    /// The fixed-width prefix of an encoded staker record (everything before the
    /// optional BLS public key): `txID(32) ‖ nodeID(20) ‖ subnetID(32) ‖
    /// weight(8) ‖ start(8) ‖ end(8) ‖ potentialReward(8) ‖ nextTime(8) ‖
    /// priority(1) ‖ pkPresent(1)` = 126 bytes.
    const STAKER_FIXED_LEN: usize = 32 + 20 + 32 + 8 + 8 + 8 + 8 + 8 + 1 + 1;

    /// Unix-seconds since the epoch for `t` (saturating at the epoch for any
    /// pre-epoch time, matching [`set_timestamp`](Self::set_timestamp)).
    fn staker_secs(t: SystemTime) -> u64 {
        t.duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
    }

    /// `UNIX_EPOCH + secs`, or [`Error::CorruptState`] on overflow.
    fn staker_time(secs: u64) -> Result<SystemTime> {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(secs))
            .ok_or(Error::CorruptState("staker time overflow"))
    }

    /// Encodes a [`Staker`] to its on-disk record (the self-describing fixed
    /// layout documented on [`Self::current_stakers_db`]). The optional BLS key
    /// is a 1-byte present flag followed by the 48-byte compressed key when set.
    fn encode_staker(s: &Staker) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::STAKER_FIXED_LEN + bls::PUBLIC_KEY_LEN);
        buf.extend_from_slice(s.tx_id.as_bytes());
        buf.extend_from_slice(s.node_id.as_bytes());
        buf.extend_from_slice(s.subnet_id.as_bytes());
        buf.extend_from_slice(&s.weight.to_be_bytes());
        buf.extend_from_slice(&Self::staker_secs(s.start_time).to_be_bytes());
        buf.extend_from_slice(&Self::staker_secs(s.end_time).to_be_bytes());
        buf.extend_from_slice(&s.potential_reward.to_be_bytes());
        buf.extend_from_slice(&Self::staker_secs(s.next_time).to_be_bytes());
        buf.push(s.priority.as_u8());
        match &s.public_key {
            Some(pk) => {
                buf.push(1);
                buf.extend_from_slice(&pk.compress());
            }
            None => buf.push(0),
        }
        buf
    }

    /// Decodes an on-disk staker record written by [`Self::encode_staker`].
    /// Defensive against truncation/garbage (the base DB is untrusted on
    /// recovery): every field read is length-checked into [`Error::CorruptState`].
    fn decode_staker(buf: &[u8]) -> Result<Staker> {
        let field = |start: usize, len: usize| -> Result<&[u8]> {
            buf.get(start..start.saturating_add(len))
                .ok_or(Error::CorruptState("staker record truncated"))
        };
        let be_u64 = |start: usize| -> Result<u64> {
            let arr: [u8; 8] = field(start, 8)?
                .try_into()
                .map_err(|_| Error::CorruptState("staker u64 field"))?;
            Ok(u64::from_be_bytes(arr))
        };

        let tx_id =
            Id::from_slice(field(0, 32)?).map_err(|_| Error::CorruptState("staker tx id"))?;
        let node_id = NodeId::from_slice(field(32, 20)?)
            .map_err(|_| Error::CorruptState("staker node id"))?;
        let subnet_id =
            Id::from_slice(field(52, 32)?).map_err(|_| Error::CorruptState("staker subnet id"))?;
        let weight = be_u64(84)?;
        let start_time = Self::staker_time(be_u64(92)?)?;
        let end_time = Self::staker_time(be_u64(100)?)?;
        let potential_reward = be_u64(108)?;
        let next_time = Self::staker_time(be_u64(116)?)?;
        let priority_byte = *field(124, 1)?
            .first()
            .ok_or(Error::CorruptState("staker priority byte"))?;
        let priority =
            Priority::from_u8(priority_byte).ok_or(Error::CorruptState("staker priority byte"))?;
        let pk_present = *field(125, 1)?
            .first()
            .ok_or(Error::CorruptState("staker pk-present flag"))?;
        let public_key = match pk_present {
            0 => None,
            1 => Some(
                bls::PublicKey::from_compressed(field(
                    Self::STAKER_FIXED_LEN,
                    bls::PUBLIC_KEY_LEN,
                )?)
                .map_err(|_| Error::CorruptState("staker public key"))?,
            ),
            _ => return Err(Error::CorruptState("staker pk-present flag")),
        };

        Ok(Staker {
            tx_id,
            node_id,
            public_key,
            subnet_id,
            weight,
            start_time,
            end_time,
            potential_reward,
            next_time,
            priority,
        })
    }

    /// Reads and decodes every staker record from a sublist `db`, in iteration
    /// (key) order.
    fn read_staker_sublist(db: &PrefixDb<D>) -> Result<Vec<Staker>> {
        let mut out: Vec<Staker> = Vec::new();
        let mut it = db.new_iterator_with_start_and_prefix(&[], &[]);
        while it.next() {
            let Some(v) = it.value() else { continue };
            out.push(Self::decode_staker(v)?);
        }
        it.error().map_err(Error::Database)?;
        it.release();
        Ok(out)
    }

    /// Rebuilds the in-memory current & pending [`Stakers`] collections from the
    /// persisted staker sublists, dispatching each record to the validator or
    /// delegator slot by its [`Priority`]. Validators also seed the mutable
    /// `staking_info` default, mirroring [`put_current_validator`](Self::put_current_validator).
    fn load_stakers(&mut self) -> Result<()> {
        for s in Self::read_staker_sublist(&self.current_stakers_db)? {
            if s.priority.is_validator() {
                self.staking_info
                    .entry((s.subnet_id, s.node_id))
                    .or_default();
                let _ = self.current.put_validator(s);
            } else {
                self.current.put_delegator(s);
            }
        }
        for s in Self::read_staker_sublist(&self.pending_stakers_db)? {
            if s.priority.is_validator() {
                let _ = self.pending.put_validator(s);
            } else {
                self.pending.put_delegator(s);
            }
        }
        Ok(())
    }

    /// Rebuilds the in-memory ACP-77 L1-validator map ([`Self::l1_validators`])
    /// from the persisted `l1Validators/l1Validator` sublist. Each entry's key
    /// is the `ValidationID` (not serialized in the value), so the decoded
    /// record's [`validation_id`](L1Validator::validation_id) is restored from
    /// the DB key before insertion. Defensive against truncation/garbage (the
    /// base DB is untrusted on recovery): a bad key width or codec failure is an
    /// [`Error::CorruptState`] / [`Error::Codec`].
    fn load_l1_validators(&mut self) -> Result<()> {
        let mut entries: Vec<(Id, L1Validator)> = Vec::new();
        {
            let mut it = self
                .l1_validators_db
                .new_iterator_with_start_and_prefix(&[], &[]);
            while it.next() {
                let Some(k) = it.key() else { continue };
                let Some(v) = it.value() else { continue };
                let validation_id =
                    Id::from_slice(k).map_err(|_| Error::CorruptState("l1 validator id length"))?;
                let mut record = L1Validator::unmarshal(v)?;
                record.validation_id = validation_id;
                entries.push((validation_id, record));
            }
            it.error().map_err(Error::Database)?;
            it.release();
        }
        for (id, record) in entries {
            self.l1_validators.insert(id, record);
        }
        Ok(())
    }

    // ----- staker weight/pk-diff stores (M4.14/M4.20) -----

    /// The persisted staker weight-diff store
    /// ([`WeightDiffStore`](super::diff_iterators::WeightDiffStore)).
    #[must_use]
    pub fn weight_diff_store(&self) -> WeightDiffStore<D> {
        WeightDiffStore::new(&self.weight_diff_parent)
    }

    /// The persisted staker public-key-diff store
    /// ([`PublicKeyDiffStore`](super::diff_iterators::PublicKeyDiffStore)).
    #[must_use]
    pub fn public_key_diff_store(&self) -> PublicKeyDiffStore<D> {
        PublicKeyDiffStore::new(&self.pk_diff_parent)
    }

    /// A snapshot of the total current-validator-set weight per `(subnet, node)`
    /// — the sum of the node's current validator weight and all of its current
    /// delegators' weights — used by the acceptor to compute the per-height
    /// weight diffs (Go `calculateValidatorDiffs` → `diffValidator.WeightDiff`).
    ///
    /// The block acceptor snapshots this before applying a block's
    /// [`Diff`](super::diff::Diff), applies it, then re-snapshots and writes the
    /// deltas through [`weight_diff_store`](Self::weight_diff_store).
    #[must_use]
    pub fn current_validator_weights(&self) -> BTreeMap<(Id, NodeId), u64> {
        let mut out: BTreeMap<(Id, NodeId), u64> = BTreeMap::new();
        for s in self.current.iter() {
            if s.priority.is_current() {
                let entry = out.entry((s.subnet_id, s.node_id)).or_insert(0);
                *entry = entry.saturating_add(s.weight);
            }
        }
        out
    }

    /// A cheap **read-consistent snapshot** of this state as an immutable
    /// [`Arc<dyn Chain>`], for use as a [`Diff`](super::diff::Diff) parent through
    /// [`Versions`](super::chain::Versions).
    ///
    /// The snapshot shares the same underlying [`Database`] handle (so byte-valued
    /// spaces read the same data) and clones the in-memory scalar/staker/index
    /// fields, so subsequent mutations to `self` are not visible through it
    /// (matching Go, where a verified block's diff parent is a frozen view).
    #[must_use]
    pub fn snapshot(&self) -> Arc<dyn Chain>
    where
        D: 'static,
    {
        let base = Arc::clone(&self.base);
        let l1_parent = PrefixDb::new_arc(prefixes::L1_VALIDATORS_PREFIX, Arc::clone(&base));
        let validators_parent = PrefixDb::new_arc(prefixes::VALIDATORS_PREFIX, Arc::clone(&base));
        Arc::new(State {
            utxos: ByteSpace::new(prefixes::UTXO_PREFIX, &base),
            utxo_index_db: PrefixDb::new_arc(prefixes::UTXO_PREFIX, Arc::clone(&base))
                .join(prefixes::UTXO_INDEX_PREFIX),
            reward_utxos: PrefixDb::new_arc(prefixes::REWARD_UTXOS_PREFIX, Arc::clone(&base)),
            subnets: ByteSpace::new(prefixes::SUBNET_PREFIX, &base),
            subnet_owners: ByteSpace::new(prefixes::SUBNET_OWNER_PREFIX, &base),
            subnet_managers: ByteSpace::new(prefixes::SUBNET_MANAGER_PREFIX, &base),
            chains: PrefixDb::new_arc(prefixes::CHAIN_PREFIX, Arc::clone(&base)),
            singletons: ByteSpace::new(prefixes::SINGLETON_PREFIX, &base),
            weight_diff_parent: l1_parent.join(prefixes::WEIGHT_DIFF_PREFIX),
            pk_diff_parent: l1_parent.join(prefixes::PK_DIFF_PREFIX),
            blocks: ByteSpace::new(prefixes::BLOCK_PREFIX, &base),
            block_ids: ByteSpace::new(prefixes::BLOCK_ID_PREFIX, &base),
            txs: ByteSpace::new(prefixes::TX_PREFIX, &base),
            current_stakers_db: validators_parent.join(prefixes::CURRENT_PREFIX),
            pending_stakers_db: validators_parent.join(prefixes::PENDING_PREFIX),
            l1_validators_db: l1_parent.join(prefixes::L1_VALIDATOR_PREFIX),

            timestamp: self.timestamp,
            fee_state: self.fee_state,
            l1_validator_excess: self.l1_validator_excess,
            accrued_fees: self.accrued_fees,
            supply: self.supply.clone(),

            last_accepted: self.last_accepted,
            height: self.height,

            current: self.current.clone(),
            pending: self.pending.clone(),
            l1_validators: self.l1_validators.clone(),

            staking_info: self.staking_info.clone(),

            reward_utxo_index: self.reward_utxo_index.clone(),
            subnet_ids: self.subnet_ids.clone(),
            chain_index: self.chain_index.clone(),

            utxo_index: self.utxo_index.clone(),

            block_id_index: self.block_id_index.clone(),

            base,
        })
    }

    /// A snapshot of current validators' uncompressed BLS public-key bytes keyed
    /// by `(subnet, node)` (only current validators that carry a key), used by the
    /// acceptor to compute per-height public-key diffs. The bytes are the
    /// uncompressed form Go stores (`PublicKeyToUncompressedBytes`).
    #[must_use]
    pub fn current_validator_public_keys(&self) -> BTreeMap<(Id, NodeId), Vec<u8>> {
        let mut out = BTreeMap::new();
        for s in self.current.iter() {
            if let (true, Some(pk)) = (s.priority.is_current_validator(), &s.public_key) {
                out.insert((s.subnet_id, s.node_id), pk.serialize().to_vec());
            }
        }
        out
    }

    /// The current validator set per subnet — the in-memory base view the
    /// [`PChainValidatorManager`](crate::validators::manager::PChainValidatorManager)
    /// un-applies diffs over (M4.21, Go `cfg.Validators.GetMap`). Keyed
    /// subnet → node, each entry carries the node's **total** current weight
    /// (its validator weight plus all of its current delegators' weights) and
    /// the validator's uncompressed BLS public-key bytes (`None` if it has no
    /// key). Only nodes that have a current *validator* are included; a lone
    /// current delegator does not constitute a validator.
    ///
    /// The returned maps are `BTreeMap`s, so iteration is canonically subnet-
    /// then-`NodeId`-ascending (the windower/Warp determinism contract).
    #[must_use]
    pub fn current_validator_sets(
        &self,
    ) -> BTreeMap<Id, BTreeMap<NodeId, super::diff_iterators::DiffValidator>> {
        use super::diff_iterators::DiffValidator;

        // Sum total weight per (subnet, node) across all current stakers
        // (validators + delegators), matching `current_validator_weights`.
        let weights = self.current_validator_weights();
        // The validators' uncompressed public-key bytes.
        let keys = self.current_validator_public_keys();

        let mut out: BTreeMap<Id, BTreeMap<NodeId, DiffValidator>> = BTreeMap::new();
        for s in self.current.iter() {
            if !s.priority.is_current_validator() {
                continue;
            }
            let key = (s.subnet_id, s.node_id);
            let weight = weights.get(&key).copied().unwrap_or(s.weight);
            let public_key = keys.get(&key).cloned();
            out.entry(s.subnet_id)
                .or_default()
                .insert(s.node_id, DiffValidator { weight, public_key });
        }
        out
    }

    /// The accepted-block `height → id` index (`blockIDDB` mirror), used by the
    /// validator manager's `get_minimum_height` to recover the height of a
    /// recently-accepted block (M4.21).
    #[must_use]
    pub fn block_id_index(&self) -> &BTreeMap<u64, Id> {
        &self.block_id_index
    }

    /// A clone of the base [`Database`] handle, for building read-only stores
    /// (e.g. the staker diff stores) that outlive a borrow of `self`.
    #[must_use]
    pub fn base(&self) -> Arc<D> {
        Arc::clone(&self.base)
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
        self.index_utxo(id, &utxo);
        let _ = self.utxos.put(id.as_bytes(), &utxo);
    }

    fn delete_utxo(&mut self, id: Id) {
        self.unindex_utxo(id);
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
        // Seed the mutable staking info with the zero value so that reads through
        // `get_staking_info` before a flush observe a default (Go
        // `State.PutCurrentValidator` seeds `modifiedStakingInfo`).
        self.staking_info
            .entry((s.subnet_id, s.node_id))
            .or_default();
        let tx_id = s.tx_id;
        let bytes = Self::encode_staker(&s);
        // A validator replaced under a different tx id leaves an orphaned disk
        // key — drop it so the sublist mirrors the in-memory set.
        if let Some(prev) = self
            .current
            .put_validator(s)
            .filter(|prev| prev.tx_id != tx_id)
        {
            let _ = self.current_stakers_db.delete(prev.tx_id.as_bytes());
        }
        let _ = self.current_stakers_db.put(tx_id.as_bytes(), &bytes);
        Ok(())
    }

    fn delete_current_validator(&mut self, s: &Staker) {
        self.staking_info.remove(&(s.subnet_id, s.node_id));
        self.current.delete_validator(s);
        let _ = self.current_stakers_db.delete(s.tx_id.as_bytes());
    }

    fn put_current_delegator(&mut self, s: Staker) {
        let _ = self
            .current_stakers_db
            .put(s.tx_id.as_bytes(), &Self::encode_staker(&s));
        self.current.put_delegator(s);
    }

    fn delete_current_delegator(&mut self, s: &Staker) {
        self.current.delete_delegator(s);
        let _ = self.current_stakers_db.delete(s.tx_id.as_bytes());
    }

    fn current_stakers(&self) -> Vec<Staker> {
        self.current.to_vec()
    }

    fn get_staking_info(
        &self,
        subnet: Id,
        node: NodeId,
    ) -> Result<crate::state::metadata_validator::StakingInfo> {
        // The validator must exist (Go `State.GetStakingInfo` first reads the
        // current validator and surfaces its error).
        self.get_current_validator(subnet, node)?;
        Ok(self
            .staking_info
            .get(&(subnet, node))
            .copied()
            .unwrap_or_default())
    }

    fn set_staking_info(
        &mut self,
        subnet: Id,
        node: NodeId,
        info: crate::state::metadata_validator::StakingInfo,
    ) -> Result<()> {
        self.get_current_validator(subnet, node)?;
        self.staking_info.insert((subnet, node), info);
        Ok(())
    }

    fn put_pending_validator(&mut self, s: Staker) -> Result<()> {
        if !s.priority.is_pending() {
            return Err(Error::WrongTxType);
        }
        let tx_id = s.tx_id;
        let bytes = Self::encode_staker(&s);
        if let Some(prev) = self
            .pending
            .put_validator(s)
            .filter(|prev| prev.tx_id != tx_id)
        {
            let _ = self.pending_stakers_db.delete(prev.tx_id.as_bytes());
        }
        let _ = self.pending_stakers_db.put(tx_id.as_bytes(), &bytes);
        Ok(())
    }

    fn delete_pending_validator(&mut self, s: &Staker) {
        self.pending.delete_validator(s);
        let _ = self.pending_stakers_db.delete(s.tx_id.as_bytes());
    }

    fn put_pending_delegator(&mut self, s: Staker) {
        let _ = self
            .pending_stakers_db
            .put(s.tx_id.as_bytes(), &Self::encode_staker(&s));
        self.pending.put_delegator(s);
    }

    fn delete_pending_delegator(&mut self, s: &Staker) {
        self.pending.delete_delegator(s);
        let _ = self.pending_stakers_db.delete(s.tx_id.as_bytes());
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
        // Write through to the disk sublist (keyed by ValidationID — stable, so a
        // re-put overwrites the same key with no orphan) so [`Self::load`] can
        // rebuild the in-memory map on restart. The value omits the ValidationID
        // (it is the key); `marshal` uses the `GenesisCodec`, matching Go
        // `putL1Validator`.
        let bytes = v.marshal()?;
        let _ = self
            .l1_validators_db
            .put(v.validation_id.as_bytes(), &bytes);
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

    fn active_l1_validators(&self) -> Vec<L1Validator> {
        let mut out: Vec<L1Validator> = self
            .l1_validators
            .values()
            .filter(|v| v.is_active())
            .cloned()
            .collect();
        out.sort_by(L1Validator::compare);
        out
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

    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>> {
        self.txs.get(tx_id.as_bytes())
    }

    fn add_tx(&mut self, tx_id: Id, tx_bytes: Vec<u8>) {
        let _ = self.txs.put(tx_id.as_bytes(), &tx_bytes);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use ava_database::MemDb;
    use ava_secp256k1fx::{OutputOwners, TransferOutput};

    use crate::txs::components::Output;
    use crate::utxo::Utxo;

    #[test]
    fn empty_supply_is_zero() {
        let s = State::new(MemDb::new()).expect("state");
        assert_eq!(s.current_supply(Id::EMPTY).expect("supply"), 0);
    }

    fn utxo(tx: u8, index: u32, addrs: &[ShortId]) -> Utxo {
        Utxo {
            tx_id: Id::from([tx; 32]),
            output_index: index,
            asset_id: Id::from([0x42; 32]),
            out: Output::Transfer(TransferOutput::new(
                1_000,
                OutputOwners::new(0, 1, addrs.to_vec()),
            )),
        }
    }

    /// `avax.utxoState` index parity: `add_utxo` indexes every owning address,
    /// `delete_utxo` removes the entries, and `utxo_ids` paginates strictly
    /// after `previous` in ascending id order.
    #[test]
    fn utxo_address_index_add_delete_paginate() {
        let addr_a = ShortId::from_slice(&[0x0A; 20]).expect("addr");
        let addr_b = ShortId::from_slice(&[0x0B; 20]).expect("addr");

        let mut s = State::new(MemDb::new()).expect("state");
        let u1 = utxo(0x01, 0, &[addr_a]);
        let u2 = utxo(0x02, 0, &[addr_a, addr_b]);
        let u3 = utxo(0x03, 0, &[addr_b]);
        for u in [&u1, &u2, &u3] {
            s.add_utxo(u.input_id(), u.marshal().expect("marshal utxo"));
        }

        // addr_a sees u1+u2; addr_b sees u2+u3 (ascending utxo-id order).
        let mut a_ids = vec![u1.input_id(), u2.input_id()];
        a_ids.sort();
        assert_eq!(
            s.utxo_ids(&addr_a, Id::EMPTY, usize::MAX),
            a_ids,
            "addr_a index"
        );
        let mut b_ids = vec![u2.input_id(), u3.input_id()];
        b_ids.sort();
        assert_eq!(
            s.utxo_ids(&addr_b, Id::EMPTY, usize::MAX),
            b_ids,
            "addr_b index"
        );

        // Pagination: previous is exclusive; limit truncates.
        assert_eq!(
            s.utxo_ids(&addr_a, a_ids[0], usize::MAX),
            vec![a_ids[1]],
            "previous is exclusive"
        );
        assert_eq!(s.utxo_ids(&addr_a, Id::EMPTY, 1), vec![a_ids[0]], "limit");

        // Deleting u2 removes it from both addresses.
        s.delete_utxo(u2.input_id());
        assert_eq!(
            s.utxo_ids(&addr_a, Id::EMPTY, usize::MAX),
            vec![u1.input_id()]
        );
        assert_eq!(
            s.utxo_ids(&addr_b, Id::EMPTY, usize::MAX),
            vec![u3.input_id()]
        );

        // Unknown address: empty.
        let addr_c = ShortId::from_slice(&[0x0C; 20]).expect("addr");
        assert!(s.utxo_ids(&addr_c, Id::EMPTY, usize::MAX).is_empty());
    }

    /// A never-seeded base DB is not initialized; `load()` over it is a harmless
    /// no-op leaving the genesis defaults (the engine then seeds genesis).
    #[test]
    fn fresh_db_is_not_initialized_and_load_is_a_noop() {
        let shared: std::sync::Arc<dyn ava_database::DynDatabase> =
            std::sync::Arc::new(MemDb::new());
        let mut s =
            State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared))).expect("State::new");
        assert!(
            !s.is_initialized(),
            "is_initialized() over a fresh db (no persisted last-accepted)"
        );
        s.load().expect("load() over fresh db");
        assert_eq!(
            s.last_accepted(),
            Id::EMPTY,
            "load() leaves genesis default LA"
        );
        assert_eq!(s.height(), 0, "load() leaves genesis default height");
    }

    /// Re-opening a `State` over a base DB that already holds an **advanced**
    /// accepted tip resumes the persisted last-accepted pointer, height, block
    /// index, and the scalar singletons — it does NOT reset to genesis defaults
    /// (spec 27 §5.1: read `singleton→last accepted`; the base DB is the truth).
    #[test]
    fn reopen_resumes_persisted_advanced_tip_not_genesis_defaults() {
        let shared: std::sync::Arc<dyn ava_database::DynDatabase> =
            std::sync::Arc::new(MemDb::new());

        // First "process": persist an advanced, accepted tip + scalar state.
        let block_id = Id::from([0x7C; 32]);
        let block_bytes = vec![0xAB, 0xCD, 0xEF];
        {
            let mut a = State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared)))
                .expect("State::new A");
            a.set_timestamp(UNIX_EPOCH + Duration::from_secs(1_700_000_123));
            a.set_current_supply(Id::EMPTY, 999_000);
            a.set_fee_state(GasState {
                capacity: 4_321,
                excess: 8_765,
            });
            a.set_l1_validator_excess(55);
            a.set_accrued_fees(77);
            a.add_block(block_id, 5, &block_bytes);
            a.set_last_accepted(block_id);
            a.set_height(5);
        } // drop A — the in-memory view is gone; the base DB survives the restart.

        // Second "process": a fresh State over the SAME backend (the real restart
        // shape — a new process, the same on-disk backend).
        let mut b = State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared)))
            .expect("State::new B");

        // Before load: the bug this closes — the populated db is detected as
        // initialized, but the in-memory caches are still genesis defaults.
        assert!(
            b.is_initialized(),
            "is_initialized() reads persisted last-accepted"
        );
        assert_eq!(
            b.last_accepted(),
            Id::EMPTY,
            "fresh State has not loaded yet"
        );
        assert_eq!(b.height(), 0, "fresh State has not loaded yet");

        // Load resumes the persisted advanced tip.
        b.load().expect("load()");
        assert_eq!(b.last_accepted(), block_id, "resumed last-accepted pointer");
        assert_eq!(b.height(), 5, "resumed height");
        assert_eq!(
            b.get_block_id_at_height(5),
            Some(block_id),
            "resumed block-id-at-height index"
        );
        assert_eq!(
            b.get_block(block_id).expect("get_block"),
            block_bytes,
            "block bytes persist across the restart"
        );
        assert_eq!(
            b.current_supply(Id::EMPTY).expect("supply"),
            999_000,
            "resumed primary-network supply singleton"
        );
        assert_eq!(
            b.timestamp(),
            UNIX_EPOCH + Duration::from_secs(1_700_000_123),
            "resumed timestamp singleton"
        );
        assert_eq!(
            b.fee_state(),
            GasState {
                capacity: 4_321,
                excess: 8_765,
            },
            "resumed fee-state singleton"
        );
        assert_eq!(b.l1_validator_excess(), 55, "resumed L1 excess singleton");
        assert_eq!(b.accrued_fees(), 77, "resumed accrued-fees singleton");
    }

    /// Re-opening a `State` over a base DB that persisted current & pending
    /// stakers resumes the full current/pending validator sets (the
    /// validator-set half of advanced-tip resume): `put_*` writes each staker
    /// through to its disk sublist, and `load()` rebuilds the in-memory
    /// [`Stakers`] collections so a recovered node sees its validators rather
    /// than an empty set. Mirrors the singleton/block-index resume above.
    #[test]
    fn reopen_resumes_persisted_stakers() {
        use ava_crypto::bls;

        use crate::txs::Priority;

        let shared: std::sync::Arc<dyn ava_database::DynDatabase> =
            std::sync::Arc::new(MemDb::new());

        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let t1 = UNIX_EPOCH + Duration::from_secs(1_700_009_999);

        // A primary-network current validator carrying a BLS key, a primary
        // current delegator with no key, and a permissioned subnet pending
        // validator — exercises both `Option<PublicKey>` arms and the
        // current/pending + validator/delegator dispatch.
        let pk = bls::SecretKey::from_bytes(&[7u8; 32])
            .expect("sk")
            .public_key();
        let subnet = Id::from([0x5B; 32]);
        let cur_validator = Staker::new_current(
            Id::from([0x01; 32]),
            NodeId::from([0xAA; 20]),
            Some(pk),
            Id::EMPTY,
            1_000,
            t0,
            t1,
            500,
            Priority::PrimaryNetworkValidatorCurrent,
        );
        let cur_delegator = Staker::new_current(
            Id::from([0x02; 32]),
            NodeId::from([0xBB; 20]),
            None,
            Id::EMPTY,
            250,
            t0,
            t1,
            7,
            Priority::PrimaryNetworkDelegatorCurrent,
        );
        let pend_validator = Staker::new_pending(
            Id::from([0x03; 32]),
            NodeId::from([0xCC; 20]),
            None,
            subnet,
            42,
            t0,
            t1,
            Priority::SubnetPermissionedValidatorPending,
        );

        // First "process": persist the stakers + an advanced tip (so the
        // re-open path is the recovery shape, not a fresh seed).
        {
            let mut a = State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared)))
                .expect("State::new A");
            a.put_current_validator(cur_validator.clone())
                .expect("put current validator");
            a.put_current_delegator(cur_delegator.clone());
            a.put_pending_validator(pend_validator.clone())
                .expect("put pending validator");
            a.set_last_accepted(Id::from([0x7C; 32]));
            a.set_height(9);
        } // drop A — only the base DB survives.

        // Second "process": fresh State over the SAME backend.
        let mut b = State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared)))
            .expect("State::new B");
        assert!(b.is_initialized(), "populated db is initialized");
        assert!(
            b.current_stakers().is_empty(),
            "fresh State has no in-memory current stakers before load"
        );
        assert!(
            b.pending_stakers().is_empty(),
            "fresh State has no in-memory pending stakers before load"
        );

        b.load().expect("load()");

        // The current set resumes both stakers, in `Staker` (Less) order.
        let current = b.current_stakers();
        assert_eq!(current.len(), 2, "resumed both current stakers");
        // Full-field equality (incl. the BLS key round-trip) for each.
        let resumed_val = b
            .get_current_validator(Id::EMPTY, NodeId::from([0xAA; 20]))
            .expect("current validator point lookup");
        assert!(
            resumed_val.equals(&cur_validator),
            "resumed current validator matches (incl. BLS key) — got {resumed_val:?}"
        );
        assert!(
            current.iter().any(|s| s.equals(&cur_delegator)),
            "resumed current delegator matches"
        );

        // The pending set resumes the subnet validator.
        let pending = b.pending_stakers();
        assert_eq!(pending.len(), 1, "resumed the pending validator");
        assert!(
            pending[0].equals(&pend_validator),
            "resumed pending validator matches"
        );
    }

    /// Re-opening a `State` over a base DB that persisted subnets, per-subnet
    /// chains, and the UTXO address index rebuilds those in-memory caches. The
    /// byte-valued spaces are write-through, but `State::new` leaves their
    /// in-memory mirrors (`subnet_ids` / `chain_index` / `utxo_index`) empty, so
    /// without `load()` a recovered node would report no subnets, no chains, and
    /// an empty `getUTXOs` index despite the data being on disk — the
    /// "subnet / chain / UTXO-index caches" half of advanced-tip resume.
    #[test]
    fn reopen_resumes_persisted_subnet_chain_and_utxo_index_caches() {
        let shared: std::sync::Arc<dyn ava_database::DynDatabase> =
            std::sync::Arc::new(MemDb::new());

        let user_subnet = Id::from([0x5B; 32]);
        // A genesis chain recorded under the primary network (subnet id EMPTY)
        // and a chain recorded under the created subnet — `add_chain` nests
        // each under its subnet via the hashed `chains.join(subnet)` sub-space,
        // so the resume must enumerate per-subnet, not a flat scan.
        let primary_chain = Id::from([0xC1; 32]);
        let subnet_chain = Id::from([0xC2; 32]);
        let addr_a = ShortId::from_slice(&[0x0A; 20]).expect("addr");
        let addr_b = ShortId::from_slice(&[0x0B; 20]).expect("addr");

        let u1 = utxo(0x01, 0, &[addr_a]);
        let u2 = utxo(0x02, 0, &[addr_a, addr_b]);

        // First "process": persist subnets, chains, and UTXOs + an advanced tip.
        {
            let mut a = State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared)))
                .expect("State::new A");
            a.add_subnet(user_subnet);
            a.add_chain(Id::EMPTY, primary_chain);
            a.add_chain(user_subnet, subnet_chain);
            a.add_utxo(u1.input_id(), u1.marshal().expect("marshal u1"));
            a.add_utxo(u2.input_id(), u2.marshal().expect("marshal u2"));
            a.set_last_accepted(Id::from([0x7C; 32]));
            a.set_height(3);
        } // drop A — only the base DB survives.

        // Second "process": fresh State over the SAME backend.
        let mut b = State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared)))
            .expect("State::new B");
        // Before load: the bug this closes — the populated db's byte spaces are
        // on disk, but the in-memory caches are empty.
        assert!(b.subnets().is_empty(), "no in-memory subnets before load");
        assert!(
            b.chains(Id::EMPTY).is_empty(),
            "no in-memory primary-network chains before load"
        );
        assert!(
            b.chains(user_subnet).is_empty(),
            "no in-memory subnet chains before load"
        );
        assert!(
            b.utxo_ids(&addr_a, Id::EMPTY, usize::MAX).is_empty(),
            "no in-memory utxo index before load"
        );

        b.load().expect("load()");

        assert_eq!(b.subnets(), vec![user_subnet], "resumed subnet set");
        assert_eq!(
            b.chains(Id::EMPTY),
            vec![primary_chain],
            "resumed primary-network chains"
        );
        assert_eq!(
            b.chains(user_subnet),
            vec![subnet_chain],
            "resumed subnet chains"
        );

        let mut a_ids = vec![u1.input_id(), u2.input_id()];
        a_ids.sort();
        assert_eq!(
            b.utxo_ids(&addr_a, Id::EMPTY, usize::MAX),
            a_ids,
            "resumed addr_a utxo index"
        );
        assert_eq!(
            b.utxo_ids(&addr_b, Id::EMPTY, usize::MAX),
            vec![u2.input_id()],
            "resumed addr_b utxo index"
        );
    }

    /// Re-opening a `State` over a base DB that persisted ACP-77 L1 validators
    /// rebuilds the in-memory `validation_id → L1Validator` map (the
    /// L1-validator half of advanced-tip resume): `put_l1_validator` writes each
    /// record through to its disk sublist (marshalled with the `GenesisCodec`,
    /// keyed by the `ValidationID`), and `load()` decodes them back so a
    /// recovered node sees its subnet validators, their summed weight, and the
    /// active-validator iterator — rather than an empty set. Mirrors the staker
    /// resume above; the in-memory map only grows via `put_l1_validator` (the
    /// key is the stable `ValidationID`, no orphan/replace cleanup needed).
    #[test]
    fn reopen_resumes_persisted_l1_validators() {
        let shared: std::sync::Arc<dyn ava_database::DynDatabase> =
            std::sync::Arc::new(MemDb::new());

        let subnet = Id::from([0x5B; 32]);
        // An ACTIVE validator (non-zero weight + non-zero end_accumulated_fee)
        // carrying a non-empty public key, and an INACTIVE validator
        // (end_accumulated_fee == 0) on a different subnet — exercises the
        // active iterator, the per-subnet weight sum, and a multi-subnet map.
        let other_subnet = Id::from([0x6C; 32]);
        let active = L1Validator {
            validation_id: Id::from([0x01; 32]),
            subnet_id: subnet,
            node_id: NodeId::from([0xAA; 20]),
            public_key: vec![0x33, 0x44, 0x55],
            remaining_balance_owner: vec![0x66, 0x77],
            deactivation_owner: vec![0x88],
            start_time: 1_700_000_000,
            weight: 1_000,
            min_nonce: 3,
            end_accumulated_fee: 9_999,
        };
        let inactive = L1Validator {
            validation_id: Id::from([0x02; 32]),
            subnet_id: other_subnet,
            node_id: NodeId::from([0xBB; 20]),
            public_key: vec![0x01],
            remaining_balance_owner: Vec::new(),
            deactivation_owner: Vec::new(),
            start_time: 1_700_000_001,
            weight: 500,
            min_nonce: 0,
            end_accumulated_fee: 0,
        };

        // First "process": persist the L1 validators + an advanced tip.
        {
            let mut a = State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared)))
                .expect("State::new A");
            a.put_l1_validator(active.clone())
                .expect("put active l1 validator");
            a.put_l1_validator(inactive.clone())
                .expect("put inactive l1 validator");
            a.set_last_accepted(Id::from([0x7C; 32]));
            a.set_height(4);
        } // drop A — only the base DB survives.

        // Second "process": fresh State over the SAME backend.
        let mut b = State::new(crate::vm::DynDb::new(std::sync::Arc::clone(&shared)))
            .expect("State::new B");
        assert!(b.is_initialized(), "populated db is initialized");
        assert!(
            b.get_l1_validator(active.validation_id).is_err(),
            "fresh State has no in-memory L1 validators before load"
        );
        assert!(
            b.active_l1_validators().is_empty(),
            "no active L1 validators before load"
        );

        b.load().expect("load()");

        // Full-field equality (incl. the GenesisCodec round-trip + the
        // ValidationID supplied from the DB key) for each.
        assert_eq!(
            b.get_l1_validator(active.validation_id)
                .expect("active resolves"),
            active,
            "resumed active L1 validator matches"
        );
        assert_eq!(
            b.get_l1_validator(inactive.validation_id)
                .expect("inactive resolves"),
            inactive,
            "resumed inactive L1 validator matches"
        );
        // Per-subnet weight sums and the active-only iterator resume.
        assert_eq!(
            b.weight_of_l1_validators(subnet).expect("weight subnet"),
            1_000,
            "resumed subnet weight"
        );
        assert_eq!(
            b.weight_of_l1_validators(other_subnet)
                .expect("weight other"),
            500,
            "resumed other-subnet weight"
        );
        assert_eq!(
            b.active_l1_validators(),
            vec![active],
            "only the active validator is in the active iterator"
        );
    }
}
