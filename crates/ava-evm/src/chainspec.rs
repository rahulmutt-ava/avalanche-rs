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
    AvaEvmError, B256, BaseFeeParams, BlobParams, Chain, ChainHardforks, ChainSpec,
    DepositContract, EthChainSpec, EthereumHardfork, EthereumHardforks, ForkCondition, Genesis,
    Hardfork, Header, NodeRecord, SpecId, U256,
};
use ava_version::upgrade::{Fork, UpgradeConfig, get_config};

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
        }
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
/// Inserts the pre-AP2 Ethereum forks at genesis (`Timestamp(0)`), then each
/// Ethereum fork coreth maps to a phase at that phase's activation time, then
/// every Avalanche phase. `ChainHardforks::insert` keeps the list ordered by
/// `ForkCondition`.
fn build_chain_hardforks(up: &NetworkUpgrades) -> ChainHardforks {
    let mut forks = ChainHardforks::new(Vec::new());

    // Pre-Apricot-Phase-2 Ethereum forks are all active from launch (coreth
    // `SetEthUpgrades` sets Homestead..MuirGlacier to block 0).
    for eth in [
        EthereumHardfork::Frontier,
        EthereumHardfork::Homestead,
        EthereumHardfork::Tangerine,
        EthereumHardfork::SpuriousDragon,
        EthereumHardfork::Byzantium,
        EthereumHardfork::Constantinople,
        EthereumHardfork::Petersburg,
        EthereumHardfork::Istanbul,
        EthereumHardfork::MuirGlacier,
    ] {
        forks.insert(AvaHardfork::Eth(eth), ForkCondition::Timestamp(0));
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
        // Pre-AP2 forks are active from genesis.
        assert_eq!(
            spec.ethereum_fork_activation(EthereumHardfork::Istanbul),
            ForkCondition::Timestamp(0)
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
