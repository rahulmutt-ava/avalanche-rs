// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-Chain block executor — Verify/Accept/Reject/Options + the acceptor
//! (`vms/platformvm/block/executor`, specs 08 §4.2; bootstrap accept-without-
//! verify, 19 §2).
//!
//! The P-Chain is the only VM that uses Snowman **oracle** blocks: a
//! `*ProposalBlock` produces two children on [`options`](BlockManager::options) —
//! a `*CommitBlock` and an `*AbortBlock` over the same parent — and consensus
//! decides which is accepted (08 §4.2). The [`BlockManager`] threads this:
//!
//! - **[`verify`](BlockManager::verify)** runs the appropriate tx executor(s)
//!   against a fresh [`Diff`] layered over the parent state and caches the
//!   resulting diff(s) per block ([`BlockState`]). A proposal block caches an
//!   `on_commit`/`on_abort` diff pair; every other block caches a single
//!   `on_accept` diff.
//! - **[`options`](BlockManager::options)** materializes the commit/abort
//!   children of a verified proposal block, ordering them by the executor's
//!   commit/abort preference.
//! - **[`accept`](BlockManager::accept)** applies the selected diff down to the
//!   persisted [`State`], writes the staker weight/public-key diffs at the
//!   accepted height (M4.14), records the block + tx bytes, advances the
//!   last-accepted / height singletons, and notifies the
//!   [`BlockAcceptanceNotifier`] (the seam M4.21's `PChainValidatorManager`
//!   will implement).
//! - **[`reject`](BlockManager::reject)** discards the cached diff.
//! - **[`accept_non_verifying`](BlockManager::accept_non_verifying)** accepts a
//!   fetched block *without* re-running Verify — the linear-bootstrap path
//!   (19 §2): it re-executes the block against the base to produce the accept
//!   diff, then applies it exactly as [`accept`](BlockManager::accept) does.
//!
//! ## Diff stack & `Versions`
//!
//! A processing block's cached diff is held as an immutable [`Arc<dyn Chain>`];
//! the [`BlockManager`] implements [`Versions`] so a child block's `Diff` resolves
//! its parent either to a processing block's cached diff or — for the last-accepted
//! block — to a frozen [`State::snapshot`]. On accept the selected diff is flushed
//! to the owned mutable `State` and the base snapshot is refreshed.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_codec::manager::Manager;
use ava_database::Database;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::block::Block;
use crate::error::{Error, Result};
use crate::state::chain::{Chain, Versions};
use crate::state::diff::Diff;
use crate::state::disk_staker_diff_iterator::ValidatorWeightDiff;
use crate::state::state::State;
use crate::txs::Tx;
use crate::txs::executor::Backend;

pub mod acceptor;
pub mod options;
pub mod reject;
pub mod verify;

/// The cached per-block state populated by [`verify`](BlockManager::verify)
/// (Go `blockState`). A proposal block caches a commit/abort diff pair; every
/// other block caches a single accept diff. The diffs are immutable
/// ([`Arc<dyn Chain>`]) once cached so children can resolve them as parents.
pub struct BlockState {
    /// The cached block's height (used to validate child heights). The full
    /// stateless block is **not** cached — `Tx` carries a `!Sync` `OnceCell`, so
    /// caching it would make [`BlockManager`] non-`Sync` (and [`Versions`]
    /// requires `Sync`). The accept path always receives the block by reference
    /// from the caller, so only the height is needed here.
    pub height: u64,
    /// The accept diff for non-proposal blocks (a standard/atomic/option block's
    /// single on-accept state); `None` for proposal blocks.
    pub on_accept: Option<Arc<Diff>>,
    /// The commit diff for a proposal block; `None` otherwise.
    pub on_commit: Option<Arc<Diff>>,
    /// The abort diff for a proposal block; `None` otherwise.
    pub on_abort: Option<Arc<Diff>>,
    /// The block's resolved chain timestamp (seconds since the Unix epoch).
    pub timestamp: u64,
    /// The executor's commit/abort preference (proposal blocks only; see
    /// [`options`](BlockManager::options)).
    pub prefers_commit: bool,
}

