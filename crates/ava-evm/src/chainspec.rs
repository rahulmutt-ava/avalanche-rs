// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `AvaChainSpec` / `AvaHardfork` / `revm_spec_id` (G7, spec 10 §7.4/§17.8).
//!
//! Avalanche interleaves **timestamp-activated** Avalanche phases
//! (Apricot Phase 1 → … → Granite) with the Ethereum forks coreth maps in, and
//! the revm [`SpecId`] for a block must be selected from *both*. This module
//! builds the bridge:
//!
//! * [`AvaPhase`] — the chronologically-ordered Avalanche phase set. `fork_at`
//!   returns the highest phase active at a timestamp; the per-phase `is_*`
//!   predicates gate feerules + precompile activation downstream.
//! * [`AvaHardfork`] — `Eth(EthereumHardfork)` plus every Avalanche phase. This
//!   is the unit stored in the reth [`ChainHardforks`] ordered list and the
//!   implementor of the reth [`Hardfork`] trait.
//! * [`AvaChainSpec`] — wraps a [`ChainHardforks`] (Ava fork logic), an inner
//!   reth [`ChainSpec`] (for the heavy [`EthChainSpec`] delegation), and the
//!   `network_upgrades` schedule, and exposes `revm_spec_id` / `check_compatible`.
//!
//! **Phase → Ethereum `SpecId` mapping (coreth `params/config_extra.go:SetEthUpgrades`).**
//! coreth enables the Ethereum upgrade at the same time as the Avalanche phase
//! that introduces it:
//!
//! | Avalanche phase                | Ethereum `SpecId` | coreth source                          |
//! |--------------------------------|-------------------|----------------------------------------|
//! | (pre-Apricot Phase 2)          | `ISTANBUL`        | Istanbul/MuirGlacier at block 0 (l.46) |
//! | Apricot Phase 2                | `BERLIN`          | `c.BerlinBlock` ← AP2 (l.54/57)        |
//! | Apricot Phase 3 … Cortina      | `LONDON`          | `c.LondonBlock` ← AP3 (l.55/58)        |
//! | Durango … (pre-Etna)           | `SHANGHAI`        | `c.ShanghaiTime` ← Durango (l.83)      |
//! | Etna, Fortuna, Granite         | `CANCUN`          | `c.CancunTime` ← Etna (l.87)           |
//!
//! coreth pins **no** Ethereum fork beyond Cancun (no `PragueTime`), so Fortuna
//! and Granite — which add Avalanche-only fee/consensus changes — keep the
//! `CANCUN` revm `SpecId`. See `tests/vectors/cchain/fork_schedule/_provenance.md`.

use chrono::{DateTime, Utc};

use ava_evm_reth::{
    AccountInfo, Address, AvaEvmError, B256, BaseFeeParams, BlobParams, BundleBuilder, BundleState,
    Bytecode, Bytes, Chain, ChainHardforks, ChainSpec, DepositContract, EMPTY_OMMER_ROOT_HASH,
    EthChainSpec, EthereumHardfork, EthereumHardforks, ForkCondition, Genesis, Hardfork, Header,
    KECCAK_EMPTY, NodeRecord, SpecId, StorageKeyMap, U256, keccak256,
};
use ava_version::upgrade::{Fork, UpgradeConfig, get_config};

use crate::block::AvaHeader;
use crate::error::Error;

/// The chronologically-ordered set of Avalanche network-upgrade phases that the
/// C-Chain EVM observes. Derives `Ord` so `phase >= AvaPhase::Cortina` is the
/// natural "at-or-after" relation used by [`AvaChainSpec::revm_spec_id`] and the
/// per-phase predicates. The discriminant order MUST stay chronological.
///
/// Mirrors `ava_version::upgrade::Fork`, but is C-Chain-local (it omits Helicon,
/// which coreth does not map to any Ethereum upgrade) and is the return type of
/// [`AvaChainSpec::fork_at`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AvaPhase {
    /// Pre-Apricot-Phase-1 launch state (genesis Ethereum forks, Istanbul-era).
    Launch,
    /// Apricot Phase 1.
    ApricotPhase1,
    /// Apricot Phase 2 — enables Berlin (`config_extra.go:54/57`).
    ApricotPhase2,
    /// Apricot Phase 3 — enables London (`config_extra.go:55/58`).
    ApricotPhase3,
    /// Apricot Phase 4.
    ApricotPhase4,
    /// Apricot Phase 5.
    ApricotPhase5,
    /// Apricot Phase Pre-6.
    ApricotPhasePre6,
    /// Apricot Phase 6.
    ApricotPhase6,
    /// Apricot Phase Post-6.
    ApricotPhasePost6,
    /// Banff.
    Banff,
    /// Cortina.
    Cortina,
    /// Durango — enables Shanghai (`config_extra.go:83`).
    Durango,
    /// Etna — enables Cancun (`config_extra.go:87`).
    Etna,
    /// Fortuna (ACP-176 fees; no new Ethereum upgrade).
    Fortuna,
    /// Granite (no new Ethereum upgrade).
    Granite,
}

impl AvaPhase {
    /// Every phase in chronological order (used by table tests / iteration).
    pub const ALL: [AvaPhase; 15] = [
        AvaPhase::Launch,
        AvaPhase::ApricotPhase1,
        AvaPhase::ApricotPhase2,
        AvaPhase::ApricotPhase3,
        AvaPhase::ApricotPhase4,
        AvaPhase::ApricotPhase5,
        AvaPhase::ApricotPhasePre6,
        AvaPhase::ApricotPhase6,
        AvaPhase::ApricotPhasePost6,
        AvaPhase::Banff,
        AvaPhase::Cortina,
        AvaPhase::Durango,
        AvaPhase::Etna,
        AvaPhase::Fortuna,
        AvaPhase::Granite,
    ];

    /// Maps an `ava_version` [`Fork`] to its C-Chain [`AvaPhase`]. Helicon (which
    /// coreth does not map) returns `None` so it never participates in the
    /// C-Chain spec.
    #[must_use]
    pub fn from_version_fork(f: Fork) -> Option<AvaPhase> {
        Some(match f {
            Fork::ApricotPhase1 => AvaPhase::ApricotPhase1,
            Fork::ApricotPhase2 => AvaPhase::ApricotPhase2,
            Fork::ApricotPhase3 => AvaPhase::ApricotPhase3,
            Fork::ApricotPhase4 => AvaPhase::ApricotPhase4,
            Fork::ApricotPhase5 => AvaPhase::ApricotPhase5,
            Fork::ApricotPhasePre6 => AvaPhase::ApricotPhasePre6,
            Fork::ApricotPhase6 => AvaPhase::ApricotPhase6,
            Fork::ApricotPhasePost6 => AvaPhase::ApricotPhasePost6,
            Fork::Banff => AvaPhase::Banff,
            Fork::Cortina => AvaPhase::Cortina,
            Fork::Durango => AvaPhase::Durango,
            Fork::Etna => AvaPhase::Etna,
            Fork::Fortuna => AvaPhase::Fortuna,
            Fork::Granite => AvaPhase::Granite,
            Fork::Helicon => return None,
        })
    }

