// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PChainValidatorManager` — the P-Chain implementation of
//! [`ava_validators::ValidatorState`] (`vms/platformvm/validators/manager.go`,
//! specs 08 §7 / §7.1 / §12).
//!
//! The manager answers historical validator-set queries by **reconstruction**:
//! it holds the current (last-accepted-height) validator set in memory and
//! un-applies the persisted per-height weight + public-key diffs backward over
//! `(target, current]` (the M4.14 [`diff_iterators`](crate::state::diff_iterators)),
//! returning a `BTreeMap<NodeId, _>` so iteration is canonically `NodeId`-ascending
//! (the windower / sampler / Warp determinism contract, 08 §6.1).
//!
//! ## Lock-free reads + refresh seam (08 §12)
//!
//! The full read-snapshot the manager queries — the current per-subnet sets, the
//! diff stores, the height / last-accepted singletons and the block index — is
//! captured in an immutable [`ManagerView`] held behind an [`ArcSwap`]. Reads are
//! lock-free; the block acceptor refreshes the view with [`refresh`](PChainValidatorManager::refresh)
//! after flushing an accepted block, and notes the accepted block id through the
//! [`BlockAcceptanceNotifier`](crate::block::executor::BlockAcceptanceNotifier)
//! seam so [`get_minimum_height`](PChainValidatorManager::get_minimum_height) can
//! lag behind the recently-accepted window.
//!
//! ## Caching
//!
//! Per-subnet `height → set` results are memoized in a bounded LRU (size 64,
//! Go `validatorSetsCacheSize`). The current set is never cached (it changes
//! every block).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use parking_lot::Mutex;

use ava_crypto::bls::PublicKey;
use ava_database::Database;
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::error::{Error as VError, Result as VResult};
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;

use crate::block::executor::BlockAcceptanceNotifier;
use crate::error::Error;
use crate::state::chain::Chain;
use crate::state::diff_iterators::{
    DiffValidator, PublicKeyDiffStore, WeightDiffStore, apply_all_validator_public_key_diffs,
    apply_all_validator_weight_diffs, apply_validator_public_key_diffs,
    apply_validator_weight_diffs,
};
use crate::state::state::State;

/// The maximum number of blocks the recommended minimum height lags behind the
/// last-accepted block (Go `MaxRecentlyAcceptedWindowSize`).
pub const MAX_RECENTLY_ACCEPTED_WINDOW_SIZE: usize = 64;
/// The amount of time after a block is accepted to avoid recommending it as the
/// minimum height (Go `RecentlyAcceptedWindowTTL`). Size takes precedence.
pub const RECENTLY_ACCEPTED_WINDOW_TTL: Duration = Duration::from_secs(30);
/// The per-subnet `height → set` LRU capacity (Go `validatorSetsCacheSize`).
pub const VALIDATOR_SETS_CACHE_SIZE: usize = 64;

/// Renders a P-Chain [`Error`] as an [`ava_validators::Error::State`].
fn state_err(e: Error) -> VError {
    VError::State {
        message: e.to_string(),
    }
}

/// A minimal sliding window of recently-accepted block ids (Go
/// `utils/window.Window`): bounded to `max_size` newest entries and evicting
/// entries older than `ttl`. The size bound takes precedence over the TTL.
struct RecentlyAccepted {
    max_size: usize,
    ttl: Duration,
    /// `(added_at, id)` in oldest → newest insertion order.
    entries: Mutex<VecDeque<(Instant, Id)>>,
}

impl RecentlyAccepted {
    fn new(max_size: usize, ttl: Duration) -> Self {
        Self {
            max_size,
            ttl,
            entries: Mutex::new(VecDeque::new()),
        }
    }

    /// `Add` — append `id`, then evict over-capacity and TTL-expired entries.
    fn add(&self, id: Id) {
        let now = Instant::now();
        let mut entries = self.entries.lock();
        entries.push_back((now, id));
        Self::evict(&mut entries, self.max_size, self.ttl, now);
    }

