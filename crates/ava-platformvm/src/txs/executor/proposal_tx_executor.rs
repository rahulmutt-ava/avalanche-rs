// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `executor.ProposalTx` — the proposal-tx executor / commit-abort oracle
//! (`vms/platformvm/txs/executor/proposal_tx_executor.go`, specs 08 §2.4 / §4.2).
//!
//! [`ProposalTxExecutor`] is a [`Visitor`](crate::txs::Visitor) that, unlike the
//! decision-tx [`StandardTxExecutor`](super::StandardTxExecutor), mutates **two**
//! [`Diff`]s at once — `on_commit` and `on_abort` — so the consensus layer can
//! materialize the commit and abort children of an Apricot/Banff proposal block
//! and choose between them (specs 08 §4.2). It overrides only the two Apricot
//! proposal txs:
//!
//! - [`advance_time`](ProposalTxExecutor::advance_time) — the Apricot-only
//!   [`AdvanceTimeTx`]: advance `on_commit` to the proposed time via
//!   [`advance_time_to`](super::advance_time::advance_time_to). The abort state is
//!   unchanged (an aborted advance-time leaves the clock where it was).
//! - [`reward_validator`](ProposalTxExecutor::reward_validator) — the
//!   [`RewardValidatorTx`]: pop the earliest-ending current staker, verify it is
//!   the one named by the tx and that its end time is now, then on **commit** pay
//!   the potential reward (mint already accrued at promotion; here we refund the
//!   stake + write reward UTXOs) and on **abort** *un-mint* the reward by
//!   decreasing the subnet supply. The staker is removed from both states.
//!
//! ## The reward-staker resolver
//!
//! Go fetches the staker's originating tx via `state.GetTx` to recover its stake
//! outputs and rewards owner. The Rust [`Chain`](crate::state::chain::Chain)
//! surface has no tx store yet (that wiring is the block manager's, M4.20), so —
//! exactly as the standard executor defers `Config.CreateChain` to its caller —
//! the reward path takes a [`StakerTxResolver`] supplied at construction that
//! maps a staker tx id to its [`RewardedStakerTx`]. Tests inject a closure; the
//! block manager will inject the real `GetTx`-backed lookup.
//!
//! ## `prefers_commit`
//!
//! [`ProposalTxExecutor::prefers_commit`] reports the executor's commit/abort
//! preference. `AdvanceTimeTx` always prefers commit (the time validity is
//! checked before execution). `RewardValidatorTx`'s real preference is the
//! validator's measured uptime, computed at the block layer (M4.20); the executor
//! defaults to `true` and the block layer overrides it.

use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::diff::Diff;
use crate::txs::components::{Owner, TransferableOutput};
use crate::txs::{AdvanceTimeTx, Priority, RewardValidatorTx, Visitor};
use crate::utxo::Utxo;

use super::advance_time::advance_time_to;
use super::backend::Backend;

/// The reward-relevant fields of a permissionless staker's originating tx,
/// recovered by a [`StakerTxResolver`] (Go `state.GetTx` →
/// `txs.ValidatorTx`/`txs.DelegatorTx`).
#[derive(Clone, Debug)]
pub struct RewardedStakerTx {
    /// `Outputs()` — the tx's base `BaseTx.Outs` (the change UTXOs already
    /// produced when the staker was added). Their count is the offset at which
    /// the refunded stake / reward UTXOs are indexed.
    pub outputs: Vec<TransferableOutput>,
    /// `Stake()` — the staked outputs, refunded when the staker leaves.
    pub stake: Vec<TransferableOutput>,
    /// `ValidationRewardsOwner()` — the owner that receives the reward UTXO.
    pub validation_rewards_owner: Owner,
}

/// Resolves a staker tx id to its [`RewardedStakerTx`] (Go `state.GetTx`).
///
/// `None` means the tx is absent (the executor surfaces [`Error::Database`]).
pub type StakerTxResolver<'a> = dyn Fn(&Id) -> Option<RewardedStakerTx> + 'a;