    /// Stable `&'static str` name (used for the [`Hardfork`] trait `name()` and
    /// for [`ChainHardforks`] map keys — MUST be unique per phase).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            AvaPhase::Launch => "Launch",
            AvaPhase::ApricotPhase1 => "ApricotPhase1",
            AvaPhase::ApricotPhase2 => "ApricotPhase2",
            AvaPhase::ApricotPhase3 => "ApricotPhase3",
            AvaPhase::ApricotPhase4 => "ApricotPhase4",
            AvaPhase::ApricotPhase5 => "ApricotPhase5",
            AvaPhase::ApricotPhasePre6 => "ApricotPhasePre6",
            AvaPhase::ApricotPhase6 => "ApricotPhase6",
            AvaPhase::ApricotPhasePost6 => "ApricotPhasePost6",
            AvaPhase::Banff => "Banff",
            AvaPhase::Cortina => "Cortina",
            AvaPhase::Durango => "Durango",
            AvaPhase::Etna => "Etna",
            AvaPhase::Fortuna => "Fortuna",
            AvaPhase::Granite => "Granite",
        }
    }
}

/// A hardfork the C-Chain understands: either an inherited Ethereum fork (coreth
/// maps a subset in) or an Avalanche phase. This is the unit the reth
/// [`ChainHardforks`] list stores and the implementor of the reth [`Hardfork`]
/// trait (spec 10 §17.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AvaHardfork {
    /// An inherited Ethereum hardfork (London, Shanghai, Cancun, …).
    Eth(EthereumHardfork),
    /// An Avalanche network-upgrade phase.
    Phase(AvaPhase),
}

impl Hardfork for AvaHardfork {
    fn name(&self) -> &'static str {
        match self {
            // Reuse the Ethereum fork's own stable name so reth's
            // `EthereumHardforks` lookups resolve through the same map key.
            AvaHardfork::Eth(eth) => eth.name(),
            AvaHardfork::Phase(p) => p.name(),
        }
    }
}

/// Minimal fee configuration carried by [`AvaChainSpec`].
///
/// **Stub (M6.5).** The full coreth `FeeConfig` (GasLimit, TargetBlockRate,
/// MinBaseFee, TargetGas, BaseFeeChangeDenominator, Min/MaxBlockGasCost,
/// BlockGasCostStep) and the per-fork [`BaseFeeParams`] selection land with the
/// feerules tasks (M6.11–M6.13, spec 10 §7.4/§17.3/§21). For now we carry the
/// `EthChainSpec`-shaped base-fee params so reth's own base-fee paths don't
/// disagree where they happen to run; Avalanche overrides base fee in feerules.
#[derive(Clone, Copy, Debug)]
pub struct FeeConfig {
    /// Ethereum EIP-1559 base-fee params used as the reth-side default until
    /// feerules (M6.11+) supplies per-fork Avalanche params.
    pub base_fee_params: BaseFeeParams,
}

impl Default for FeeConfig {
    fn default() -> Self {
        Self {
            base_fee_params: BaseFeeParams::ethereum(),
        }
    }
}

/// The Avalanche network-upgrade activation schedule, in **u64 unix seconds**.
///
/// This is the C-Chain-local, EVM-facing projection of
/// `ava_version::upgrade::UpgradeConfig` (the protocol-constant source of truth,
/// spec 00 §5). Each field is `t` such that the phase is active iff
/// `block_timestamp >= t` (inclusive boundary, matching Go `!t.Before(forkTime)`).
/// Unscheduled phases use [`u64::MAX`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkUpgrades {
    /// Apricot Phase 1 activation (unix seconds).
    pub apricot_phase_1: u64,
    /// Apricot Phase 2 activation (unix seconds).
    pub apricot_phase_2: u64,
    /// Apricot Phase 3 activation (unix seconds).
    pub apricot_phase_3: u64,
    /// Apricot Phase 4 activation (unix seconds).
    pub apricot_phase_4: u64,
    /// Apricot Phase 5 activation (unix seconds).
    pub apricot_phase_5: u64,
    /// Apricot Phase Pre-6 activation (unix seconds).
    pub apricot_phase_pre_6: u64,
    /// Apricot Phase 6 activation (unix seconds).
    pub apricot_phase_6: u64,
    /// Apricot Phase Post-6 activation (unix seconds).
    pub apricot_phase_post_6: u64,
    /// Banff activation (unix seconds).
    pub banff: u64,
    /// Cortina activation (unix seconds).
    pub cortina: u64,
    /// Durango activation (unix seconds).
    pub durango: u64,
    /// Etna activation (unix seconds).
    pub etna: u64,
    /// Fortuna activation (unix seconds).
    pub fortuna: u64,
    /// Granite activation (unix seconds).
    pub granite: u64,
    /// Helicon activation (unix seconds).
    ///
    /// Unlike the other phases, Helicon is **not** an [`AvaPhase`] — coreth maps
    /// it to no Ethereum upgrade, so it does not participate in `fork_at` /
    /// `revm_spec_id`. It is carried separately here purely to gate the ACP-194
    /// minimum-gas-consumption floor in the gas-charge path (M7.35, spec 11;
    /// coreth `params/hooks_libevm.go` `RulesExtra.MinimumGasConsumption` keys
    /// on `IsHelicon`). Currently unscheduled on all networks ([`u64::MAX`]).
    pub helicon: u64,
}

/// Converts a `chrono` [`DateTime<Utc>`] to a `u64` unix second, mapping any
/// pre-epoch time (none occur in the real schedule) to 0. No raw casts (clippy
/// `cast_*`); no panics.
fn to_unix_secs(dt: DateTime<Utc>) -> u64 {
    u64::try_from(dt.timestamp()).unwrap_or(0)
}

impl NetworkUpgrades {
    /// Builds the schedule for a network id (Mainnet=1, Fuji=5, else local),
    /// sourcing the timestamps from `ava_version` (spec 00 §5).
    #[must_use]
    pub fn for_network(network_id: u32) -> Self {
        Self::from_upgrade_config(&get_config(network_id))
    }

    /// Projects an `ava_version` [`UpgradeConfig`] onto u64 unix seconds.
    #[must_use]
    pub fn from_upgrade_config(cfg: &UpgradeConfig) -> Self {
        Self {
            apricot_phase_1: to_unix_secs(cfg.apricot_phase_1_time),
            apricot_phase_2: to_unix_secs(cfg.apricot_phase_2_time),
            apricot_phase_3: to_unix_secs(cfg.apricot_phase_3_time),
            apricot_phase_4: to_unix_secs(cfg.apricot_phase_4_time),
            apricot_phase_5: to_unix_secs(cfg.apricot_phase_5_time),
            apricot_phase_pre_6: to_unix_secs(cfg.apricot_phase_pre_6_time),
            apricot_phase_6: to_unix_secs(cfg.apricot_phase_6_time),
            apricot_phase_post_6: to_unix_secs(cfg.apricot_phase_post_6_time),
            banff: to_unix_secs(cfg.banff_time),
            cortina: to_unix_secs(cfg.cortina_time),
            durango: to_unix_secs(cfg.durango_time),
            etna: to_unix_secs(cfg.etna_time),
            fortuna: to_unix_secs(cfg.fortuna_time),
            granite: to_unix_secs(cfg.granite_time),
            helicon: to_unix_secs(cfg.helicon_time),
        }
    }

    /// Whether the ACP-194 minimum-gas-consumption floor is active at
    /// `timestamp` (Helicon activated, `timestamp >= helicon`). Mirrors coreth
    /// `RulesExtra.IsHelicon` (`params/hooks_libevm.go`); Helicon is currently
    /// unscheduled on every network, so this is `false` on all live blocks.
    #[must_use]
    pub fn is_helicon(&self, timestamp: u64) -> bool {
        timestamp >= self.helicon
    }