/// The notification the acceptor fires when a block is accepted (Go
/// `validators.Manager.OnAcceptedBlockID`).
///
/// This is the **integration seam M4.21's `PChainValidatorManager` will own**: the
/// block acceptor calls [`on_accepted_block_id`](BlockAcceptanceNotifier::on_accepted_block_id)
/// after flushing a block's diff and writing the staker weight/pk diffs, so the
/// validator manager can refresh its recently-accepted window and current set.
/// For M4.20 a default no-op implementation ([`NoopNotifier`]) is provided; the
/// conformance tests inject a recording double to assert the call fires.
pub trait BlockAcceptanceNotifier: Send + Sync {
    /// Notifies the validator manager that `block_id` was accepted.
    fn on_accepted_block_id(&self, block_id: Id);
}

/// A no-op [`BlockAcceptanceNotifier`] (used when no validator manager is wired,
/// e.g. read-only sync before M4.21).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopNotifier;

impl BlockAcceptanceNotifier for NoopNotifier {
    fn on_accepted_block_id(&self, _block_id: Id) {}
}

/// `block/executor.manager` — owns the persisted [`State`], the per-block diff
/// cache, and the verify/accept/reject/options logic.
pub struct BlockManager<D: Database> {
    /// The persisted state base (the bottom of the diff stack).
    state: State<D>,
    /// A frozen snapshot of `state` used as the diff parent for blocks whose
    /// parent is the last-accepted block. Refreshed after each accept.
    base_view: Arc<dyn Chain>,
    /// The tx-executor backend (fork schedule, fee/staking config, fx, ...).
    backend: Backend,
    /// The codec used to (re)build option blocks and parse stored txs.
    codec: Manager,
    /// Per-block verified diff cache, keyed by block id (Go `blkIDToState`).
    blk_id_to_state: BTreeMap<Id, BlockState>,
    /// The id of the most-recently accepted block.
    last_accepted: Id,
    /// The validator-manager notification seam (M4.21).
    notifier: Arc<dyn BlockAcceptanceNotifier>,
}

