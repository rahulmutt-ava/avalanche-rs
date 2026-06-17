// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The executor [`Backend`] context (`vms/platformvm/txs/executor/backend.go`,
//! specs 08 §2.4).
//!
//! Go's `Backend` carries the node-wide handles a tx executor needs: the chain
//! `Config` (fork schedule + staking parameters), the `snow.Context` (network /
//! asset / node ids), the `fx.Fx` spend gate, the `FlowChecker`, the reward
//! `Calculator`, and the `Bootstrapped` flag. The Rust port collapses the parts
//! the M4.16 standard executor (and its M4.17/M4.18/M4.19 siblings) actually
//! consume into a single self-contained struct so the executors do not depend on
//! the not-yet-ported `config.Internal` / `snow.Context` types.
//!
//! The fork schedule is expressed as activation [`SystemTime`]s (`Durango` /
//! `Etna`) compared against the chain timestamp, mirroring Go's
//! `UpgradeConfig.Is<Fork>Activated(t)`.

use std::time::SystemTime;

use ava_secp256k1fx::Fx;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::reward;
use crate::txs::fee::simple_calculator::StaticFeeConfig;

/// Network-upgrade activation schedule (`upgrade.Config`, the subset the
/// executor consults; specs 08 §6).
///
/// A fork is active iff the chain timestamp is `>=` its activation time. An
/// activation time of [`SystemTime::UNIX_EPOCH`] therefore means "always active".
#[derive(Clone, Copy, Debug)]
pub struct UpgradeSchedule {
    /// `DurangoTime` — when the Durango upgrade activates.
    pub durango_time: SystemTime,
    /// `EtnaTime` — when the Etna upgrade activates.
    pub etna_time: SystemTime,
    /// `HeliconTime` — when the Helicon upgrade (ACP-236 auto-renew) activates.
    /// Unscheduled on every live network (year-9999), so this is the far future
    /// for all the production-config constructors below.
    pub helicon_time: SystemTime,
}

impl UpgradeSchedule {
    /// All forks active from genesis (every activation time is the epoch).
    #[must_use]
    pub const fn all_active() -> Self {
        Self {
            durango_time: SystemTime::UNIX_EPOCH,
            etna_time: SystemTime::UNIX_EPOCH,
            helicon_time: SystemTime::UNIX_EPOCH,
        }
    }

    /// Durango active from genesis, Etna inactive (the pre-Etna static-fee
    /// regime with the post-Durango immediate-current staker model).
    #[must_use]
    pub fn durango_only() -> Self {
        let far = SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(100 * 365 * 24 * 60 * 60))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Self {
            durango_time: SystemTime::UNIX_EPOCH,
            etna_time: far,
            helicon_time: far,
        }
    }

    /// All forks inactive (every activation time is "the far future",
    /// represented by [`SystemTime`]'s max as approximated by a 100-year offset).
    /// Useful for Apricot/Banff-era conformance fixtures.
    #[must_use]
    pub fn none_active() -> Self {
        // ~100 years past the epoch — comfortably after any test chain time.
        let far = SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(100 * 365 * 24 * 60 * 60))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Self {
            durango_time: far,
            etna_time: far,
            helicon_time: far,
        }
    }

    /// `UpgradeConfig.IsDurangoActivated(t)`.
    #[must_use]
    pub fn is_durango_activated(&self, t: SystemTime) -> bool {
        t >= self.durango_time
    }

    /// `UpgradeConfig.IsEtnaActivated(t)`.
    #[must_use]
    pub fn is_etna_activated(&self, t: SystemTime) -> bool {
        t >= self.etna_time
    }

    /// `UpgradeConfig.IsHeliconActivated(t)`.
    #[must_use]
    pub fn is_helicon_activated(&self, t: SystemTime) -> bool {
        t >= self.helicon_time
    }
}