    /// Activation timestamp for a phase. `Launch` is active from the dawn of time
    /// (0) — it has no gating upgrade.
    #[must_use]
    pub fn activation(&self, phase: AvaPhase) -> u64 {
        match phase {
            AvaPhase::Launch => 0,
            AvaPhase::ApricotPhase1 => self.apricot_phase_1,
            AvaPhase::ApricotPhase2 => self.apricot_phase_2,
            AvaPhase::ApricotPhase3 => self.apricot_phase_3,
            AvaPhase::ApricotPhase4 => self.apricot_phase_4,
            AvaPhase::ApricotPhase5 => self.apricot_phase_5,
            AvaPhase::ApricotPhasePre6 => self.apricot_phase_pre_6,
            AvaPhase::ApricotPhase6 => self.apricot_phase_6,
            AvaPhase::ApricotPhasePost6 => self.apricot_phase_post_6,
            AvaPhase::Banff => self.banff,
            AvaPhase::Cortina => self.cortina,
            AvaPhase::Durango => self.durango,
            AvaPhase::Etna => self.etna,
            AvaPhase::Fortuna => self.fortuna,
            AvaPhase::Granite => self.granite,
        }
    }
}

/// The C-Chain chain spec (spec 10 §7.4/§17.8, G7).
///
/// Wraps the Avalanche fork schedule on top of reth's Ethereum chain spec:
/// `inner` is the ordered [`ChainHardforks`] (Ethereum forks + Avalanche phases,
/// all `ForkCondition::Timestamp`); `eth_spec` is a reth [`ChainSpec`] used to
/// satisfy the heavy [`EthChainSpec`] methods by delegation (genesis hash/header,
/// deposit contract, display, bootnodes, blob params); `network_upgrades` is the
/// u64 projection of the protocol-constant schedule used by `fork_at` /
/// `revm_spec_id` / `check_compatible`.
#[derive(Clone, Debug)]
pub struct AvaChainSpec {
    /// reth ordered fork list (`(Hardfork, ForkCondition::Timestamp)`).
    inner: ChainHardforks,
    /// The genesis header (kept for `EthChainSpec::genesis_header` parity, §11.1).
    eth_genesis_header: Header,
    /// The reth chain spec used for `EthChainSpec` delegation.
    eth_spec: ChainSpec,
    /// Per-fork fee configuration (stub — full version in M6.11+).
    fee_config: FeeConfig,
    /// The Avalanche activation schedule (u64 unix seconds).
    network_upgrades: NetworkUpgrades,
    /// `true` for an EVM-subnet profile, `false` for the C-Chain.
    is_subnet: bool,
    /// The `Chain` id (43114 mainnet C-Chain, 43113 Fuji, …).
    chain: Chain,
}

impl AvaChainSpec {
    /// Builds the C-Chain spec for the given network id and chain id.
    ///
    /// The Ethereum fork → Avalanche phase mapping is coreth
    /// `params/config_extra.go:SetEthUpgrades`: Berlin←AP2, London←AP3,
    /// Shanghai←Durango, Cancun←Etna; pre-AP2 Ethereum forks (through Istanbul)
    /// are active from genesis (block 0 → timestamp 0).
    #[must_use]
    pub fn c_chain(network_id: u32, chain: Chain) -> Self {
        let network_upgrades = NetworkUpgrades::for_network(network_id);
        Self::from_parts(network_upgrades, chain, false)
    }

    /// Builds a spec from an explicit schedule (used by tests / subnet profiles).
    #[must_use]
    pub fn from_parts(network_upgrades: NetworkUpgrades, chain: Chain, is_subnet: bool) -> Self {
        let inner = build_chain_hardforks(&network_upgrades);
        let mut genesis = Genesis::default();
        genesis.config.chain_id = chain.id();
        let eth_spec = ChainSpec::from(genesis);
        let eth_genesis_header = EthChainSpec::genesis_header(&eth_spec).clone();
        Self {
            inner,
            eth_genesis_header,
            eth_spec,
            fee_config: FeeConfig::default(),
            network_upgrades,
            is_subnet,
            chain,
        }
    }

    /// The active [`NetworkUpgrades`] schedule.
    #[must_use]
    pub fn network_upgrades(&self) -> &NetworkUpgrades {
        &self.network_upgrades
    }

    /// The fee configuration (stub until M6.11+).
    #[must_use]
    pub fn fee_config(&self) -> &FeeConfig {
        &self.fee_config
    }

    /// `true` for an EVM-subnet profile.
    #[must_use]
    pub fn is_subnet(&self) -> bool {
        self.is_subnet
    }

    /// The ordered reth fork list.
    #[must_use]
    pub fn hardforks(&self) -> &ChainHardforks {
        &self.inner
    }

    /// The highest Avalanche [`AvaPhase`] active at `timestamp` (the "current
    /// fork"). Returns [`AvaPhase::Launch`] before any phase activates.
    #[must_use]
    pub fn fork_at(&self, timestamp: u64) -> AvaPhase {
        let mut current = AvaPhase::Launch;
        for phase in AvaPhase::ALL {
            if timestamp >= self.network_upgrades.activation(phase) {
                current = phase;
            }
        }
        current
    }

    /// Whether the ACP-194 minimum-gas-consumption floor is active at
    /// `timestamp` (Helicon activated). Mirrors coreth `RulesExtra.IsHelicon`
    /// (`params/hooks_libevm.go`); see [`NetworkUpgrades::is_helicon`].
    #[must_use]
    pub fn is_helicon(&self, timestamp: u64) -> bool {
        self.network_upgrades.is_helicon(timestamp)
    }

    /// The revm Ethereum [`SpecId`] coreth pins for the active Avalanche phase at
    /// `timestamp` (coreth `params/config_extra.go:SetEthUpgrades`).
    ///
    /// Berlin←AP2, London←AP3, Shanghai←Durango, Cancun←Etna; coreth pins no
    /// Ethereum upgrade beyond Cancun, so Fortuna/Granite remain `CANCUN`.
    #[must_use]
    pub fn revm_spec_id(&self, timestamp: u64) -> SpecId {
        match self.fork_at(timestamp) {
            // Etna (Cancun) and everything after it (Fortuna, Granite).
            p if p >= AvaPhase::Etna => SpecId::CANCUN,
            // Durango (Shanghai) up to but not including Etna.
            p if p >= AvaPhase::Durango => SpecId::SHANGHAI,
            // Apricot Phase 3 (London) up to but not including Durango.
            p if p >= AvaPhase::ApricotPhase3 => SpecId::LONDON,
            // Apricot Phase 2 (Berlin).
            p if p >= AvaPhase::ApricotPhase2 => SpecId::BERLIN,
            // Launch / Apricot Phase 1: pre-Berlin (Istanbul-era at block 0).
            _ => SpecId::ISTANBUL,
        }
    }

    /// Validates that `other` does not change the activation timestamp of any
    /// fork that has **already activated** at `head_ts` (coreth
    /// `params/network_upgrades.go:checkCompatible` parity, spec 10 §7.4).
    ///
    /// Returns [`AvaEvmError::IncompatibleFork`] on the first phase whose
    /// activation differs while being active under `self` at `head_ts`.
    ///
    /// # Errors
    ///
    /// Returns [`AvaEvmError::IncompatibleFork`] if an already-activated fork is
    /// rescheduled to a different timestamp.
    pub fn check_compatible(
        &self,
        other: &NetworkUpgrades,
        head_ts: u64,
    ) -> Result<(), AvaEvmError> {
        for phase in AvaPhase::ALL {
            if phase == AvaPhase::Launch {
                continue;
            }
            let self_act = self.network_upgrades.activation(phase);
            let other_act = other.activation(phase);
            // An already-activated fork (active under the current schedule at the
            // head) may not be rescheduled to a different time.
            if head_ts >= self_act && self_act != other_act {
                return Err(AvaEvmError::IncompatibleFork { fork: phase.name() });
            }
        }
        Ok(())
    }
}