    /// `Oldest` — the oldest live block id, evicting first.
    fn oldest(&self) -> Option<Id> {
        let now = Instant::now();
        let mut entries = self.entries.lock();
        Self::evict(&mut entries, self.max_size, self.ttl, now);
        entries.front().map(|(_, id)| *id)
    }

    fn evict(entries: &mut VecDeque<(Instant, Id)>, max_size: usize, ttl: Duration, now: Instant) {
        // Size bound takes precedence (Go: "size constraints take precedence").
        while entries.len() > max_size {
            entries.pop_front();
        }
        while let Some(&(added, _)) = entries.front() {
            if now.duration_since(added) >= ttl {
                entries.pop_front();
            } else {
                break;
            }
        }
    }
}

/// A bounded LRU keyed by height, holding a shareable validator-set value.
struct HeightLru {
    capacity: usize,
    /// (height, set) in least- → most-recently-used order.
    entries: Vec<(u64, Arc<BTreeMap<NodeId, GetValidatorOutput>>)>,
}

impl HeightLru {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Vec::new(),
        }
    }

    fn get(&mut self, height: u64) -> Option<Arc<BTreeMap<NodeId, GetValidatorOutput>>> {
        let pos = self.entries.iter().position(|(h, _)| *h == height)?;
        let entry = self.entries.remove(pos);
        let value = Arc::clone(&entry.1);
        self.entries.push(entry);
        Some(value)
    }

    fn put(&mut self, height: u64, value: Arc<BTreeMap<NodeId, GetValidatorOutput>>) {
        if let Some(pos) = self.entries.iter().position(|(h, _)| *h == height) {
            self.entries.remove(pos);
        } else if self.entries.len() >= self.capacity {
            self.entries.remove(0);
        }
        self.entries.push((height, value));
    }
}

/// A current base-staker projection captured for `get_current_validator_set`.
#[derive(Clone)]
struct CurrentStaker {
    tx_id: Id,
    node_id: NodeId,
    public_key: Option<PublicKey>,
    weight: u64,
    start_time: u64,
}

/// A current L1-validator projection captured for `get_current_validator_set`.
#[derive(Clone)]
struct CurrentL1Validator {
    validation_id: Id,
    node_id: NodeId,
    public_key: Vec<u8>,
    weight: u64,
    start_time: u64,
    min_nonce: u64,
    is_active: bool,
}

/// The immutable read-snapshot the manager queries (08 §12). Captured from a
/// [`State`] and swapped atomically on [`refresh`](PChainValidatorManager::refresh).
struct ManagerView<D: Database> {
    /// A frozen [`Chain`] view (for `get_subnet_id`'s `get_tx` lookup).
    chain: Arc<dyn Chain>,
    /// The current per-subnet validator set (weight + uncompressed pk bytes).
    current_sets: BTreeMap<Id, BTreeMap<NodeId, DiffValidator>>,
    /// Current base stakers per subnet (for `get_current_validator_set`).
    base_stakers: BTreeMap<Id, Vec<CurrentStaker>>,
    /// Current L1 validators per subnet (for `get_current_validator_set`).
    l1_validators: BTreeMap<Id, Vec<CurrentL1Validator>>,
    /// The last-accepted block height.
    height: u64,
    /// The reverse `block id → height` index (for `get_minimum_height`).
    block_heights: BTreeMap<Id, u64>,
    /// The persisted staker weight-diff store.
    weight_store: WeightDiffStore<D>,
    /// The persisted staker public-key-diff store.
    pk_store: PublicKeyDiffStore<D>,
}