/// `proposalTxExecutor` — the [`Visitor`] that executes a proposal tx against a
/// commit/abort pair of [`Diff`]s.
pub struct ProposalTxExecutor<'a> {
    backend: &'a Backend,
    /// The state if the proposal is committed.
    on_commit: &'a mut Diff,
    /// The state if the proposal is aborted.
    on_abort: &'a mut Diff,
    /// The number of credentials on the signed tx (proposal txs take none).
    num_credentials: usize,
    /// Resolves a staker tx id to its reward outputs/owner.
    resolver: &'a StakerTxResolver<'a>,
    /// The executor's commit/abort preference (see the module docs).
    prefers_commit: bool,
}

impl<'a> ProposalTxExecutor<'a> {
    /// Builds an executor over the `on_commit`/`on_abort` diff pair.
    ///
    /// `num_credentials` is the count of credentials on the signed tx (a proposal
    /// tx must carry none); `resolver` recovers a staker tx's reward outputs/owner
    /// for the reward path.
    pub fn new(
        backend: &'a Backend,
        on_commit: &'a mut Diff,
        on_abort: &'a mut Diff,
        num_credentials: usize,
        resolver: &'a StakerTxResolver<'a>,
    ) -> Self {
        Self {
            backend,
            on_commit,
            on_abort,
            num_credentials,
            resolver,
            prefers_commit: true,
        }
    }

    /// The executor's commit/abort preference after a successful run.
    #[must_use]
    pub fn prefers_commit(&self) -> bool {
        self.prefers_commit
    }

    /// Adds `utxo` to both the commit and abort states (Go's stake-refund, which
    /// is paid back regardless of the commit/abort outcome).
    fn add_utxo_both(&mut self, utxo: &Utxo) -> Result<()> {
        let bytes = utxo.marshal()?;
        self.on_commit.add_utxo(utxo.input_id(), bytes.clone());
        self.on_abort.add_utxo(utxo.input_id(), bytes);
        Ok(())
    }

    /// `rewardValidatorTx` — refund the stake (both states), and on commit pay
    /// the potential reward as a reward UTXO. The supply mint already happened at
    /// promotion ([`advance_time_to`]); the abort path un-mints it in
    /// [`reward_validator`](Self::reward_validator).
    fn reward_staker(
        &mut self,
        tx_id: Id,
        potential_reward: u64,
        staker_tx: &RewardedStakerTx,
        avax_asset_id: Id,
    ) -> Result<()> {
        let outputs_len = u32::try_from(staker_tx.outputs.len()).map_err(|_| Error::Overflow)?;
        let stake_len = u32::try_from(staker_tx.stake.len()).map_err(|_| Error::Overflow)?;

        // Refund each staked output at index len(outputs) + i, in both states.
        for (i, out) in staker_tx.stake.iter().enumerate() {
            let i = u32::try_from(i).map_err(|_| Error::Overflow)?;
            let output_index = outputs_len.checked_add(i).ok_or(Error::Overflow)?;
            let utxo = Utxo {
                tx_id,
                output_index,
                asset_id: out.asset_id,
                out: out.out.clone(),
            };
            self.add_utxo_both(&utxo)?;
        }

        // Pay the reward on commit only (Go writes nothing on abort here).
        if potential_reward > 0 {
            let output_index = outputs_len.checked_add(stake_len).ok_or(Error::Overflow)?;
            let Owner::Secp256k1(owners) = &staker_tx.validation_rewards_owner;
            let reward_out = TransferableOutput {
                asset_id: avax_asset_id,
                out: crate::txs::components::Output::Transfer(
                    ava_secp256k1fx::TransferOutput::new(potential_reward, owners.clone()),
                ),
            };
            let utxo = Utxo {
                tx_id,
                output_index,
                asset_id: avax_asset_id,
                out: reward_out.out,
            };
            let bytes = utxo.marshal()?;
            self.on_commit.add_utxo(utxo.input_id(), bytes.clone());
            self.on_commit.add_reward_utxo(tx_id, bytes);
        }
        Ok(())
    }
}