// ── Per-phase predicates (mirror coreth `extras.IsApricotPhaseN` etc.) ────────

impl AvaChainSpec {
    /// `extras.IsApricotPhase1`
    #[must_use]
    pub fn is_apricot_phase1(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::ApricotPhase1
    }
    /// `extras.IsApricotPhase2`
    #[must_use]
    pub fn is_apricot_phase2(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::ApricotPhase2
    }
    /// `extras.IsApricotPhase3`
    #[must_use]
    pub fn is_apricot_phase3(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::ApricotPhase3
    }
    /// `extras.IsApricotPhase4`
    #[must_use]
    pub fn is_apricot_phase4(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::ApricotPhase4
    }
    /// `extras.IsApricotPhase5`
    #[must_use]
    pub fn is_apricot_phase5(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::ApricotPhase5
    }
    /// `extras.IsApricotPhasePre6`
    #[must_use]
    pub fn is_apricot_phase_pre6(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::ApricotPhasePre6
    }
    /// `extras.IsApricotPhase6`
    #[must_use]
    pub fn is_apricot_phase6(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::ApricotPhase6
    }
    /// `extras.IsApricotPhasePost6`
    #[must_use]
    pub fn is_apricot_phase_post6(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::ApricotPhasePost6
    }
    /// `extras.IsBanff`
    #[must_use]
    pub fn is_banff(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::Banff
    }
    /// `extras.IsCortina`
    #[must_use]
    pub fn is_cortina(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::Cortina
    }
    /// `extras.IsDurango`
    #[must_use]
    pub fn is_durango(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::Durango
    }
    /// `extras.IsEtna`
    #[must_use]
    pub fn is_etna(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::Etna
    }
    /// `extras.IsFortuna`
    #[must_use]
    pub fn is_fortuna(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::Fortuna
    }
    /// `extras.IsGranite`
    #[must_use]
    pub fn is_granite(&self, t: u64) -> bool {
        self.fork_at(t) >= AvaPhase::Granite
    }
}

/// Builds the ordered reth [`ChainHardforks`] from the Avalanche schedule.
///
/// Inserts the pre-AP2 Ethereum forks **plus Paris/MergeNetsplit** at genesis
/// (`ForkCondition::Block(0)`), then each Ethereum fork coreth maps to a phase
/// at that phase's activation time, then every Avalanche phase.
/// `ChainHardforks::insert` keeps the list ordered by `ForkCondition`.
///
/// **Paris-at-genesis (M6.8, resolves M6.6 finding #2).** Avalanche is never
/// PoW; the merge "happened" before genesis. reth's block-reward path keys on
/// [`EthereumHardforks::is_paris_active_at_block`] (a *block* predicate), and
/// `final_paris_total_difficulty == 0` ([`AvaChainSpec::final_paris_total_difficulty`]).
/// By activating Paris + every pre-merge Ethereum fork at `Block(0)` here, the
/// executor sees the chain as post-merge from block 0 and applies **no** PoW
/// block reward — matching coreth — without the temporary `AvaExecutorSpec`
/// override M6.6 carried (now removed). `revm_spec_id` is unaffected: it is
/// driven by the Avalanche `network_upgrades` timestamps, not this list.
fn build_chain_hardforks(up: &NetworkUpgrades) -> ChainHardforks {
    let mut forks = ChainHardforks::new(Vec::new());

    // Pre-Apricot-Phase-2 Ethereum forks are all active from launch, plus the
    // merge-related forks (Dao..Paris): Avalanche is post-merge from block 0
    // (coreth `SetEthUpgrades` sets Homestead..MuirGlacier to block 0, and the
    // network is never PoW so Paris is active at genesis). Keyed by `Block(0)`
    // so reth's `is_paris_active_at_block` / block-reward path resolve correctly.
    for eth in [
        EthereumHardfork::Frontier,
        EthereumHardfork::Homestead,
        EthereumHardfork::Dao,
        EthereumHardfork::Tangerine,
        EthereumHardfork::SpuriousDragon,
        EthereumHardfork::Byzantium,
        EthereumHardfork::Constantinople,
        EthereumHardfork::Petersburg,
        EthereumHardfork::Istanbul,
        EthereumHardfork::MuirGlacier,
        EthereumHardfork::ArrowGlacier,
        EthereumHardfork::GrayGlacier,
        EthereumHardfork::Paris,
    ] {
        forks.insert(AvaHardfork::Eth(eth), ForkCondition::Block(0));
    }

    // Ethereum forks coreth maps to specific Avalanche phases.
    forks.insert(
        AvaHardfork::Eth(EthereumHardfork::Berlin),
        ForkCondition::Timestamp(up.apricot_phase_2),
    );
    forks.insert(
        AvaHardfork::Eth(EthereumHardfork::London),
        ForkCondition::Timestamp(up.apricot_phase_3),
    );
    forks.insert(
        AvaHardfork::Eth(EthereumHardfork::Shanghai),
        ForkCondition::Timestamp(up.durango),
    );
    forks.insert(
        AvaHardfork::Eth(EthereumHardfork::Cancun),
        ForkCondition::Timestamp(up.etna),
    );

    // Every Avalanche phase (timestamp-activated).
    for phase in AvaPhase::ALL {
        if phase == AvaPhase::Launch {
            continue;
        }
        forks.insert(
            AvaHardfork::Phase(phase),
            ForkCondition::Timestamp(up.activation(phase)),
        );
    }

    forks
}

// ── reth trait impls (delegate the heavy machinery to the inner ChainSpec) ────

impl EthChainSpec for AvaChainSpec {
    type Header = Header;

    fn chain(&self) -> Chain {
        self.chain
    }

    fn base_fee_params_at_timestamp(&self, _timestamp: u64) -> BaseFeeParams {
        // Avalanche overrides base fee in feerules (§17.3); return the configured
        // params so reth's own base-fee paths don't disagree where they run.
        self.fee_config.base_fee_params
    }

    fn blob_params_at_timestamp(&self, timestamp: u64) -> Option<BlobParams> {
        EthChainSpec::blob_params_at_timestamp(&self.eth_spec, timestamp)
    }

    fn deposit_contract(&self) -> Option<&DepositContract> {
        // Avalanche has no beacon-deposit contract.
        None
    }

    fn genesis_hash(&self) -> B256 {
        EthChainSpec::genesis_hash(&self.eth_spec)
    }

    fn prune_delete_limit(&self) -> usize {
        EthChainSpec::prune_delete_limit(&self.eth_spec)
    }

    fn display_hardforks(&self) -> Box<dyn core::fmt::Display> {
        EthChainSpec::display_hardforks(&self.eth_spec)
    }

    fn genesis_header(&self) -> &Self::Header {
        &self.eth_genesis_header
    }

    fn genesis(&self) -> &Genesis {
        EthChainSpec::genesis(&self.eth_spec)
    }

    fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        // Avalanche peers are discovered via the Avalanche p2p layer (spec 05).
        None
    }

    fn final_paris_total_difficulty(&self) -> Option<U256> {
        // Avalanche is not PoW; total terminal difficulty is zero.
        Some(U256::ZERO)
    }
}