impl<D: Database + 'static> ManagerView<D> {
    /// Captures a snapshot of `state`.
    fn capture(state: &State<D>) -> Self {
        let chain = state.snapshot();

        let mut base_stakers: BTreeMap<Id, Vec<CurrentStaker>> = BTreeMap::new();
        for s in state.current_stakers() {
            if !s.priority.is_current_validator() {
                continue;
            }
            base_stakers
                .entry(s.subnet_id)
                .or_default()
                .push(CurrentStaker {
                    tx_id: s.tx_id,
                    node_id: s.node_id,
                    public_key: s.public_key.clone(),
                    weight: s.weight,
                    start_time: unix_secs(s.start_time),
                });
        }

        let mut l1_validators: BTreeMap<Id, Vec<CurrentL1Validator>> = BTreeMap::new();
        for v in state.active_l1_validators() {
            l1_validators
                .entry(v.subnet_id)
                .or_default()
                .push(CurrentL1Validator {
                    validation_id: v.validation_id,
                    node_id: v.node_id,
                    public_key: v.public_key.clone(),
                    weight: v.weight,
                    start_time: v.start_time,
                    min_nonce: v.min_nonce,
                    is_active: v.is_active(),
                });
        }

        let block_heights = state
            .block_id_index()
            .iter()
            .map(|(&h, &id)| (id, h))
            .collect();

        Self {
            chain,
            current_sets: state.current_validator_sets(),
            base_stakers,
            l1_validators,
            height: state.height(),
            block_heights,
            weight_store: state.weight_diff_store(),
            pk_store: state.public_key_diff_store(),
        }
    }

    /// The current validator set of `subnet` as a fresh `DiffValidator` map (the
    /// reconstruction starting point). Unknown subnets are an empty set.
    fn current_set(&self, subnet: Id) -> BTreeMap<NodeId, DiffValidator> {
        self.current_sets.get(&subnet).cloned().unwrap_or_default()
    }
}

/// Converts a reconstructed `DiffValidator` set into the public
/// `BTreeMap<NodeId, GetValidatorOutput>`, parsing the stored uncompressed BLS
/// key bytes back into a [`PublicKey`].
fn to_output_set(
    set: BTreeMap<NodeId, DiffValidator>,
) -> VResult<BTreeMap<NodeId, GetValidatorOutput>> {
    let mut out = BTreeMap::new();
    for (node_id, v) in set {
        let public_key = match &v.public_key {
            Some(bytes) => {
                Some(
                    PublicKey::from_uncompressed(bytes).map_err(|e| VError::State {
                        message: format!("invalid stored BLS key: {e}"),
                    })?,
                )
            }
            None => None,
        };
        out.insert(
            node_id,
            GetValidatorOutput {
                node_id,
                public_key,
                weight: v.weight,
            },
        );
    }
    Ok(out)
}