impl<D: Database + 'static> BlockManager<D> {
    /// Builds a manager over `state` with the given executor `backend`, `codec`,
    /// and the validator-manager `notifier` (use [`NoopNotifier`] when none).
    ///
    /// `state.last_accepted()` seeds the manager's last-accepted block id.
    #[must_use]
    pub fn new(
        state: State<D>,
        backend: Backend,
        codec: Manager,
        notifier: Arc<dyn BlockAcceptanceNotifier>,
    ) -> Self {
        let last_accepted = state.last_accepted();
        let base_view = state.snapshot();
        Self {
            state,
            base_view,
            backend,
            codec,
            blk_id_to_state: BTreeMap::new(),
            last_accepted,
            notifier,
        }
    }

    /// The id of the most-recently accepted block (Go `LastAccepted`).
    #[must_use]
    pub fn last_accepted(&self) -> Id {
        self.last_accepted
    }

    /// The persisted state base (read-only access for tests / callers).
    #[must_use]
    pub fn state(&self) -> &State<D> {
        &self.state
    }

    /// The codec used by the manager (for option-block construction in tests).
    #[must_use]
    pub fn codec(&self) -> &Manager {
        &self.codec
    }

    /// `getTimestamp(blkID)` — the resolved chain time of `block_id`: a processing
    /// block's cached timestamp, else the base chain time. Seconds since epoch.
    #[must_use]
    pub fn timestamp(&self, block_id: Id) -> u64 {
        if let Some(s) = self.blk_id_to_state.get(&block_id) {
            return s.timestamp;
        }
        self.base_view.timestamp_secs()
    }

    /// The resolver the proposal executor uses to recover a staker tx (Go
    /// `state.GetTx`): looks the tx bytes up in `parent`, parses it, and projects
    /// its reward-relevant fields.
    pub(crate) fn staker_tx_resolver<'a>(
        codec: &'a Manager,
        parent: &'a Arc<dyn Chain>,
    ) -> impl Fn(&Id) -> Option<crate::txs::executor::RewardedStakerTx> + 'a {
        move |id: &Id| -> Option<crate::txs::executor::RewardedStakerTx> {
            let bytes = parent.get_tx(*id).ok()?;
            let tx = Tx::parse(codec, &bytes).ok()?;
            crate::block::executor::verify::rewarded_staker_tx(&tx)
        }
    }

    /// Resolves the parent `Chain` view for `parent_id` (Go `backend.GetState`):
    /// a processing block's cached on-accept diff, else the base snapshot when
    /// `parent_id` is the last-accepted block.
    fn parent_view(&self, parent_id: Id) -> Option<Arc<dyn Chain>> {
        if let Some(s) = self.blk_id_to_state.get(&parent_id) {
            return s.on_accept.clone().map(|d| d as Arc<dyn Chain>);
        }
        (parent_id == self.last_accepted).then(|| Arc::clone(&self.base_view))
    }

    /// The `Chain` view to layer a child diff over during verify — for an
    /// already-verified proposal parent, the commit diff (the decision state);
    /// else the parent on-accept view / base snapshot.
    pub(crate) fn get_state_for_verify(&self, parent_id: Id) -> Option<Arc<dyn Chain>> {
        if let Some(s) = self.blk_id_to_state.get(&parent_id) {
            return s
                .on_accept
                .clone()
                .or_else(|| s.on_commit.clone())
                .map(|d| d as Arc<dyn Chain>);
        }
        (parent_id == self.last_accepted).then(|| Arc::clone(&self.base_view))
    }

    /// The height of the block identified by `parent_id` (a cached processing
    /// block's height, else the persisted last-accepted height).
    pub(crate) fn parent_height(&self, parent_id: Id) -> Result<u64> {
        if let Some(s) = self.blk_id_to_state.get(&parent_id) {
            return Ok(s.height);
        }
        if parent_id == self.last_accepted {
            return Ok(self.state.height());
        }
        Err(Error::Database(ava_database::error::Error::NotFound))
    }

    /// `Verify(block)` — executes `block` against the parent state, caching the
    /// resulting diff(s). A proposal block caches a commit/abort pair; every other
    /// block caches a single accept diff (08 §4.2).
    ///
    /// # Errors
    /// Returns an [`Error`] if the parent state is missing, the block height is
    /// wrong, or a tx fails execution.
    pub fn verify(&mut self, block: &Block) -> Result<()> {
        verify::verify(self, block)
    }

    /// `Options(block)` — the `(commit, abort)` children of a verified proposal
    /// block, ordered by the executor's commit/abort preference (08 §4.2).
    ///
    /// # Errors
    /// Returns [`Error::WrongTxType`] if `block` is not a verified proposal block,
    /// or a codec error if the option blocks cannot be built.
    pub fn options(&self, block: &Block) -> Result<(Block, Block)> {
        options::options(self, block)
    }

    /// `Accept(block)` — applies the block's selected diff down to [`State`],
    /// writes the staker weight/pk diffs at the block height, records the block +
    /// txs, advances the last-accepted / height singletons, and notifies the
    /// validator manager (08 §4.2). A proposal block is *not* written here — only
    /// its accepted child writes it (the diff is its decision diff applied first).
    ///
    /// # Errors
    /// Returns an [`Error`] if the block was not verified or a state write fails.
    pub fn accept(&mut self, block: &Block) -> Result<()> {
        acceptor::accept(self, block)
    }

    /// `Reject(block)` — discards the block's cached diff (Go `rejector`).
    pub fn reject(&mut self, block: &Block) {
        self.blk_id_to_state.remove(&block.id());
    }

    /// The bootstrap accept-without-verify path (19 §2): re-executes `block`
    /// against the current base to produce its accept diff, then applies it
    /// exactly as [`accept`](Self::accept). Used during linear bootstrap when
    /// fetched blocks are trusted and need not be re-verified.
    ///
    /// # Errors
    /// Returns an [`Error`] if execution or a state write fails.
    pub fn accept_non_verifying(&mut self, block: &Block) -> Result<()> {
        acceptor::accept_non_verifying(self, block)
    }

    // ----- internal helpers shared by the visitor submodules -----

    /// Caches a verified block's state.
    pub(crate) fn cache(&mut self, block_id: Id, st: BlockState) {
        self.blk_id_to_state.insert(block_id, st);
    }

    /// Removes a cached block state.
    pub(crate) fn free(&mut self, block_id: Id) {
        self.blk_id_to_state.remove(&block_id);
    }

    /// The cached state of `block_id`, if any.
    pub(crate) fn cached(&self, block_id: Id) -> Option<&BlockState> {
        self.blk_id_to_state.get(&block_id)
    }

    /// The executor backend.
    pub(crate) fn backend(&self) -> &Backend {
        &self.backend
    }

    /// A fresh [`Diff`] over `parent_id` (Go `state.NewDiff`).
    pub(crate) fn new_diff(&self, parent_id: Id) -> Result<Diff> {
        Diff::new(parent_id, self)
    }

    /// Applies `diff` down to the persisted [`State`], writes the per-height
    /// staker weight/pk diffs, records the block + its txs, advances the
    /// last-accepted / height singletons, refreshes the base snapshot, and fires
    /// the [`BlockAcceptanceNotifier`].
    pub(crate) fn commit_accept(&mut self, block: &Block, diff: &Diff) -> Result<()> {
        let height = block.height();
        let block_id = block.id();

        // Snapshot the current validator-set weights / public keys before the
        // diff lands, so we can compute the per-height diffs Go writes
        // (`calculateValidatorDiffs` + `writeValidatorDiffs`).
        let weights_before = self.state.current_validator_weights();
        let keys_before = self.state.current_validator_public_keys();

        diff.apply(&mut self.state)?;

        let weights_after = self.state.current_validator_weights();
        let keys_after = self.state.current_validator_public_keys();
        self.write_validator_diffs(
            height,
            &weights_before,
            &weights_after,
            &keys_before,
            &keys_after,
        )?;

        // Record the block, its txs, and advance the singletons.
        for tx in block.txs() {
            self.state.add_tx(tx.id(), tx.bytes().to_vec());
        }
        self.state.add_block(block_id, height, block.bytes());
        self.state.set_last_accepted(block_id);
        self.state.set_height(height);
        self.last_accepted = block_id;

        // Refresh the frozen base view so the next block layers over the new base.
        self.base_view = self.state.snapshot();

        self.notifier.on_accepted_block_id(block_id);
        Ok(())
    }

    /// Records the last-accepted singleton for a proposal block without writing
    /// it to disk (Go `acceptor.proposalBlock`): the proposal's decision diff is
    /// applied when its child is accepted, so we only advance the in-memory
    /// last-accepted pointer here.
    pub(crate) fn note_proposal_accept(&mut self, block_id: Id) {
        self.last_accepted = block_id;
    }

    /// Writes the staker weight diffs and public-key diffs at `height` (Go
    /// `writeValidatorDiffs`): a weight increase is `decrease = false`, a decrease
    /// `decrease = true`; a pk diff stores the key the node *had before*.
    fn write_validator_diffs(
        &self,
        height: u64,
        weights_before: &BTreeMap<(Id, NodeId), u64>,
        weights_after: &BTreeMap<(Id, NodeId), u64>,
        keys_before: &BTreeMap<(Id, NodeId), Vec<u8>>,
        keys_after: &BTreeMap<(Id, NodeId), Vec<u8>>,
    ) -> Result<()> {
        let weight_store = self.state.weight_diff_store();
        let pk_store = self.state.public_key_diff_store();

        // Union of all (subnet, node) keys touched on either side.
        let mut touched: std::collections::BTreeSet<(Id, NodeId)> =
            std::collections::BTreeSet::new();
        touched.extend(weights_before.keys().copied());
        touched.extend(weights_after.keys().copied());
        touched.extend(keys_before.keys().copied());
        touched.extend(keys_after.keys().copied());

        for (subnet, node) in touched {
            let before = weights_before.get(&(subnet, node)).copied().unwrap_or(0);
            let after = weights_after.get(&(subnet, node)).copied().unwrap_or(0);
            if before != after {
                let diff = if after > before {
                    ValidatorWeightDiff {
                        decrease: false,
                        amount: after.saturating_sub(before),
                    }
                } else {
                    ValidatorWeightDiff {
                        decrease: true,
                        amount: before.saturating_sub(after),
                    }
                };
                weight_store.put(subnet, node, height, &diff)?;
            }

            // The pk diff stores the key the node had *before* the change; only
            // written when the key actually changed.
            let prev = keys_before.get(&(subnet, node));
            let new = keys_after.get(&(subnet, node));
            if prev != new {
                let prev_bytes = prev.map_or(&[][..], Vec::as_slice);
                pk_store.put(subnet, node, height, prev_bytes)?;
            }
        }
        Ok(())
    }
}