impl EthereumHardforks for AvaChainSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        // coreth maps each inherited Ethereum fork to a phase timestamp; the
        // `ChainHardforks` list keys on the Ethereum fork's own name, so this
        // resolves through the same map entry `build_chain_hardforks` inserted.
        self.inner.fork(AvaHardfork::Eth(fork))
    }
}

// ── C-Chain genesis parse + materialization (spec 10 §11.1 / §8.3, M6.8) ──────

/// Local result alias for the genesis parser (the crate `Result`, not the
/// `std::result::Result<(), AvaEvmError>` `check_compatible` uses above).
type GenesisResult<T> = crate::error::Result<T>;

/// One pre-funded / pre-deployed account in the C-Chain genesis `alloc`
/// (coreth `core.GenesisAccount`). All fields are `0x`-hex in the JSON, parsed
/// here into typed values. `code`/`storage` are absent for plain EOAs.
#[derive(Clone, Debug, Default, serde::Deserialize)]
struct RawGenesisAccount {
    /// Account balance in wei (`0x`-hex big-endian scalar).
    #[serde(default)]
    balance: Option<String>,
    /// Account nonce (`0x`-hex scalar).
    #[serde(default)]
    nonce: Option<String>,
    /// Contract bytecode (`0x`-hex), absent for EOAs.
    #[serde(default)]
    code: Option<String>,
    /// Storage slots (`slot -> value`, both `0x`-hex 32-byte).
    #[serde(default)]
    storage: Option<std::collections::BTreeMap<String, String>>,
}

/// The genesis `config` block (coreth `params.ChainConfig` + `extras`,
/// spec 10 §11.1). Only the fields the EVM port consumes are decoded; the rest
/// (the many block-0 Ethereum fork flags, `eip*Hash`, `daoForkSupport`) are
/// accepted and ignored. Avalanche network-upgrade timestamps live in the node
/// config (`ava_version`), not the embedded Mainnet/Fuji genesis JSON, so the
/// optional `*BlockTimestamp` fields here override the schedule only when a
/// custom (local/subnet) genesis sets them.
#[derive(Clone, Debug, Default, serde::Deserialize)]
struct RawGenesisConfig {
    /// EVM chain id (43114 Mainnet, 43113 Fuji).
    #[serde(rename = "chainId")]
    chain_id: u64,
    /// Timestamp-keyed precompile enable/disable + param changes (§8.3). Absent
    /// in the Mainnet/Fuji genesis; carried for subnet/local parity.
    #[serde(rename = "precompileUpgrades", default)]
    precompile_upgrades: Vec<RawPrecompileUpgrade>,
}

/// One timestamp-keyed precompile upgrade entry (coreth/subnet-evm
/// `PrecompileUpgrade`, spec 10 §8.3). The concrete precompile config is an
/// opaque JSON object here (the precompile registry M6.22 owns interpretation);
/// M6.8 captures only the `blockTimestamp` activation key so the upgrade
/// schedule round-trips byte-compatibly. Each entry carries exactly one
/// precompile config keyed by the precompile's name (e.g. `feeManagerConfig`),
/// which always includes a `blockTimestamp`.
#[derive(Clone, Debug, serde::Deserialize)]
struct RawPrecompileUpgrade(serde_json::Map<String, serde_json::Value>);

/// A single parsed precompile-upgrade activation (§8.3): the precompile-config
/// key (e.g. `"feeManagerConfig"`) and its `blockTimestamp` activation second.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrecompileUpgrade {
    /// The precompile-config key in the genesis/upgrade JSON.
    pub key: String,
    /// The `blockTimestamp` (unix seconds) at which the upgrade activates.
    pub block_timestamp: u64,
}

/// The full C-Chain genesis JSON (coreth `core.Genesis`, spec 10 §11.1). The
/// `cChainGenesis` string embedded in `genesis/genesis_{mainnet,fuji}.json` is
/// exactly this shape.
#[derive(Clone, Debug, serde::Deserialize)]
struct RawGenesis {
    config: RawGenesisConfig,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "extraData", default)]
    extra_data: Option<String>,
    #[serde(rename = "gasLimit")]
    gas_limit: String,
    #[serde(default)]
    difficulty: Option<String>,
    #[serde(rename = "mixHash", default)]
    mix_hash: Option<String>,
    #[serde(default)]
    coinbase: Option<String>,
    alloc: std::collections::BTreeMap<String, RawGenesisAccount>,
    #[serde(default)]
    number: Option<String>,
    #[serde(rename = "gasUsed", default)]
    gas_used: Option<String>,
    #[serde(rename = "parentHash", default)]
    parent_hash: Option<String>,
    /// `baseFeePerGas` (EIP-1559) — Go `Genesis.BaseFee`. Absent in every
    /// embedded network genesis; when AP3 is active at the genesis timestamp,
    /// an absent value defaults to `ap3.InitialBaseFee` (coreth `toBlock`).
    #[serde(rename = "baseFeePerGas", default)]
    base_fee_per_gas: Option<String>,
    /// `excessBlobGas` (EIP-4844) — Go `Genesis.ExcessBlobGas`.
    #[serde(rename = "excessBlobGas", default)]
    excess_blob_gas: Option<String>,
    /// `blobGasUsed` (EIP-4844) — Go `Genesis.BlobGasUsed`.
    #[serde(rename = "blobGasUsed", default)]
    blob_gas_used: Option<String>,
}

/// A parsed C-Chain genesis (spec 10 §11.1): the chain id, the header scalar
/// fields, the `precompileUpgrades` schedule (§8.3), and the `alloc` (already
/// materialized into Firewood ethhash [`BatchOp`]s + a `code_hash -> code`
/// side-store list). [`CChainGenesis::state_root`] computes the genesis state
/// root; [`CChainGenesis::genesis_header`] builds the coreth genesis header for
/// the block ID.
#[derive(Clone, Debug)]
pub struct CChainGenesis {
    /// EVM chain id.
    chain_id: u64,
    /// Genesis header timestamp (unix seconds).
    timestamp: u64,
    /// Genesis block number (always 0).
    number: u64,
    /// Genesis gas limit.
    gas_limit: u64,
    /// Genesis gas used (always 0).
    gas_used: u64,
    /// Genesis difficulty (0 — Avalanche is post-merge).
    difficulty: U256,
    /// Genesis block nonce (8 bytes, 0).
    nonce: [u8; 8],
    /// Genesis `extraData`.
    extra: Bytes,
    /// Genesis `mixHash`.
    mix_digest: B256,
    /// Genesis coinbase.
    coinbase: Address,
    /// Genesis parent hash (zero).
    parent_hash: B256,
    /// Genesis `baseFeePerGas` (`None` in every embedded network genesis; the
    /// AP3-active default is applied in [`CChainGenesis::genesis_header`]).
    base_fee: Option<U256>,
    /// Genesis `excessBlobGas` (`None` ⇒ `0` when Cancun is active).
    excess_blob_gas: Option<u64>,
    /// Genesis `blobGasUsed` (`None` ⇒ `0` when Cancun is active).
    blob_gas_used: Option<u64>,
    /// The genesis `alloc` as a revm [`BundleState`]: feed it through
    /// [`FirewoodStateProvider::propose_from_bundle`](crate::state::FirewoodStateProvider::propose_from_bundle)
    /// to obtain the genesis state root. The bundle path materializes accounts
    /// via the 5-field `rlp_account` (M6.30) so the root matches coreth.
    alloc_bundle: BundleState,
    /// `code_hash -> bytecode` for every contract account in the `alloc`; the
    /// caller seeds the bytecode side store before reading account code.
    bytecode: Vec<(B256, Vec<u8>)>,
    /// The `precompileUpgrades` schedule (§8.3), timestamp-keyed.
    precompile_upgrades: Vec<PrecompileUpgrade>,
}