/// Seconds since the Unix epoch for `t` (saturating at 0 before the epoch).
fn unix_secs(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `validators.Manager` — the P-Chain [`ValidatorState`] (08 §7).
pub struct PChainValidatorManager<D: Database> {
    /// The lock-free read-snapshot (08 §12).
    view: ArcSwap<ManagerView<D>>,
    /// The recently-accepted sliding window (Go `recentlyAccepted`).
    recently_accepted: RecentlyAccepted,
    /// `cfg.UseCurrentHeight` — override `get_minimum_height` to the current
    /// height (used by subnets that always sample at the tip).
    use_current_height: bool,
    /// Per-subnet `height → set` LRU caches (Go `caches`).
    caches: Mutex<HashMap<Id, HeightLru>>,
}

impl<D: Database + 'static> PChainValidatorManager<D> {
    /// Builds a manager over a snapshot of `state`.
    ///
    /// `use_current_height` mirrors Go `config.Internal.UseCurrentHeight`: when
    /// `true`, [`get_minimum_height`](Self::get_minimum_height) always returns the
    /// current height.
    #[must_use]
    pub fn from_state(state: &State<D>, use_current_height: bool) -> Self {
        Self {
            view: ArcSwap::from_pointee(ManagerView::capture(state)),
            recently_accepted: RecentlyAccepted::new(
                MAX_RECENTLY_ACCEPTED_WINDOW_SIZE,
                RECENTLY_ACCEPTED_WINDOW_TTL,
            ),
            use_current_height,
            caches: Mutex::new(HashMap::new()),
        }
    }

    /// Re-captures the read-snapshot from `state` (the acceptor calls this after
    /// flushing an accepted block) and clears the per-subnet caches, so the next
    /// queries observe the new current set.
    pub fn refresh(&self, state: &State<D>) {
        self.view.store(Arc::new(ManagerView::capture(state)));
        self.caches.lock().clear();
    }

    /// The current P-Chain height (Go `getCurrentHeight`).
    fn current_height(&self) -> u64 {
        self.view.load().height
    }

    /// Reconstructs the validator set of `subnet` at `target` by un-applying the
    /// weight + public-key diffs over `(target, current]` (Go `makeValidatorSet`).
    fn make_validator_set(
        &self,
        view: &ManagerView<D>,
        target: u64,
        subnet: Id,
    ) -> VResult<BTreeMap<NodeId, GetValidatorOutput>> {
        let current_height = view.height;
        if current_height < target {
            return Err(VError::UnfinalizedHeight);
        }

        let mut set = view.current_set(subnet);
        // Apply diffs in [target + 1, current] (the inclusive window).
        let last_diff_height = target.saturating_add(1);
        apply_validator_weight_diffs(
            &view.weight_store,
            &mut set,
            current_height,
            last_diff_height,
            subnet,
        )
        .map_err(state_err)?;
        apply_validator_public_key_diffs(
            &view.pk_store,
            &mut set,
            current_height,
            last_diff_height,
            subnet,
        )
        .map_err(state_err)?;

        to_output_set(set)
    }

    /// Reconstructs **all** subnet validator sets at `target` (Go
    /// `makeAllValidatorSets`).
    fn make_all_validator_sets(
        &self,
        view: &ManagerView<D>,
        target: u64,
    ) -> VResult<BTreeMap<Id, BTreeMap<NodeId, GetValidatorOutput>>> {
        let current_height = view.height;
        if current_height < target {
            return Err(VError::UnfinalizedHeight);
        }

        let mut all: BTreeMap<Id, BTreeMap<NodeId, DiffValidator>> = view.current_sets.clone();
        let last_diff_height = target.saturating_add(1);
        apply_all_validator_weight_diffs(
            &view.weight_store,
            &mut all,
            current_height,
            last_diff_height,
        )
        .map_err(state_err)?;
        apply_all_validator_public_key_diffs(
            &view.pk_store,
            &mut all,
            current_height,
            last_diff_height,
        )
        .map_err(state_err)?;

        let mut out = BTreeMap::new();
        for (subnet, set) in all {
            out.insert(subnet, to_output_set(set)?);
        }
        Ok(out)
    }
}

/// Flattens a validator set into a [`WarpSet`] by deduplicating on BLS public key
/// (Go `validators.FlattenValidatorSet`): multiple nodes can share a key, in which
/// case their weights are summed into a single entry. Validators with no key are
/// dropped from the warp set but still count toward `total_weight`. The output
/// `validators` are sorted by uncompressed public-key bytes.
///
/// # Errors
/// Returns [`Error::Overflow`] if the total weight overflows `u64`.
fn flatten_validator_set(set: &BTreeMap<NodeId, GetValidatorOutput>) -> VResult<WarpSet> {
    // pk bytes → (public key, summed weight). BTreeMap keeps canonical pk order.
    let mut by_key: BTreeMap<[u8; 96], (PublicKey, u64)> = BTreeMap::new();
    let mut total_weight: u64 = 0;
    for vdr in set.values() {
        total_weight = total_weight
            .checked_add(vdr.weight)
            .ok_or(VError::WeightOverflow)?;
        let Some(pk) = &vdr.public_key else {
            continue;
        };
        let entry = by_key
            .entry(pk.serialize())
            .or_insert_with(|| (pk.clone(), 0));
        // Summing per-key weights cannot overflow once total_weight didn't.
        entry.1 = entry.1.saturating_add(vdr.weight);
    }

    let validators = by_key
        .into_values()
        .map(|(public_key, weight)| GetValidatorOutput {
            node_id: NodeId::EMPTY,
            public_key: Some(public_key),
            weight,
        })
        .collect();
    Ok(WarpSet {
        validators,
        total_weight,
    })
}

impl<D: Database + 'static> BlockAcceptanceNotifier for PChainValidatorManager<D> {
    /// `OnAcceptedBlockID` — registers `block_id` in the recently-accepted window
    /// (Go `Manager.OnAcceptedBlockID`).
    fn on_accepted_block_id(&self, block_id: Id) {
        self.recently_accepted.add(block_id);
    }
}