impl<D: Database + 'static> Versions for BlockManager<D> {
    fn get_state(&self, block_id: Id) -> Option<Arc<dyn Chain>> {
        self.parent_view(block_id)
    }
}

/// A small `Chain` extension used by the manager for the seconds-since-epoch
/// chain time (the wire/diff form of the timestamp).
trait ChainTimeSecs {
    fn timestamp_secs(&self) -> u64;
}

impl ChainTimeSecs for Arc<dyn Chain> {
    fn timestamp_secs(&self) -> u64 {
        self.timestamp()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
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
    //! Block-executor conformance: the oracle (proposal → commit/abort option
    //! generation + selecting the right diff on accept) and the acceptor's
    //! state-flushing contract (Go `proposal_block_test.go`, `acceptor_test.go`,
    //! `options_test.go`). Test names are prefixed `block_oracle_*` /
    //! `accept_writes_diffs_*` so the plan's nextest filters match.

    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ava_database::MemDb;
    use ava_secp256k1fx::OutputOwners;
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_types::short_id::ShortId;

    use super::*;
    use crate::block::BlockBody;
    use crate::block::apricot::{ApricotProposalBlock, ApricotStandardBlock, CommonBlock};
    use crate::block::banff::{BanffProposalBlock, BanffStandardBlock};
    use crate::state::chain::Chain;
    use crate::state::diff_iterators::{DiffValidator, apply_validator_weight_diffs};
    use crate::state::staker::Staker;
    use crate::txs::components::{Output, Owner, TransferableOutput};
    use crate::txs::executor::{Backend, StakingConfig, UpgradeSchedule};
    use crate::txs::fee::simple_calculator::StaticFeeConfig;
    use crate::txs::{Priority, RewardValidatorTx, Tx, UnsignedTx};

    const AVAX: u64 = 1_000_000_000;
    const AVAX_ASSET: [u8; 32] = [0x42; 32];

    fn genesis_id() -> Id {
        Id::from([0xAB; 32])
    }

    fn unix(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn owners(addr: u8) -> OutputOwners {
        OutputOwners::new(0, 1, vec![ShortId::from([addr; 20])])
    }

    /// A test backend (Durango-active, pre-Etna), mainnet staking.
    fn backend() -> Backend {
        Backend {
            upgrades: UpgradeSchedule::durango_only(),
            staking: StakingConfig::mainnet(),
            static_fee_config: StaticFeeConfig::MAINNET,
            network_id: 1,
            chain_id: Id::EMPTY,
            avax_asset_id: Id::from(AVAX_ASSET),
            node_id: NodeId::EMPTY,
            fx: ava_secp256k1fx::Fx::new(Arc::new(ava_utils::clock::MockClock::at(UNIX_EPOCH))),
            bootstrapped: true,
        }
    }

    /// A counting [`BlockAcceptanceNotifier`] double recording the last accepted
    /// block id and the number of notifications.
    #[derive(Default)]
    struct RecordingNotifier {
        count: AtomicU64,
        last: parking_lot::Mutex<Id>,
    }
    impl BlockAcceptanceNotifier for RecordingNotifier {
        fn on_accepted_block_id(&self, block_id: Id) {
            self.count.fetch_add(1, Ordering::SeqCst);
            *self.last.lock() = block_id;
        }
    }

    /// Builds a genesis-seeded `State` (chain time `ts`, primary supply `supply`,
    /// last-accepted = [`genesis_id`] at height 0), applying `seed`.
    fn genesis_state(
        ts: SystemTime,
        supply: u64,
        seed: impl FnOnce(&mut State<MemDb>),
    ) -> State<MemDb> {
        let mut s = State::new(MemDb::new()).expect("state");
        s.set_timestamp(ts);
        s.set_current_supply(Id::EMPTY, supply);
        s.set_last_accepted(genesis_id());
        s.set_height(0);
        seed(&mut s);
        s
    }

    fn manager(state: State<MemDb>, notifier: Arc<RecordingNotifier>) -> BlockManager<MemDb> {
        let codec = crate::txs::codec::codec().expect("codec");
        BlockManager::new(state, backend(), codec, notifier)
    }

    /// Build + initialize a permissionless-validator staker tx whose stake refunds
    /// `weight` to `owners(1)`; returns the signed tx (so its bytes/id are set).
    fn staker_tx(weight: u64) -> Tx {
        use crate::txs::AddPermissionlessValidatorTx;
        let v = AddPermissionlessValidatorTx {
            stake_outs: vec![TransferableOutput {
                asset_id: Id::from(AVAX_ASSET),
                out: Output::Transfer(ava_secp256k1fx::TransferOutput::new(weight, owners(1))),
            }],
            validator_rewards_owner: Owner::Secp256k1(owners(1)),
            delegator_rewards_owner: Owner::Secp256k1(owners(1)),
            ..Default::default()
        };
        let mut tx = Tx::new(UnsignedTx::AddPermissionlessValidator(v));
        let codec = crate::txs::codec::codec().expect("codec");
        tx.initialize(&codec).expect("init staker tx");
        tx
    }

    /// `block_oracle`: a `*BanffProposalBlock` carrying a `RewardValidatorTx` for a
    /// current validator due to leave produces a commit + abort option pair (same
    /// parent, next height), and accepting the **commit** child removes the staker
    /// from `State` and writes its reward UTXO (08 §4.2).
    #[test]
    fn block_oracle_proposal_options_and_commit_accept() {
        let node = NodeId::from([9; 20]);
        let end = unix(3_000);
        let weight = 2_000 * AVAX;
        let reward = 38_944;

        // The staker tx must be resolvable from the tx store for the reward path.
        let tx = staker_tx(weight);
        let staker_tx_id = tx.id();

        let state = genesis_state(end, 100_000_000 * AVAX, |s| {
            let staker = Staker::new_current(
                staker_tx_id,
                node,
                None,
                Id::EMPTY,
                weight,
                unix(1_000),
                end,
                reward,
                Priority::PrimaryNetworkValidatorCurrent,
            );
            s.put_current_validator(staker).expect("put current");
            s.add_tx(staker_tx_id, tx.bytes().to_vec());
        });
        let supply_before = state.current_supply(Id::EMPTY).unwrap();

        let notifier = Arc::new(RecordingNotifier::default());
        let mut mgr = manager(state, Arc::clone(&notifier));

        // A Banff proposal block at height 1 over genesis, time == end.
        let proposal = {
            let mut blk = Block::new(BlockBody::BanffProposal(BanffProposalBlock {
                time: 3_000,
                transactions: vec![],
                apricot: ApricotProposalBlock {
                    common: CommonBlock {
                        parent_id: genesis_id(),
                        height: 1,
                    },
                    tx: Tx::new(UnsignedTx::RewardValidator(RewardValidatorTx {
                        tx_id: staker_tx_id,
                    })),
                },
            }));
            blk.initialize(mgr.codec()).expect("init proposal");
            blk
        };

        mgr.verify(&proposal).expect("verify proposal");

        // Options: commit is preferred (prefers_commit defaults true), both option
        // blocks point at the proposal and sit at height 2.
        let (preferred, alternate) = mgr.options(&proposal).expect("options");
        assert!(
            matches!(preferred.body(), BlockBody::BanffCommit(_)),
            "preferred should be the commit block"
        );
        assert!(matches!(alternate.body(), BlockBody::BanffAbort(_)));
        assert_eq!(preferred.parent_id(), proposal.id());
        assert_eq!(alternate.parent_id(), proposal.id());
        assert_eq!(preferred.height(), 2);

        // Verify both options, accept the proposal then the commit child.
        mgr.verify(&preferred).expect("verify commit");
        mgr.verify(&alternate).expect("verify abort");
        mgr.accept(&proposal).expect("accept proposal");
        // The proposal is *not* written to disk; only the last-accepted pointer
        // advances in memory.
        assert_eq!(mgr.last_accepted(), proposal.id());

        mgr.accept(&preferred).expect("accept commit");

        // The committed diff flushed to State: staker removed, supply unchanged
        // (the mint accrued at promotion), a reward UTXO recorded.
        let s = mgr.state();
        assert!(
            s.get_current_validator(Id::EMPTY, node).is_err(),
            "staker should be removed on commit"
        );
        assert_eq!(s.current_supply(Id::EMPTY).unwrap(), supply_before);
        assert_eq!(s.get_reward_utxos(staker_tx_id).len(), 1);

        // Last-accepted / height advanced to the commit block.
        assert_eq!(mgr.last_accepted(), preferred.id());
        assert_eq!(s.height(), 2);
        assert_eq!(s.last_accepted(), preferred.id());
        assert_eq!(s.get_block_id_at_height(2), Some(preferred.id()));
        assert!(s.get_block(preferred.id()).is_ok());

        // The validator manager was notified for both accepted state blocks
        // (the proposal block does not notify — it is not written).
        assert_eq!(notifier.count.load(Ordering::SeqCst), 1);
        assert_eq!(*notifier.last.lock(), preferred.id());

        // A weight-decrease diff was written at height 2: reconstructing the set
        // at height 1 from the (now-empty) current set re-adds the staker's weight.
        let store = s.weight_diff_store();
        let mut set: BTreeMap<NodeId, DiffValidator> = BTreeMap::new();
        apply_validator_weight_diffs(&store, &mut set, 2, 1, Id::EMPTY).expect("apply diffs");
        assert_eq!(
            set.get(&node).map(|v| v.weight),
            Some(weight),
            "un-applying the height-2 decrease should restore the staker's weight"
        );
    }

    /// `block_oracle`: accepting the **abort** child of the same proposal keeps the
    /// staker removed but un-mints the reward (supply decreases) and writes no
    /// reward UTXO (the abort diff).
    #[test]
    fn block_oracle_abort_accept_unmints_reward() {
        let node = NodeId::from([9; 20]);
        let end = unix(3_000);
        let weight = 2_000 * AVAX;
        let reward = 38_944;

        let tx = staker_tx(weight);
        let staker_tx_id = tx.id();

        let state = genesis_state(end, 100_000_000 * AVAX, |s| {
            let staker = Staker::new_current(
                staker_tx_id,
                node,
                None,
                Id::EMPTY,
                weight,
                unix(1_000),
                end,
                reward,
                Priority::PrimaryNetworkValidatorCurrent,
            );
            s.put_current_validator(staker).expect("put current");
            s.add_tx(staker_tx_id, tx.bytes().to_vec());
        });
        let supply_before = state.current_supply(Id::EMPTY).unwrap();

        let notifier = Arc::new(RecordingNotifier::default());
        let mut mgr = manager(state, Arc::clone(&notifier));

        let proposal = {
            let mut blk = Block::new(BlockBody::BanffProposal(BanffProposalBlock {
                time: 3_000,
                transactions: vec![],
                apricot: ApricotProposalBlock {
                    common: CommonBlock {
                        parent_id: genesis_id(),
                        height: 1,
                    },
                    tx: Tx::new(UnsignedTx::RewardValidator(RewardValidatorTx {
                        tx_id: staker_tx_id,
                    })),
                },
            }));
            blk.initialize(mgr.codec()).expect("init proposal");
            blk
        };

        mgr.verify(&proposal).expect("verify proposal");
        let (_commit, abort) = mgr.options(&proposal).expect("options");
        mgr.verify(&abort).expect("verify abort");
        mgr.accept(&proposal).expect("accept proposal");
        mgr.accept(&abort).expect("accept abort");

        let s = mgr.state();
        assert!(s.get_current_validator(Id::EMPTY, node).is_err());
        // Abort un-mints: supply drops by the potential reward; no reward UTXO.
        assert_eq!(s.current_supply(Id::EMPTY).unwrap(), supply_before - reward);
        assert!(s.get_reward_utxos(staker_tx_id).is_empty());
        assert_eq!(mgr.last_accepted(), abort.id());
        assert_eq!(s.height(), 2);
    }

    /// `accept_writes_diffs`: accepting a standard block flushes its on-accept diff
    /// to `State` (a newly-added current validator), writes the weight + public-key
    /// diffs at the block height (M4.14), records the block + advances the
    /// last-accepted / height singletons, and notifies the validator manager
    /// (Go `acceptor_test.go` / `standard_block_test.go`).
    #[test]
    fn accept_writes_diffs_flushes_state_and_writes_height_diffs() {
        let node = NodeId::from([5; 20]);
        let weight = 1_000 * AVAX;

        let state = genesis_state(unix(1_000), 100_000_000 * AVAX, |_s| {});
        let notifier = Arc::new(RecordingNotifier::default());
        let mut mgr = manager(state, Arc::clone(&notifier));

        // A Banff standard block at height 1 over genesis. Rather than synthesize a
        // fully-signed staking tx (the standard executor's verification surface is
        // exercised by M4.16), we seed the verified on-accept diff directly — the
        // contract under test is the *acceptor*: that it flushes the diff and writes
        // the per-height staker diffs (Go acceptor_test seeds blkIDToState likewise).
        let blk = {
            let mut b = Block::new(BlockBody::BanffStandard(BanffStandardBlock {
                time: 1_000,
                apricot: ApricotStandardBlock {
                    common: CommonBlock {
                        parent_id: genesis_id(),
                        height: 1,
                    },
                    transactions: vec![],
                },
            }));
            b.initialize(mgr.codec()).expect("init standard");
            b
        };

        let mut diff = mgr.new_diff(genesis_id()).expect("diff");
        diff.put_current_validator(Staker::new_current(
            Id::from([7; 32]),
            node,
            None,
            Id::EMPTY,
            weight,
            unix(1_000),
            unix(9_000),
            0,
            Priority::PrimaryNetworkValidatorCurrent,
        ))
        .expect("put current in diff");
        mgr.cache(
            blk.id(),
            BlockState {
                height: 1,
                on_accept: Some(Arc::new(diff)),
                on_commit: None,
                on_abort: None,
                timestamp: 1_000,
                prefers_commit: true,
            },
        );

        mgr.accept(&blk).expect("accept standard");

        let s = mgr.state();
        // Diff flushed: the validator is now in the persisted current set.
        let got = s.get_current_validator(Id::EMPTY, node).expect("validator");
        assert_eq!(got.weight, weight);

        // Singletons / block store advanced.
        assert_eq!(mgr.last_accepted(), blk.id());
        assert_eq!(s.last_accepted(), blk.id());
        assert_eq!(s.height(), 1);
        assert_eq!(s.get_block_id_at_height(1), Some(blk.id()));
        assert!(s.get_block(blk.id()).is_ok());

        // The validator manager was notified.
        assert_eq!(notifier.count.load(Ordering::SeqCst), 1);
        assert_eq!(*notifier.last.lock(), blk.id());

        // The weight diff was written at height 1: reconstructing the set at
        // height 0 from the current set un-applies the increase, removing the node.
        let store = s.weight_diff_store();
        let mut set: BTreeMap<NodeId, DiffValidator> = BTreeMap::new();
        set.insert(
            node,
            DiffValidator {
                weight,
                public_key: None,
            },
        );
        apply_validator_weight_diffs(&store, &mut set, 1, 0, Id::EMPTY).expect("apply diffs");
        assert!(
            !set.contains_key(&node),
            "un-applying the height-1 increase should remove the node"
        );
    }

    /// `accept_writes_diffs`: the non-verifying acceptor path (bootstrap, 19 §2)
    /// accepts a fetched standard block *without* a prior `verify` call, flushing
    /// its diff and advancing the singletons just like the verifying path.
    #[test]
    fn accept_writes_diffs_non_verifying_bootstrap_path() {
        let state = genesis_state(unix(1_000), 100_000_000 * AVAX, |_s| {});
        let notifier = Arc::new(RecordingNotifier::default());
        let mut mgr = manager(state, Arc::clone(&notifier));

        // An empty Banff standard block performs an advance-time change, which is a
        // valid state change; verify it via the bootstrap path (no explicit verify).
        let blk = {
            let mut b = Block::new(BlockBody::BanffStandard(BanffStandardBlock {
                time: 2_000,
                apricot: ApricotStandardBlock {
                    common: CommonBlock {
                        parent_id: genesis_id(),
                        height: 1,
                    },
                    transactions: vec![],
                },
            }));
            b.initialize(mgr.codec()).expect("init standard");
            b
        };

        mgr.accept_non_verifying(&blk).expect("bootstrap accept");

        let s = mgr.state();
        assert_eq!(mgr.last_accepted(), blk.id());
        assert_eq!(s.height(), 1);
        // The advance-time change is reflected: chain time moved to the block time.
        assert_eq!(
            s.timestamp().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            2_000
        );
        assert_eq!(notifier.count.load(Ordering::SeqCst), 1);
    }
}