/// Parses a `0x`-prefixed (or bare) hex scalar into a `u64` (RLP-style minimal
/// big-endian). An empty / `0x` string is `0`.
fn parse_u64_hex(s: &str) -> GenesisResult<u64> {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(trimmed, 16).map_err(|e| Error::GenesisParse(format!("u64 hex {s:?}: {e}")))
}

/// Parses a `0x`-prefixed (or bare) hex scalar into a [`U256`]. Empty is `0`.
fn parse_u256_hex(s: &str) -> GenesisResult<U256> {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.is_empty() {
        return Ok(U256::ZERO);
    }
    U256::from_str_radix(trimmed, 16)
        .map_err(|_| Error::GenesisParse(format!("u256 hex {s:?} invalid")))
}

/// Decodes a `0x`-prefixed (or bare) hex byte string.
fn parse_bytes_hex(s: &str) -> GenesisResult<Vec<u8>> {
    hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| Error::GenesisParse(format!("hex {s:?}: {e}")))
}

/// Parses a 20-byte address from hex (with or without `0x`).
fn parse_address(s: &str) -> GenesisResult<Address> {
    let bytes = parse_bytes_hex(s)?;
    if bytes.len() != 20 {
        return Err(Error::GenesisParse(format!(
            "address {s:?}: expected 20 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(Address::from_slice(&bytes))
}

/// Parses a 32-byte hash from hex (with or without `0x`).
fn parse_b256(s: &str) -> GenesisResult<B256> {
    let bytes = parse_bytes_hex(s)?;
    if bytes.len() != 32 {
        return Err(Error::GenesisParse(format!(
            "hash {s:?}: expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(B256::from_slice(&bytes))
}

impl CChainGenesis {
    /// Parses C-Chain genesis JSON (the `cChainGenesis` string, coreth
    /// `core.Genesis`) into a [`CChainGenesis`], materializing the `alloc` into
    /// Firewood ethhash [`BatchOp`]s via the 5-field [`rlp_account`] path
    /// (M6.30) so the state root matches coreth byte-for-byte.
    ///
    /// # Errors
    /// Returns [`Error::GenesisParse`] on invalid JSON or a malformed hex field.
    pub fn parse(json: &str) -> GenesisResult<Self> {
        let raw: RawGenesis =
            serde_json::from_str(json).map_err(|e| Error::GenesisParse(e.to_string()))?;

        // Header scalar fields (coreth `Genesis.toBlock`). Optional JSON fields
        // default the way coreth's zero-value `Genesis` does.
        let timestamp = raw
            .timestamp
            .as_deref()
            .map(parse_u64_hex)
            .transpose()?
            .unwrap_or(0);
        let number = raw
            .number
            .as_deref()
            .map(parse_u64_hex)
            .transpose()?
            .unwrap_or(0);
        let gas_limit = parse_u64_hex(&raw.gas_limit)?;
        let gas_used = raw
            .gas_used
            .as_deref()
            .map(parse_u64_hex)
            .transpose()?
            .unwrap_or(0);
        let difficulty = raw
            .difficulty
            .as_deref()
            .map(parse_u256_hex)
            .transpose()?
            .unwrap_or(U256::ZERO);
        let nonce_u64 = raw
            .nonce
            .as_deref()
            .map(parse_u64_hex)
            .transpose()?
            .unwrap_or(0);
        let nonce = nonce_u64.to_be_bytes();
        let extra = Bytes::from(
            raw.extra_data
                .as_deref()
                .map(parse_bytes_hex)
                .transpose()?
                .unwrap_or_default(),
        );
        let mix_digest = raw
            .mix_hash
            .as_deref()
            .map(parse_b256)
            .transpose()?
            .unwrap_or(B256::ZERO);
        let coinbase = raw
            .coinbase
            .as_deref()
            .map(parse_address)
            .transpose()?
            .unwrap_or(Address::ZERO);
        let parent_hash = raw
            .parent_hash
            .as_deref()
            .map(parse_b256)
            .transpose()?
            .unwrap_or(B256::ZERO);
        let base_fee = raw
            .base_fee_per_gas
            .as_deref()
            .map(parse_u256_hex)
            .transpose()?;
        let excess_blob_gas = raw
            .excess_blob_gas
            .as_deref()
            .map(parse_u64_hex)
            .transpose()?;
        let blob_gas_used = raw
            .blob_gas_used
            .as_deref()
            .map(parse_u64_hex)
            .transpose()?;

        // Materialize the alloc into a revm `BundleState`: each account becomes a
        // present `AccountInfo` (balance, nonce, code_hash) + its storage slots.
        // The provider's `propose_from_bundle` runs this through the 5-field
        // `rlp_account` path (M6.30) so the state root matches coreth. The genesis
        // is block 0, so the revert range is `0..=0`.
        let mut builder: BundleBuilder = BundleState::builder(0..=0);
        let mut bytecode: Vec<(B256, Vec<u8>)> = Vec::new();

        // Parse + sort accounts by address for deterministic construction.
        let mut accounts: Vec<(Address, RawGenesisAccount)> = Vec::with_capacity(raw.alloc.len());
        for (addr_hex, acct) in &raw.alloc {
            accounts.push((parse_address(addr_hex)?, acct.clone()));
        }
        accounts.sort_by_key(|(addr, _)| *addr);

        for (addr, acct) in &accounts {
            let balance = acct
                .balance
                .as_deref()
                .map(parse_u256_hex)
                .transpose()?
                .unwrap_or(U256::ZERO);
            let acct_nonce = acct
                .nonce
                .as_deref()
                .map(parse_u64_hex)
                .transpose()?
                .unwrap_or(0);
            let (code_hash, code) = match &acct.code {
                Some(code_hex) => {
                    let raw_code = parse_bytes_hex(code_hex)?;
                    if raw_code.is_empty() {
                        (KECCAK_EMPTY, None)
                    } else {
                        let h = keccak256(&raw_code);
                        let bytecode_obj = Bytecode::new_raw(Bytes::from(raw_code.clone()));
                        bytecode.push((h, raw_code));
                        (h, Some(bytecode_obj))
                    }
                }
                None => (KECCAK_EMPTY, None),
            };
            builder = builder.state_present_account_info(
                *addr,
                AccountInfo {
                    balance,
                    nonce: acct_nonce,
                    code_hash,
                    code,
                    ..Default::default()
                },
            );

            // Storage slots (present). A zero genesis slot is an absent slot.
            if let Some(storage) = &acct.storage {
                let mut slots: StorageKeyMap<(U256, U256)> = StorageKeyMap::default();
                for (slot_hex, val_hex) in storage {
                    let slot = parse_b256(slot_hex)?;
                    let value = parse_u256_hex(val_hex)?;
                    if value.is_zero() {
                        continue;
                    }
                    // `StorageKeyMap` is keyed by the U256 slot; the value is the
                    // (original, present) pair — original is zero at genesis.
                    slots.insert(slot.into(), (U256::ZERO, value));
                }
                if !slots.is_empty() {
                    builder = builder.state_storage(*addr, slots);
                }
            }
        }
        let alloc_bundle = builder.build();

        // Precompile upgrades (§8.3): one config object per entry, keyed by the
        // precompile-config name; each carries a `blockTimestamp`.
        let mut precompile_upgrades = Vec::with_capacity(raw.config.precompile_upgrades.len());
        for entry in &raw.config.precompile_upgrades {
            for (key, cfg) in &entry.0 {
                let block_timestamp = cfg
                    .get("blockTimestamp")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        Error::GenesisParse(format!(
                            "precompileUpgrade {key:?} missing blockTimestamp"
                        ))
                    })?;
                precompile_upgrades.push(PrecompileUpgrade {
                    key: key.clone(),
                    block_timestamp,
                });
            }
        }

        Ok(Self {
            chain_id: raw.config.chain_id,
            timestamp,
            number,
            gas_limit,
            gas_used,
            difficulty,
            nonce,
            extra,
            mix_digest,
            coinbase,
            parent_hash,
            base_fee,
            excess_blob_gas,
            blob_gas_used,
            alloc_bundle,
            bytecode,
            precompile_upgrades,
        })
    }

    /// The EVM chain id (43114 Mainnet, 43113 Fuji).
    #[must_use]
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// The genesis header timestamp (unix seconds). Mainnet and Fuji embed `0`;
    /// the Local/Default genesis embeds `unix(upgrade::InitiallyActiveTime)`
    /// (specs 23 §3.6).
    #[must_use]
    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    /// The parsed `precompileUpgrades` schedule (§8.3).
    #[must_use]
    pub fn precompile_upgrades(&self) -> &[PrecompileUpgrade] {
        &self.precompile_upgrades
    }

    /// The genesis `alloc` as a revm [`BundleState`] (5-field `rlp_account`,
    /// M6.30). Feed it through
    /// [`FirewoodStateProvider::propose_from_bundle`](crate::state::FirewoodStateProvider::propose_from_bundle)
    /// + `commit` to obtain (and durably set) the genesis state root.
    #[must_use]
    pub fn alloc_bundle(&self) -> &BundleState {
        &self.alloc_bundle
    }

    /// The `code_hash -> bytecode` pairs for every contract account in the
    /// `alloc`; seed the bytecode side store with these so contract code reads
    /// resolve (the genesis state root only commits the `code_hash`).
    #[must_use]
    pub fn bytecode(&self) -> &[(B256, Vec<u8>)] {
        &self.bytecode
    }

    /// Builds the coreth genesis [`AvaHeader`] given the computed genesis
    /// `state_root` and the network's activation schedule (spec 10 §9.3 /
    /// §11.1, coreth `Genesis.toBlock`).
    ///
    /// coreth's `toBlock` fills the fork-gated optional header tail for every
    /// upgrade **active at the genesis timestamp** (`core/genesis.go`):
    ///
    /// * AP3 ⇒ `BaseFee` (`g.BaseFee`, else `ap3.InitialBaseFee` = 225 gwei);
    /// * Etna ⇒ `ExtDataGasUsed = 0`, `BlockGasCost = 0`;
    /// * Cancun (aligned to Etna, `SetEthUpgrades`) ⇒ `ParentBeaconRoot =
    ///   0x0…0`, `ExcessBlobGas`/`BlobGasUsed` (`g.…`, else `0`);
    /// * Granite ⇒ `TimeMilliseconds = timestamp * 1000`, `MinDelayExcess =
    ///   acp226.InitialDelayExcess`.
    ///
    /// For the embedded Mainnet/Fuji genesis (timestamp 0, nothing active at
    /// genesis) the tail is empty — the 15 standard Ethereum fields +
    /// `ext_data_hash` only. For `network-id=local` (genesis timestamp ==
    /// `InitiallyActiveTime`, AP1→Granite all active **at** genesis) every tail
    /// field above is present — omitting them was the M9.15 rung-4 C-Chain
    /// genesis identity divergence. Helicon would append further fields
    /// (coreth `IsHelicon` branch) but is unscheduled on every network and
    /// [`AvaHeader`] carries no Helicon fields yet.
    ///
    /// `tx_root`/`receipt_root` are the empty-trie root; `uncle_hash` is the
    /// empty-ommers hash; `ext_data_hash` is the zero hash (coreth leaves the
    /// genesis header `ExtDataHash` unset — it is the zero value, NOT
    /// `EmptyExtDataHash`).
    #[must_use]
    pub fn genesis_header(&self, state_root: B256, upgrades: &NetworkUpgrades) -> AvaHeader {
        let ts = self.timestamp;
        let ap3 = upgrades.activation(AvaPhase::ApricotPhase3) <= ts;
        let etna = upgrades.activation(AvaPhase::Etna) <= ts;
        let granite = upgrades.activation(AvaPhase::Granite) <= ts;
        AvaHeader {
            parent_hash: self.parent_hash,
            uncle_hash: EMPTY_OMMER_ROOT_HASH,
            coinbase: self.coinbase,
            state_root,
            tx_root: ava_evm_reth::EMPTY_ROOT_HASH,
            receipt_root: ava_evm_reth::EMPTY_ROOT_HASH,
            bloom: Bytes::from(vec![0u8; 256]),
            difficulty: self.difficulty,
            number: self.number,
            gas_limit: self.gas_limit,
            gas_used: self.gas_used,
            time: ts,
            extra: self.extra.clone(),
            mix_digest: self.mix_digest,
            nonce: self.nonce,
            // coreth's genesis header leaves ExtDataHash as the zero value (the
            // genesis block has no ExtData and toBlock never computes the hash).
            ext_data_hash: B256::ZERO,
            // AP3: the genesis `baseFeePerGas`, defaulting to ap3.InitialBaseFee
            // (== ap3.MaxBaseFee, 225 gwei).
            base_fee: ap3.then(|| {
                self.base_fee
                    .unwrap_or(crate::feerules::window::BaseFeeParams::ap3().max_base_fee)
            }),
            // Etna: decoded-genesis consistency zeros (coreth toBlock).
            ext_data_gas_used: etna.then_some(U256::ZERO),
            block_gas_cost: etna.then_some(U256::ZERO),
            // Cancun is aligned to Etna (`SetEthUpgrades`).
            blob_gas_used: etna.then(|| self.blob_gas_used.unwrap_or(0)),
            excess_blob_gas: etna.then(|| self.excess_blob_gas.unwrap_or(0)),
            parent_beacon_root: etna.then_some(B256::ZERO),
            // Granite: millisecond timestamp + the ACP-226 initial delay excess.
            time_milliseconds: granite.then(|| ts.saturating_mul(1000)),
            min_delay_excess: granite.then_some(crate::feerules::acp226::INITIAL_DELAY_EXCESS.0),
        }
    }

    /// The complete genesis **state** for a network: the `alloc` bundle plus
    /// the stateful-precompile **activation accounts** coreth writes before
    /// computing the genesis state root, and the full `code_hash -> bytecode`
    /// side-store seed list (spec 10 §11.1; coreth `toBlock` →
    /// `ApplyPrecompileActivations`).
    ///
    /// coreth's `parseGenesis` schedules the Warp precompile at the Durango
    /// timestamp; when Durango is active **at** the genesis timestamp (every
    /// `network-id=local` network — never Mainnet/Fuji, whose genesis is
    /// timestamp 0), activation writes the "deployed contract" marker into the
    /// genesis state: `nonce = 1`, `code = [0x01]` at the warp precompile
    /// address (`core/state_processor_ext.go`; warp's `Configure` writes no
    /// further state). Omitting that account was the other half of the M9.15
    /// rung-4 C-Chain genesis divergence (state root).
    ///
    /// JSON-scheduled `precompileUpgrades` (§8.3) are a subnet-evm surface; the
    /// C-Chain registers only the warp module, so they never activate at the
    /// C-Chain genesis and are not materialized here.
    #[must_use]
    pub fn genesis_alloc(&self, upgrades: &NetworkUpgrades) -> (BundleState, Vec<(B256, Vec<u8>)>) {
        let mut bundle = self.alloc_bundle.clone();
        let mut bytecode = self.bytecode.clone();
        if upgrades.activation(AvaPhase::Durango) <= self.timestamp {
            let warp_code = vec![0x01u8];
            let warp_code_hash = keccak256(&warp_code);
            let activation = BundleState::builder(0..=0)
                .state_present_account_info(
                    crate::precompile::warp::WARP_PRECOMPILE_ADDRESS,
                    AccountInfo {
                        balance: U256::ZERO,
                        nonce: 1,
                        code_hash: warp_code_hash,
                        code: Some(Bytecode::new_raw(Bytes::from(warp_code.clone()))),
                        ..Default::default()
                    },
                )
                .build();
            bundle.extend(activation);
            bytecode.push((warp_code_hash, warp_code));
        }
        (bundle, bytecode)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ava_types::constants::MAINNET_ID;
    use pretty_assertions::assert_eq;
    use serde::Deserialize;

    use super::*;

    /// One golden-vector row: an Avalanche phase, its mainnet activation unix
    /// timestamp, and the revm `SpecId` coreth pins for it.
    #[derive(Debug, Deserialize)]
    struct ForkRow {
        phase: String,
        activation_unix: u64,
        spec_id: String,
    }

    #[derive(Debug, Deserialize)]
    struct ForkVector {
        network: String,
        rows: Vec<ForkRow>,
    }

    fn mainnet_spec() -> AvaChainSpec {
        // Mainnet C-Chain id 43114.
        AvaChainSpec::c_chain(MAINNET_ID, Chain::from_id(43114))
    }

    fn phase_from_name(name: &str) -> AvaPhase {
        AvaPhase::ALL
            .into_iter()
            .find(|p| p.name() == name)
            .unwrap_or_else(|| panic!("unknown phase name in vector: {name}"))
    }

    fn spec_id_from_name(name: &str) -> SpecId {
        match name {
            "ISTANBUL" => SpecId::ISTANBUL,
            "BERLIN" => SpecId::BERLIN,
            "LONDON" => SpecId::LONDON,
            "SHANGHAI" => SpecId::SHANGHAI,
            "CANCUN" => SpecId::CANCUN,
            other => panic!("unexpected spec id in vector: {other}"),
        }
    }

    #[test]
    fn fork_at_and_spec_id_match_coreth() {
        let spec = mainnet_spec();

        let raw = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/vectors/cchain/fork_schedule/mainnet.json"
        ))
        .expect("golden vector present");
        let vector: ForkVector = serde_json::from_str(&raw).expect("vector parses");
        assert_eq!(vector.network, "mainnet");

        for row in &vector.rows {
            let want_phase = phase_from_name(&row.phase);
            let want_spec = spec_id_from_name(&row.spec_id);

            // The highest phase exactly at `activation_unix` IS this phase (every
            // mainnet phase has a distinct activation second).
            assert_eq!(
                spec.fork_at(row.activation_unix),
                want_phase,
                "fork_at at exact activation of {}",
                row.phase
            );

            // The revm SpecId at this phase matches coreth's pinned mapping.
            assert_eq!(
                spec.revm_spec_id(row.activation_unix),
                want_spec,
                "revm_spec_id for {}",
                row.phase
            );

            // One second before activation, this phase is NOT yet active.
            if row.activation_unix > 0 {
                assert!(
                    spec.fork_at(row.activation_unix - 1) < want_phase,
                    "phase {} not active one second before its activation",
                    row.phase
                );
            }
        }
    }

    #[test]
    fn spec_id_progression_is_monotonic() {
        let spec = mainnet_spec();
        // Walking the schedule forward, the revm SpecId never regresses.
        let mut last = SpecId::ISTANBUL;
        for phase in AvaPhase::ALL {
            let t = spec.network_upgrades().activation(phase);
            let id = spec.revm_spec_id(t);
            assert!(id >= last, "spec id regressed at {phase:?}");
            last = id;
        }
        // After Granite the spec id is Cancun (no Prague mapping in coreth).
        let granite_t = spec.network_upgrades().granite;
        assert_eq!(spec.revm_spec_id(granite_t), SpecId::CANCUN);
        // Before any Apricot fork it is Istanbul-era.
        assert_eq!(spec.revm_spec_id(0), SpecId::ISTANBUL);
    }

    #[test]
    fn ethereum_fork_activation_maps_to_phase_timestamps() {
        let spec = mainnet_spec();
        let up = *spec.network_upgrades();
        // coreth: Berlin←AP2, London←AP3, Shanghai←Durango, Cancun←Etna.
        assert_eq!(
            spec.ethereum_fork_activation(EthereumHardfork::Berlin),
            ForkCondition::Timestamp(up.apricot_phase_2)
        );
        assert_eq!(
            spec.ethereum_fork_activation(EthereumHardfork::London),
            ForkCondition::Timestamp(up.apricot_phase_3)
        );
        assert_eq!(
            spec.ethereum_fork_activation(EthereumHardfork::Shanghai),
            ForkCondition::Timestamp(up.durango)
        );
        assert_eq!(
            spec.ethereum_fork_activation(EthereumHardfork::Cancun),
            ForkCondition::Timestamp(up.etna)
        );
        // Pre-AP2 forks (and Paris) are active from genesis, keyed by block.
        assert_eq!(
            spec.ethereum_fork_activation(EthereumHardfork::Istanbul),
            ForkCondition::Block(0)
        );
        assert_eq!(
            spec.ethereum_fork_activation(EthereumHardfork::Paris),
            ForkCondition::Block(0)
        );
    }

    #[test]
    fn check_compatible_rejects_activated_fork_change() {
        let spec = mainnet_spec();
        let base = *spec.network_upgrades();

        // A head far in the future: every fork has activated.
        let head_ts = base.granite + 1;

        // Identical schedule is compatible.
        assert!(spec.check_compatible(&base, head_ts).is_ok());

        // Rescheduling an already-activated fork (Durango) is rejected.
        let mut changed = base;
        changed.durango += 1;
        let err = spec
            .check_compatible(&changed, head_ts)
            .expect_err("rescheduling an activated fork must be rejected");
        assert!(matches!(
            err,
            AvaEvmError::IncompatibleFork { fork } if fork == "Durango"
        ));
    }

    #[test]
    fn check_compatible_allows_future_fork_reschedule() {
        // Build a schedule with a far-future Granite that is NOT yet active.
        let mut up = NetworkUpgrades::for_network(MAINNET_ID);
        up.granite = u64::MAX;
        let spec = AvaChainSpec::from_parts(up, Chain::from_id(43114), false);

        // Head between Etna and the (future) Granite.
        let head_ts = up.etna + 1;

        // Rescheduling the not-yet-activated Granite is allowed.
        let mut changed = up;
        changed.granite = up.etna + 1_000;
        assert!(
            spec.check_compatible(&changed, head_ts).is_ok(),
            "rescheduling a future (unactivated) fork must be allowed"
        );
    }

    #[test]
    fn eth_chain_spec_chain_id_matches() {
        let spec = mainnet_spec();
        assert_eq!(EthChainSpec::chain(&spec).id(), 43114);
        assert_eq!(spec.chain_id(), 43114);
        assert!(!spec.is_subnet());
    }
}