impl Visitor for ProposalTxExecutor<'_> {
    type Error = Error;

    fn advance_time(&mut self, tx: &AdvanceTimeTx) -> Result<()> {
        // A proposal tx carries no credentials.
        if self.num_credentials != 0 {
            return Err(Error::WrongNumberOfCredentials);
        }

        let new_time = std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(tx.time))
            .ok_or(Error::Overflow)?;

        // Note: the abort state's clock is left unchanged — an aborted
        // advance-time leaves chain time where it was.
        advance_time_to(self.backend, self.on_commit, new_time)?;
        self.prefers_commit = true;
        Ok(())
    }

    fn reward_validator(&mut self, tx: &RewardValidatorTx) -> Result<()> {
        if tx.tx_id == Id::EMPTY {
            return Err(Error::NilTx);
        }
        if self.num_credentials != 0 {
            return Err(Error::WrongNumberOfCredentials);
        }

        // The next staker to leave is the earliest-ending current staker.
        let current = self.on_commit.current_stakers();
        let staker_to_reward = current
            .first()
            .ok_or(Error::Database(ava_database::error::Error::NotFound))?;

        if staker_to_reward.tx_id != tx.tx_id {
            return Err(Error::RemoveWrongStaker);
        }

        // The chain timestamp must equal the staker's end time.
        if staker_to_reward.end_time != self.on_commit.timestamp() {
            return Err(Error::RemoveStakerTooEarly);
        }

        // Permissioned subnet validators are removed by the advancement of time,
        // so only permissionless stakers should remain to be rewarded here.
        if staker_to_reward.priority == Priority::SubnetPermissionedValidatorCurrent {
            return Err(Error::RemovePermissionlessValidator);
        }

        let staker_tx = (self.resolver)(&staker_to_reward.tx_id)
            .ok_or(Error::Database(ava_database::error::Error::NotFound))?;

        let staker = staker_to_reward.clone();
        self.reward_staker(
            staker.tx_id,
            staker.potential_reward,
            &staker_tx,
            self.backend.avax_asset_id,
        )?;

        // Remove the staker from both states.
        if staker.priority.is_current_validator() {
            self.on_commit.delete_current_validator(&staker);
            self.on_abort.delete_current_validator(&staker);
        } else {
            self.on_commit.delete_current_delegator(&staker);
            self.on_abort.delete_current_delegator(&staker);
        }

        // On abort the reward is not awarded, so the supply minted at promotion
        // must be returned (decreased) in the abort state.
        let abort_supply = self.on_abort.current_supply(staker.subnet_id)?;
        let new_supply = abort_supply
            .checked_sub(staker.potential_reward)
            .ok_or(Error::Overflow)?;
        self.on_abort
            .set_current_supply(staker.subnet_id, new_supply);

        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod proposal_executor {
    //! M4.17 conformance tests — ported from Go `advance_time_test.go` +
    //! `reward_validator_test.go` (the cases buildable against the in-crate
    //! `Diff`/`State` surface, without the full VM environment / tx store).
    //!
    //! Each test builds a base `State`, layers a commit/abort `Diff` pair over it,
    //! runs the `ProposalTxExecutor`, and asserts the two resulting diffs plus the
    //! commit preference: supply mint on commit (advance-time promotion), no mint
    //! on abort (reward un-mint), and staker promotion/removal order.

    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use ava_database::MemDb;
    use ava_secp256k1fx::{OutputOwners, TransferOutput};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_types::short_id::ShortId;

    use super::*;
    use crate::state::chain::{Chain, Versions};
    use crate::state::staker::Staker;
    use crate::state::state::State;
    use crate::txs::components::Output;
    use crate::txs::executor::backend::{StakingConfig, UpgradeSchedule};
    use crate::txs::{AdvanceTimeTx, RewardValidatorTx, UnsignedTx};

    const AVAX_ASSET: [u8; 32] = [0x42; 32];
    const AVAX: u64 = 1_000_000_000;

    /// A `Versions` resolving exactly one parent block id to its `Chain` view.
    struct SingleParent {
        id: Id,
        chain: Arc<dyn Chain>,
    }
    impl Versions for SingleParent {
        fn get_state(&self, block_id: Id) -> Option<Arc<dyn Chain>> {
            (block_id == self.id).then(|| Arc::clone(&self.chain))
        }
    }

    fn owners(addr: u8) -> OutputOwners {
        OutputOwners::new(0, 1, vec![ShortId::from([addr; 20])])
    }

    fn unix(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// A test backend (pre-Etna by default via `durango_only`), mainnet staking.
    fn backend(upgrades: UpgradeSchedule) -> Backend {
        Backend {
            upgrades,
            staking: StakingConfig::mainnet(),
            static_fee_config: crate::txs::fee::simple_calculator::StaticFeeConfig::MAINNET,
            network_id: 1,
            chain_id: Id::EMPTY,
            avax_asset_id: Id::from(AVAX_ASSET),
            node_id: NodeId::EMPTY,
            fx: ava_secp256k1fx::Fx::new(Arc::new(ava_utils::clock::MockClock::at(
                SystemTime::UNIX_EPOCH,
            ))),
            bootstrapped: true,
        }
    }

    /// Builds the base `State` (chain time `ts`, primary supply `supply`),
    /// applies `seed` to it, and returns it wrapped as a parent `Chain`.
    fn base_state(
        ts: SystemTime,
        supply: u64,
        seed: impl FnOnce(&mut State<MemDb>),
    ) -> State<MemDb> {
        let mut state = State::new(MemDb::new()).expect("state");
        state.set_timestamp(ts);
        state.set_current_supply(Id::EMPTY, supply);
        seed(&mut state);
        state
    }

    /// Layers a fresh commit/abort `Diff` pair over `base`.
    fn diff_pair(base: State<MemDb>) -> (Diff, Diff, Arc<dyn Chain>) {
        let parent_id = Id::from([0xAB; 32]);
        let chain: Arc<dyn Chain> = Arc::new(base);
        let versions = SingleParent {
            id: parent_id,
            chain: Arc::clone(&chain),
        };
        let commit = Diff::new(parent_id, &versions).expect("commit diff");
        let abort = Diff::new(parent_id, &versions).expect("abort diff");
        (commit, abort, chain)
    }

    /// A no-op resolver (the advance-time tests do not touch the reward path).
    fn no_resolver(_id: &Id) -> Option<RewardedStakerTx> {
        None
    }

    /// `TestAdvanceTimeTxUpdatePrimaryNetworkStakers` (slice): advancing time past
    /// a pending primary-network validator's start promotes it to the current set
    /// and mints its potential reward into the supply on the commit state, while
    /// the abort state is untouched.
    #[test]
    fn proposal_executor_advance_time_promotes_and_mints() {
        let node = NodeId::from([7; 20]);
        let staker_tx = Id::from([1; 32]);
        let start = unix(1_000);
        let end = unix(1_000 + 200 * 24 * 60 * 60);
        let new_time = unix(1_001); // strictly after `start`.

        let base = base_state(unix(900), 100_000_000 * AVAX, |s| {
            // A pending primary-network validator that becomes current at `start`.
            let pending = Staker::new_pending(
                staker_tx,
                node,
                None,
                Id::EMPTY,
                2_000 * AVAX,
                start,
                end,
                Priority::PrimaryNetworkValidatorPending,
            );
            s.put_pending_validator(pending).expect("put pending");
        });
        let supply_before = base.current_supply(Id::EMPTY).unwrap();
        let (mut commit, mut abort, _chain) = diff_pair(base);

        let b = backend(UpgradeSchedule::durango_only());
        let resolver: &StakerTxResolver = &no_resolver;
        let mut exec = ProposalTxExecutor::new(&b, &mut commit, &mut abort, 0, resolver);
        let tx = AdvanceTimeTx {
            time: new_time
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        UnsignedTx::AdvanceTime(tx)
            .visit(&mut exec)
            .expect("advance time");
        assert!(exec.prefers_commit());

        // Commit: the validator is now current and the supply grew by the reward.
        assert!(commit.get_current_validator(Id::EMPTY, node).is_ok());
        let supply_after = commit.current_supply(Id::EMPTY).unwrap();
        assert!(supply_after > supply_before, "supply should mint on commit");
        assert_eq!(commit.timestamp(), new_time);

        // Abort: clock and supply are unchanged (no promotion applied).
        assert_eq!(abort.current_supply(Id::EMPTY).unwrap(), supply_before);
        assert_eq!(abort.timestamp(), unix(900));
    }

    /// `TestAdvanceTimeTxRemoveSubnetValidator` (slice): a current permissioned
    /// subnet validator whose end time has passed is removed from the current set
    /// when time advances (permissionless stakers are *not* removed here).
    #[test]
    fn proposal_executor_advance_time_removes_permissioned_subnet() {
        let node = NodeId::from([8; 20]);
        let subnet = Id::from([0x55; 32]);
        let staker_tx = Id::from([2; 32]);
        let end = unix(2_000);
        let new_time = unix(2_000); // == end.

        let base = base_state(unix(1_500), 100_000_000 * AVAX, |s| {
            let current = Staker::new_current(
                staker_tx,
                node,
                None,
                subnet,
                10,
                unix(500),
                end,
                0,
                Priority::SubnetPermissionedValidatorCurrent,
            );
            s.put_current_validator(current).expect("put current");
        });
        let (mut commit, mut abort, _chain) = diff_pair(base);

        let b = backend(UpgradeSchedule::durango_only());
        let resolver: &StakerTxResolver = &no_resolver;
        let mut exec = ProposalTxExecutor::new(&b, &mut commit, &mut abort, 0, resolver);
        let tx = AdvanceTimeTx {
            time: new_time
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        UnsignedTx::AdvanceTime(tx)
            .visit(&mut exec)
            .expect("advance time");

        // Commit: the permissioned subnet validator is gone.
        assert!(commit.get_current_validator(subnet, node).is_err());
        // Abort: still present (no removal applied).
        assert!(abort.get_current_validator(subnet, node).is_ok());
    }

    /// `TestAdvanceTimeTxTimestampTooEarly`-adjacent: a credentialed proposal tx
    /// is rejected (proposal txs carry no credentials).
    #[test]
    fn proposal_executor_advance_time_rejects_credentials() {
        let base = base_state(unix(900), 100_000_000 * AVAX, |_s| {});
        let (mut commit, mut abort, _chain) = diff_pair(base);
        let b = backend(UpgradeSchedule::durango_only());
        let resolver: &StakerTxResolver = &no_resolver;
        let mut exec = ProposalTxExecutor::new(&b, &mut commit, &mut abort, 1, resolver);
        let err = UnsignedTx::AdvanceTime(AdvanceTimeTx { time: 1_000 })
            .visit(&mut exec)
            .expect_err("should reject creds");
        assert!(
            matches!(err, Error::WrongNumberOfCredentials),
            "got {err:?}"
        );
    }

    /// Seeds a current primary-network validator due to leave at `end`, plus the
    /// resolver row recovering its stake outputs + rewards owner.
    fn seed_current_validator(
        s: &mut State<MemDb>,
        staker_tx: Id,
        node: NodeId,
        end: SystemTime,
        weight: u64,
        potential_reward: u64,
    ) {
        let staker = Staker::new_current(
            staker_tx,
            node,
            None,
            Id::EMPTY,
            weight,
            unix(500),
            end,
            potential_reward,
            Priority::PrimaryNetworkValidatorCurrent,
        );
        s.put_current_validator(staker).expect("put current");
    }

    fn rewarded_staker_tx(weight: u64) -> RewardedStakerTx {
        RewardedStakerTx {
            outputs: vec![],
            stake: vec![TransferableOutput {
                asset_id: Id::from(AVAX_ASSET),
                out: Output::Transfer(TransferOutput::new(weight, owners(1))),
            }],
            validation_rewards_owner: Owner::Secp256k1(owners(1)),
        }
    }

    /// `TestRewardValidatorTxExecuteOnCommit`/`OnAbort` (happy path): on commit the
    /// stake is refunded and the reward UTXO is written (supply unchanged — the
    /// mint happened at promotion); on abort the stake is refunded but the reward
    /// is *not* paid and the supply is decreased by the potential reward. The
    /// staker is removed from both states.
    #[test]
    fn proposal_executor_reward_validator_commit_and_abort() {
        let node = NodeId::from([9; 20]);
        let staker_tx = Id::from([3; 32]);
        let end = unix(3_000);
        let weight = 2_000 * AVAX;
        let reward = 38_944; // an arbitrary nonzero potential reward.

        let base = base_state(end, 100_000_000 * AVAX, |s| {
            seed_current_validator(s, staker_tx, node, end, weight, reward);
        });
        let supply_before = base.current_supply(Id::EMPTY).unwrap();
        let (mut commit, mut abort, _chain) = diff_pair(base);

        let b = backend(UpgradeSchedule::durango_only());
        let resolve = |id: &Id| -> Option<RewardedStakerTx> {
            (*id == staker_tx).then(|| rewarded_staker_tx(weight))
        };
        let resolver: &StakerTxResolver = &resolve;
        let mut exec = ProposalTxExecutor::new(&b, &mut commit, &mut abort, 0, resolver);
        UnsignedTx::RewardValidator(RewardValidatorTx { tx_id: staker_tx })
            .visit(&mut exec)
            .expect("reward validator");

        // Both states removed the staker.
        assert!(commit.get_current_validator(Id::EMPTY, node).is_err());
        assert!(abort.get_current_validator(Id::EMPTY, node).is_err());

        // Commit: supply unchanged (mint already accrued), a reward UTXO recorded.
        assert_eq!(commit.current_supply(Id::EMPTY).unwrap(), supply_before);
        assert_eq!(commit.get_reward_utxos(staker_tx).len(), 1);

        // Abort: supply decreased by the potential reward, no reward UTXO.
        assert_eq!(
            abort.current_supply(Id::EMPTY).unwrap(),
            supply_before - reward
        );
        assert!(abort.get_reward_utxos(staker_tx).is_empty());
    }

    /// `reward_validator_test.go` Case 2: a `RewardValidatorTx` naming a staker
    /// other than the next-to-leave is rejected with `RemoveWrongStaker`.
    #[test]
    fn proposal_executor_reward_validator_wrong_staker() {
        let node = NodeId::from([9; 20]);
        let staker_tx = Id::from([3; 32]);
        let end = unix(3_000);

        let base = base_state(end, 100_000_000 * AVAX, |s| {
            seed_current_validator(s, staker_tx, node, end, 2_000 * AVAX, 0);
        });
        let (mut commit, mut abort, _chain) = diff_pair(base);

        let b = backend(UpgradeSchedule::durango_only());
        let resolver: &StakerTxResolver = &no_resolver;
        let mut exec = ProposalTxExecutor::new(&b, &mut commit, &mut abort, 0, resolver);
        let err = UnsignedTx::RewardValidator(RewardValidatorTx {
            tx_id: Id::from([0x77; 32]),
        })
        .visit(&mut exec)
        .expect_err("wrong staker");
        assert!(matches!(err, Error::RemoveWrongStaker), "got {err:?}");
    }

    /// `reward_validator_test.go` Case 1: a correct `RewardValidatorTx` is
    /// rejected with `RemoveStakerTooEarly` when the chain time has not reached the
    /// staker's end time.
    #[test]
    fn proposal_executor_reward_validator_too_early() {
        let node = NodeId::from([9; 20]);
        let staker_tx = Id::from([3; 32]);
        let end = unix(3_000);

        // Chain time is before `end`.
        let base = base_state(unix(2_500), 100_000_000 * AVAX, |s| {
            seed_current_validator(s, staker_tx, node, end, 2_000 * AVAX, 0);
        });
        let (mut commit, mut abort, _chain) = diff_pair(base);

        let b = backend(UpgradeSchedule::durango_only());
        let resolver: &StakerTxResolver = &no_resolver;
        let mut exec = ProposalTxExecutor::new(&b, &mut commit, &mut abort, 0, resolver);
        let err = UnsignedTx::RewardValidator(RewardValidatorTx { tx_id: staker_tx })
            .visit(&mut exec)
            .expect_err("too early");
        assert!(matches!(err, Error::RemoveStakerTooEarly), "got {err:?}");
    }
}