#[async_trait]
impl<D: Database + 'static> ValidatorState for PChainValidatorManager<D> {
    async fn get_minimum_height(&self) -> VResult<u64> {
        if self.use_current_height {
            return Ok(self.current_height());
        }

        let Some(oldest) = self.recently_accepted.oldest() else {
            return Ok(self.current_height());
        };

        let view = self.view.load();
        let oldest_height =
            view.block_heights
                .get(&oldest)
                .copied()
                .ok_or_else(|| VError::State {
                    message: format!("oldest recently-accepted block {oldest} not found"),
                })?;

        // The parent of the oldest element in the window: there is always a block
        // accepted before this window (the first added is >= height 1).
        Ok(oldest_height.saturating_sub(1))
    }

    async fn get_current_height(&self) -> VResult<u64> {
        Ok(self.current_height())
    }

    async fn get_subnet_id(&self, chain: Id) -> VResult<Id> {
        if chain == PRIMARY_NETWORK_ID {
            return Ok(PRIMARY_NETWORK_ID);
        }

        let view = self.view.load();
        let bytes = view.chain.get_tx(chain).map_err(state_err)?;
        let codec = crate::txs::codec::codec().map_err(|e| VError::State {
            message: format!("codec: {e}"),
        })?;
        let tx = crate::txs::Tx::parse(&codec, &bytes).map_err(|e| VError::State {
            message: format!("parse CreateChainTx: {e}"),
        })?;
        match tx.unsigned {
            crate::txs::UnsignedTx::CreateChain(c) => Ok(c.subnet_id),
            _ => Err(VError::State {
                message: format!("{chain} is not a blockchain"),
            }),
        }
    }

    async fn get_validator_set(
        &self,
        height: u64,
        subnet: Id,
    ) -> VResult<BTreeMap<NodeId, GetValidatorOutput>> {
        // Consult the per-subnet LRU first.
        if let Some(hit) = self
            .caches
            .lock()
            .get_mut(&subnet)
            .and_then(|lru| lru.get(height))
        {
            return Ok((*hit).clone());
        }

        let view = self.view.load();
        let set = self.make_validator_set(&view, height, subnet)?;

        let shared = Arc::new(set);
        self.caches
            .lock()
            .entry(subnet)
            .or_insert_with(|| HeightLru::new(VALIDATOR_SETS_CACHE_SIZE))
            .put(height, Arc::clone(&shared));
        Ok((*shared).clone())
    }

    async fn get_current_validator_set(
        &self,
        subnet: Id,
    ) -> VResult<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        let view = self.view.load();
        let mut result: BTreeMap<Id, GetCurrentValidatorOutput> = BTreeMap::new();

        if let Some(stakers) = view.base_stakers.get(&subnet) {
            for v in stakers {
                result.insert(
                    v.tx_id,
                    GetCurrentValidatorOutput {
                        validation_id: v.tx_id,
                        node_id: v.node_id,
                        public_key: v.public_key.clone(),
                        weight: v.weight,
                        start_time: v.start_time,
                        min_nonce: 0,
                        is_active: true,
                        is_l1_validator: false,
                    },
                );
            }
        }

        if let Some(l1s) = view.l1_validators.get(&subnet) {
            for v in l1s {
                let public_key = if v.public_key.is_empty() {
                    None
                } else {
                    PublicKey::from_uncompressed(&v.public_key).ok()
                };
                result.insert(
                    v.validation_id,
                    GetCurrentValidatorOutput {
                        validation_id: v.validation_id,
                        node_id: v.node_id,
                        public_key,
                        weight: v.weight,
                        start_time: v.start_time,
                        min_nonce: v.min_nonce,
                        is_active: v.is_active,
                        is_l1_validator: true,
                    },
                );
            }
        }

        Ok((result, view.height))
    }

    async fn get_warp_validator_sets(&self, height: u64) -> VResult<HashMap<Id, WarpSet>> {
        let view = self.view.load();
        let all = self.make_all_validator_sets(&view, height)?;

        let mut out = HashMap::with_capacity(all.len());
        for (subnet, set) in &all {
            // Skip subnets that fail to flatten (warp verification disallowed).
            if let Ok(ws) = flatten_validator_set(set) {
                out.insert(*subnet, ws);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod conformance {
    //! `validator_set_at_height` — the diff-windowing correctness proof.
    //!
    //! Builds a chain of staker add/remove blocks accepted through the M4.20
    //! [`BlockManager`](crate::block::executor::BlockManager) (so real
    //! weight/public-key diffs are written at each accepted height), then for
    //! **every** height reconstructs `get_validator_set(h, subnet)` and asserts
    //! it equals the independently-computed expected set (weights + BLS keys,
    //! `NodeId`-ascending). Also covers `get_minimum_height` /
    //! `get_current_height` / `get_subnet_id` and the `errUnfinalizedHeight`
    //! sentinel. Oracle: Go `validators/manager_test.go`
    //! (`TestGetValidatorSet_AfterEtna`, `TestGetWarpValidatorSets`).

    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ava_crypto::bls::{PublicKey, SecretKey};
    use ava_database::MemDb;
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;

    use super::*;
    use crate::block::Block;
    use crate::block::BlockBody;
    use crate::block::apricot::{ApricotStandardBlock, CommonBlock};
    use crate::block::banff::BanffStandardBlock;
    use crate::block::executor::{BlockManager, BlockState};
    use crate::state::staker::Staker;
    use crate::txs::Priority;
    use crate::txs::executor::{Backend, StakingConfig, UpgradeSchedule};
    use crate::txs::fee::simple_calculator::StaticFeeConfig;

    const AVAX: u64 = 1_000_000_000;

    /// A projected validator set: node → (weight, compressed BLS key bytes).
    type ProjectedSet = BTreeMap<NodeId, (u64, Option<[u8; 48]>)>;

    fn genesis_id() -> Id {
        Id::from([0xAB; 32])
    }

    fn unix(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn pk(seed: u8) -> PublicKey {
        SecretKey::from_bytes(&[seed; 32]).expect("sk").public_key()
    }

    fn backend() -> Backend {
        Backend {
            upgrades: UpgradeSchedule::durango_only(),
            staking: StakingConfig::mainnet(),
            static_fee_config: StaticFeeConfig::MAINNET,
            network_id: 1,
            chain_id: Id::EMPTY,
            avax_asset_id: Id::from([0x42; 32]),
            node_id: NodeId::EMPTY,
            fx: ava_secp256k1fx::Fx::new(Arc::new(ava_utils::clock::MockClock::at(UNIX_EPOCH))),
            bootstrapped: true,
        }
    }

    fn genesis_state() -> State<MemDb> {
        let mut s = State::new(MemDb::new()).expect("state");
        s.set_timestamp(unix(1_000));
        s.set_current_supply(Id::EMPTY, 100_000_000 * AVAX);
        s.set_last_accepted(genesis_id());
        s.set_height(0);
        s
    }

    /// A staker mutation applied to a block's diff.
    enum Mutation {
        Add(Staker),
        Remove(Staker),
    }

    /// Builds + accepts a Banff standard block at `height` over `parent`, whose
    /// on-accept diff applies `mutations`. Returns the accepted block id.
    fn accept_block(
        mgr: &mut BlockManager<MemDb>,
        parent: Id,
        height: u64,
        mutations: Vec<Mutation>,
    ) -> Id {
        let blk = {
            let mut b = Block::new(BlockBody::BanffStandard(BanffStandardBlock {
                time: 1_000,
                apricot: ApricotStandardBlock {
                    common: CommonBlock {
                        parent_id: parent,
                        height,
                    },
                    transactions: vec![],
                },
            }));
            b.initialize(mgr.codec()).expect("init standard");
            b
        };

        let mut diff = mgr.new_diff(parent).expect("diff");
        for m in mutations {
            match m {
                Mutation::Add(s) => diff.put_current_validator(s).expect("add"),
                Mutation::Remove(s) => diff.delete_current_validator(&s),
            }
        }
        mgr.cache(
            blk.id(),
            BlockState {
                height,
                on_accept: Some(Arc::new(diff)),
                on_commit: None,
                on_abort: None,
                timestamp: 1_000,
                prefers_commit: true,
            },
        );
        mgr.accept(&blk).expect("accept");
        blk.id()
    }

    /// A current primary-network validator staker with a BLS key.
    fn validator(tx: u8, node: NodeId, key: &PublicKey, subnet: Id, weight: u64) -> Staker {
        Staker::new_current(
            Id::from([tx; 32]),
            node,
            Some(key.clone()),
            subnet,
            weight,
            unix(1_000),
            unix(9_000),
            0,
            if subnet == Id::EMPTY {
                Priority::PrimaryNetworkValidatorCurrent
            } else {
                Priority::SubnetPermissionlessValidatorCurrent
            },
        )
    }

    /// Asserts the reconstructed set at `height` equals `expected`
    /// (node → (weight, compressed pk bytes)), `NodeId`-ascending.
    async fn assert_set(
        m: &PChainValidatorManager<MemDb>,
        height: u64,
        subnet: Id,
        expected: &ProjectedSet,
    ) {
        let got = m.get_validator_set(height, subnet).await.expect("set");
        let got_proj: ProjectedSet = got
            .iter()
            .map(|(n, o)| {
                (
                    *n,
                    (o.weight, o.public_key.as_ref().map(PublicKey::compress)),
                )
            })
            .collect();
        assert_eq!(
            &got_proj, expected,
            "validator set mismatch at height {height}"
        );
    }

    #[tokio::test]
    async fn validator_set_at_height() {
        let node_a = NodeId::from([0x0A; 20]);
        let node_b = NodeId::from([0x0B; 20]);
        let key_a = pk(0x11);
        let key_b = pk(0x22);
        let subnet = Id::EMPTY; // Primary Network.
        let wa = 1_000 * AVAX;
        let wb = 2_000 * AVAX;

        let mut bm = BlockManager::new(
            genesis_state(),
            backend(),
            crate::txs::codec::codec().expect("codec"),
            Arc::new(crate::block::executor::NoopNotifier),
        );
        // Wire the validator manager as the acceptance notifier.
        let vmgr = Arc::new(PChainValidatorManager::from_state(bm.state(), false));
        bm = BlockManager::new(
            // Rebuild with the validator manager as notifier (State is fresh
            // genesis; the prior bm was only used to seed vmgr's initial view).
            genesis_state(),
            backend(),
            crate::txs::codec::codec().expect("codec"),
            Arc::clone(&vmgr) as Arc<dyn BlockAcceptanceNotifier>,
        );
        vmgr.refresh(bm.state());

        // Height 1: add validator A. Height 2: add validator B.
        // Height 3: remove validator A.
        let h1 = accept_block(
            &mut bm,
            genesis_id(),
            1,
            vec![Mutation::Add(validator(1, node_a, &key_a, subnet, wa))],
        );
        vmgr.refresh(bm.state());
        let h2 = accept_block(
            &mut bm,
            h1,
            2,
            vec![Mutation::Add(validator(2, node_b, &key_b, subnet, wb))],
        );
        vmgr.refresh(bm.state());
        let h3 = accept_block(
            &mut bm,
            h2,
            3,
            vec![Mutation::Remove(validator(1, node_a, &key_a, subnet, wa))],
        );
        vmgr.refresh(bm.state());
        let _ = h3;

        assert_eq!(vmgr.get_current_height().await.unwrap(), 3);

        // Expected sets at each height (independently computed).
        let ca = key_a.compress();
        let cb = key_b.compress();
        let expected: [ProjectedSet; 4] = [
            // Height 0: empty (genesis had no stakers).
            BTreeMap::new(),
            // Height 1: A.
            BTreeMap::from([(node_a, (wa, Some(ca)))]),
            // Height 2: A + B.
            BTreeMap::from([(node_a, (wa, Some(ca))), (node_b, (wb, Some(cb)))]),
            // Height 3: B (A removed).
            BTreeMap::from([(node_b, (wb, Some(cb)))]),
        ];

        for (height, exp) in expected.iter().enumerate() {
            assert_set(&vmgr, height as u64, subnet, exp).await;
        }
    }

    #[tokio::test]
    async fn unfinalized_height_is_returned_not_panicked() {
        let bm = BlockManager::new(
            genesis_state(),
            backend(),
            crate::txs::codec::codec().expect("codec"),
            Arc::new(crate::block::executor::NoopNotifier),
        );
        let vmgr = PChainValidatorManager::from_state(bm.state(), false);

        // Current height is 0; querying height 5 must return ErrUnfinalizedHeight.
        assert!(matches!(
            vmgr.get_validator_set(5, Id::EMPTY).await,
            Err(VError::UnfinalizedHeight)
        ));
        assert!(matches!(
            vmgr.get_warp_validator_sets(5).await,
            Err(VError::UnfinalizedHeight)
        ));

        // Height 0 (== current) is finalized → empty set, no error.
        let set = vmgr.get_validator_set(0, Id::EMPTY).await.expect("set");
        assert!(set.is_empty());
    }

    #[tokio::test]
    async fn height_accessors_and_subnet_id() {
        let mut bm = BlockManager::new(
            genesis_state(),
            backend(),
            crate::txs::codec::codec().expect("codec"),
            Arc::new(crate::block::executor::NoopNotifier),
        );
        let vmgr = Arc::new(PChainValidatorManager::from_state(bm.state(), false));
        // Re-wire with the manager as notifier so its window is fed.
        bm = BlockManager::new(
            genesis_state(),
            backend(),
            crate::txs::codec::codec().expect("codec"),
            Arc::clone(&vmgr) as Arc<dyn BlockAcceptanceNotifier>,
        );

        // The Platform chain (Id::EMPTY) maps to the Primary Network.
        assert_eq!(
            vmgr.get_subnet_id(Id::EMPTY).await.unwrap(),
            super::PRIMARY_NETWORK_ID
        );

        // With no accepted blocks in the window, min height == current height (0).
        assert_eq!(vmgr.get_current_height().await.unwrap(), 0);
        assert_eq!(vmgr.get_minimum_height().await.unwrap(), 0);

        // Accept a couple of blocks (no staker changes); the window fills.
        let h1 = accept_block(&mut bm, genesis_id(), 1, vec![]);
        vmgr.refresh(bm.state());
        let _h2 = accept_block(&mut bm, h1, 2, vec![]);
        vmgr.refresh(bm.state());

        assert_eq!(vmgr.get_current_height().await.unwrap(), 2);
        // Oldest in the window is the height-1 block → minimum height = 1 - 1 = 0.
        assert_eq!(vmgr.get_minimum_height().await.unwrap(), 0);

        // use_current_height overrides to the current height.
        let vmgr_cur = PChainValidatorManager::from_state(bm.state(), true);
        assert_eq!(vmgr_cur.get_minimum_height().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn warp_sets_flatten_and_dedup_by_key() {
        // Two nodes share a BLS key → one flattened warp entry with summed weight.
        let node_a = NodeId::from([0x0A; 20]);
        let node_b = NodeId::from([0x0B; 20]);
        let shared = pk(0x33);

        let mut bm = BlockManager::new(
            genesis_state(),
            backend(),
            crate::txs::codec::codec().expect("codec"),
            Arc::new(crate::block::executor::NoopNotifier),
        );
        let vmgr = PChainValidatorManager::from_state(bm.state(), false);

        let _h1 = accept_block(
            &mut bm,
            genesis_id(),
            1,
            vec![
                Mutation::Add(validator(1, node_a, &shared, Id::EMPTY, 100)),
                Mutation::Add(validator(2, node_b, &shared, Id::EMPTY, 250)),
            ],
        );
        vmgr.refresh(bm.state());

        let sets = vmgr.get_warp_validator_sets(1).await.expect("warp");
        let ws = sets.get(&Id::EMPTY).expect("primary warp set");
        assert_eq!(ws.total_weight, 350);
        assert_eq!(ws.validators.len(), 1, "shared key deduped to one entry");
        assert_eq!(ws.validators[0].weight, 350);
        assert_eq!(
            ws.validators[0]
                .public_key
                .as_ref()
                .map(PublicKey::compress),
            Some(shared.compress())
        );
    }
}