/// Primary-Network / subnet staking parameters (`config.Internal`, the staking
/// subset; specs 08 §3.3, 21 §3).
///
/// These bound the weight and duration of a staker and drive the
/// reward/over-delegation checks in [`staker_tx_verification`](super::staker_tx_verification).
#[derive(Clone, Copy, Debug)]
pub struct StakingConfig {
    /// `MinValidatorStake` — minimum Primary-Network validator weight.
    pub min_validator_stake: u64,
    /// `MaxValidatorStake` — maximum Primary-Network validator weight.
    pub max_validator_stake: u64,
    /// `MinDelegatorStake` — minimum delegator weight.
    pub min_delegator_stake: u64,
    /// `MinDelegationFee` — minimum delegation-fee share (millionths).
    pub min_delegation_fee: u32,
    /// `MinStakeDuration` — minimum staking duration.
    pub min_stake_duration: std::time::Duration,
    /// `MaxStakeDuration` — maximum staking duration.
    pub max_stake_duration: std::time::Duration,
    /// `RewardConfig` — Primary-Network minting parameters.
    pub reward_config: reward::Config,
}

impl StakingConfig {
    /// The canonical mainnet staking parameters (specs 08 §3.3, 21 §3).
    ///
    /// `MinValidatorStake = 2 000 AVAX`, `MaxValidatorStake = 3 000 000 AVAX`,
    /// `MinDelegatorStake = 25 AVAX`, `MinDelegationFee = 2%`,
    /// `MinStakeDuration = 2 weeks`, `MaxStakeDuration = 365 days`.
    #[must_use]
    pub fn mainnet() -> Self {
        const AVAX: u64 = 1_000_000_000;
        Self {
            min_validator_stake: 2_000 * AVAX,
            max_validator_stake: 3_000_000 * AVAX,
            min_delegator_stake: 25 * AVAX,
            // 2% expressed over reward::PERCENT_DENOMINATOR (1e6).
            min_delegation_fee: 20_000,
            min_stake_duration: std::time::Duration::from_secs(2 * 7 * 24 * 60 * 60),
            max_stake_duration: std::time::Duration::from_secs(365 * 24 * 60 * 60),
            reward_config: reward::Config::mainnet(),
        }
    }
}

/// `executor.Backend` — the node-wide context a tx executor reads.
///
/// Shared by the M4.16 [`StandardTxExecutor`](super::StandardTxExecutor) and the
/// M4.17/M4.18/M4.19 sibling executors. Holds the fork schedule, the staking +
/// fee config, the chain identifiers, the fx spend gate, and the bootstrapped
/// flag. It owns no state (the executor mutates a `Diff`), so it is shareable by
/// reference across all visitors of a block.
pub struct Backend {
    /// The network-upgrade activation schedule.
    pub upgrades: UpgradeSchedule,
    /// The staking parameters.
    pub staking: StakingConfig,
    /// The static (pre-Etna) per-network fee config.
    pub static_fee_config: StaticFeeConfig,
    /// `Ctx.NetworkID` — the network this chain belongs to.
    pub network_id: u32,
    /// `Ctx.ChainID` — the P-Chain's own blockchain id.
    pub chain_id: Id,
    /// `Ctx.AVAXAssetID` — the AVAX asset id (the fee asset).
    pub avax_asset_id: Id,
    /// `Ctx.NodeID` — this node's id (for the partial-sync health warning).
    pub node_id: NodeId,
    /// `Fx` — the secp256k1 spend gate used to authorize subnet/owner actions.
    pub fx: Fx,
    /// `Bootstrapped` — when `false`, the heavier semantic checks (start-time,
    /// staker overlap, flow check, shared-memory) are skipped, mirroring Go.
    pub bootstrapped: bool,
}

impl Backend {
    /// `UpgradeConfig.IsDurangoActivated(t)`.
    #[must_use]
    pub fn is_durango_activated(&self, t: SystemTime) -> bool {
        self.upgrades.is_durango_activated(t)
    }

    /// `UpgradeConfig.IsEtnaActivated(t)`.
    #[must_use]
    pub fn is_etna_activated(&self, t: SystemTime) -> bool {
        self.upgrades.is_etna_activated(t)
    }

    /// `UpgradeConfig.IsHeliconActivated(t)`.
    #[must_use]
    pub fn is_helicon_activated(&self, t: SystemTime) -> bool {
        self.upgrades.is_helicon_activated(t)
    }
}
