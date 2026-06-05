// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `UpgradeConfig` + `Fork` + the network-upgrade activation schedule.
//!
//! Mirrors `upgrade/upgrade.go` from Go. The upgrade schedule is a set of
//! **protocol constants** — activation times for each named fork, identified by
//! network ID (Mainnet/Fuji/Default). Activation is purely time-based:
//! `is_active(fork, t) ⟺ t >= fork_time(fork)`.
//!
//! Owning spec: `specs/03-core-primitives.md` §5.2 + §11.2.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};

use ava_types::id::Id;

use crate::error::{Error, Result};

// ── Network IDs (from ava-types::constants) ───────────────────────────────────

use ava_types::constants::{FUJI_ID, MAINNET_ID};

// ── Special activation time constants ────────────────────────────────────────

/// `2020-12-05 05:00:00 UTC` — the genesis time used for the Default (local)
/// config. All forks except Helicon are set to this time in the default config,
/// meaning they are effectively all activated from genesis.
///
/// Mirrors Go `upgrade.go: InitiallyActiveTime`.
pub fn initially_active_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2020, 12, 5, 5, 0, 0)
        .single()
        .expect("static: always valid")
}

/// `9999-12-01 00:00:00 UTC` — used for forks that have not yet been scheduled.
///
/// Mirrors Go `upgrade.go: UnscheduledActivationTime`.
pub fn unscheduled_activation_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(9999, 12, 1, 0, 0, 0)
        .single()
        .expect("static: always valid")
}

// ── CB58 decode helper (inline — avoids ava-utils cycle) ─────────────────────

/// Decodes a CB58-encoded string into a 32-byte array.
///
/// CB58 format: `bs58(bytes ++ sha256(bytes)[..4])`. We strip the 4-byte
/// checksum and return the payload bytes as an [`Id`].
///
/// This is used only for compile-time constant initialization of the
/// `CortinaXChainStopVertexID` side-params.
fn cb58_to_id(s: &str) -> Id {
    let decoded = bs58::decode(s)
        .into_vec()
        .expect("static CB58 IDs must be valid base58");
    assert!(
        decoded.len() > 4,
        "CB58 payload too short: {s}"
    );
    let payload = &decoded[..decoded.len() - 4];
    Id::from_slice(payload)
        .unwrap_or_else(|e| panic!("CB58 ID {s} decoded to wrong length: {e}"))
}

// ── Fork enum ─────────────────────────────────────────────────────────────────

/// A named network-upgrade fork. Variants are in **chronological order**
/// (i.e. `ApricotPhase1 < ApricotPhase2 < … < Helicon`), which is why
/// `#[derive(Ord)]` gives the correct "before-or-same" relation.
///
/// There are exactly 15 time-gated forks. The height/id/duration side-params
/// (`ApricotPhase4MinPChainHeight`, `CortinaXChainStopVertexID`,
/// `GraniteEpochDuration`) live on [`UpgradeConfig`] — they are not `Fork`
/// variants because they are not time-gated.
///
/// Mirrors Go `upgrade.go` (the `IsApricotPhase1Activated`/… naming).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum Fork {
    /// Apricot Phase 1. `upgrade.go:IsApricotPhase1Activated`.
    ApricotPhase1,
    /// Apricot Phase 2. `upgrade.go:IsApricotPhase2Activated`.
    ApricotPhase2,
    /// Apricot Phase 3. `upgrade.go:IsApricotPhase3Activated`.
    ApricotPhase3,
    /// Apricot Phase 4. `upgrade.go:IsApricotPhase4Activated`.
    ApricotPhase4,
    /// Apricot Phase 5. `upgrade.go:IsApricotPhase5Activated`.
    ApricotPhase5,
    /// Apricot Phase Pre-6. `upgrade.go:IsApricotPhasePre6Activated`.
    ApricotPhasePre6,
    /// Apricot Phase 6. `upgrade.go:IsApricotPhase6Activated`.
    ApricotPhase6,
    /// Apricot Phase Post-6. `upgrade.go:IsApricotPhasePost6Activated`.
    ApricotPhasePost6,
    /// Banff. `upgrade.go:IsBanffActivated`.
    Banff,
    /// Cortina. `upgrade.go:IsCortinaActivated`.
    Cortina,
    /// Durango. `upgrade.go:IsDurangoActivated`.
    Durango,
    /// Etna. `upgrade.go:IsEtnaActivated`.
    Etna,
    /// Fortuna. `upgrade.go:IsFortunaActivated`.
    Fortuna,
    /// Granite. `upgrade.go:IsGraniteActivated`.
    Granite,
    /// Helicon. `upgrade.go:IsHeliconActivated`. Currently unscheduled on all networks.
    Helicon,
}

impl Fork {
    /// All 15 time-gated forks in chronological order.
    ///
    /// This slice is used by [`UpgradeConfig::validate`] (monotonicity check) and
    /// [`UpgradeConfig::fork_at`] (last-active scan). The order MUST match the
    /// enum discriminant order above.
    pub const ALL: [Fork; 15] = [
        Fork::ApricotPhase1,
        Fork::ApricotPhase2,
        Fork::ApricotPhase3,
        Fork::ApricotPhase4,
        Fork::ApricotPhase5,
        Fork::ApricotPhasePre6,
        Fork::ApricotPhase6,
        Fork::ApricotPhasePost6,
        Fork::Banff,
        Fork::Cortina,
        Fork::Durango,
        Fork::Etna,
        Fork::Fortuna,
        Fork::Granite,
        Fork::Helicon,
    ];
}

// ── UpgradeConfig ─────────────────────────────────────────────────────────────

/// The complete network-upgrade activation schedule for a single network.
///
/// Each `*_time` field is a [`DateTime<Utc>`]; activation of the corresponding
/// fork is inclusive at the boundary: `is_active(fork, t) ⟺ t >= fork_time`.
///
/// The three non-time side-params (`apricot_phase_4_min_p_chain_height`,
/// `cortina_x_chain_stop_vertex_id`, `granite_epoch_duration`) are carried here
/// but are **excluded** from the [`validate`][UpgradeConfig::validate] ordering
/// check (matching Go's `upgrades` slice which only includes time fields).
///
/// Mirrors `upgrade.Config` from Go (`upgrade/upgrade.go`).
pub struct UpgradeConfig {
    /// `upgrade.go:ApricotPhase1Time` (`upgrade.go:20`)
    pub apricot_phase_1_time: DateTime<Utc>,
    /// `upgrade.go:ApricotPhase2Time` (`upgrade.go:21`)
    pub apricot_phase_2_time: DateTime<Utc>,
    /// `upgrade.go:ApricotPhase3Time` (`upgrade.go:22`)
    pub apricot_phase_3_time: DateTime<Utc>,
    /// `upgrade.go:ApricotPhase4Time` (`upgrade.go:23`)
    pub apricot_phase_4_time: DateTime<Utc>,
    /// Height-based side-param (not time-gated). `upgrade.go:ApricotPhase4MinPChainHeight`.
    pub apricot_phase_4_min_p_chain_height: u64,
    /// `upgrade.go:ApricotPhase5Time` (`upgrade.go:25`)
    pub apricot_phase_5_time: DateTime<Utc>,
    /// `upgrade.go:ApricotPhasePre6Time` (`upgrade.go:26`)
    pub apricot_phase_pre_6_time: DateTime<Utc>,
    /// `upgrade.go:ApricotPhase6Time` (`upgrade.go:27`)
    pub apricot_phase_6_time: DateTime<Utc>,
    /// `upgrade.go:ApricotPhasePost6Time` (`upgrade.go:28`)
    pub apricot_phase_post_6_time: DateTime<Utc>,
    /// `upgrade.go:BanffTime` (`upgrade.go:29`)
    pub banff_time: DateTime<Utc>,
    /// `upgrade.go:CortinaTime` (`upgrade.go:30`)
    pub cortina_time: DateTime<Utc>,
    /// ID-based side-param (not time-gated). `upgrade.go:CortinaXChainStopVertexID`.
    pub cortina_x_chain_stop_vertex_id: Id,
    /// `upgrade.go:DurangoTime` (`upgrade.go:32`)
    pub durango_time: DateTime<Utc>,
    /// `upgrade.go:EtnaTime` (`upgrade.go:33`)
    pub etna_time: DateTime<Utc>,
    /// `upgrade.go:FortunaTime` (`upgrade.go:34`)
    pub fortuna_time: DateTime<Utc>,
    /// `upgrade.go:GraniteTime` (`upgrade.go:35`)
    pub granite_time: DateTime<Utc>,
    /// Duration-based side-param (not time-gated). `upgrade.go:GraniteEpochDuration`.
    pub granite_epoch_duration: Duration,
    /// `upgrade.go:HeliconTime` (`upgrade.go:37`)
    pub helicon_time: DateTime<Utc>,
}

impl UpgradeConfig {
    /// Returns the activation time for the given fork.
    ///
    /// Each arm maps to the corresponding `*_time` field.
    pub fn fork_time(&self, fork: Fork) -> DateTime<Utc> {
        match fork {
            Fork::ApricotPhase1 => self.apricot_phase_1_time,
            Fork::ApricotPhase2 => self.apricot_phase_2_time,
            Fork::ApricotPhase3 => self.apricot_phase_3_time,
            Fork::ApricotPhase4 => self.apricot_phase_4_time,
            Fork::ApricotPhase5 => self.apricot_phase_5_time,
            Fork::ApricotPhasePre6 => self.apricot_phase_pre_6_time,
            Fork::ApricotPhase6 => self.apricot_phase_6_time,
            Fork::ApricotPhasePost6 => self.apricot_phase_post_6_time,
            Fork::Banff => self.banff_time,
            Fork::Cortina => self.cortina_time,
            Fork::Durango => self.durango_time,
            Fork::Etna => self.etna_time,
            Fork::Fortuna => self.fortuna_time,
            Fork::Granite => self.granite_time,
            Fork::Helicon => self.helicon_time,
        }
    }

    /// **THE canonical activation gate.**
    ///
    /// Returns `true` iff `t >= fork_time(fork)`.
    /// Matches Go `!t.Before(forkTime)` (inclusive at the boundary).
    ///
    /// Mirrors Go `upgrade.go:IsApricotPhase1Activated` etc.
    pub fn is_active(&self, fork: Fork, t: DateTime<Utc>) -> bool {
        t >= self.fork_time(fork)
    }

    /// Returns the most-recent fork active at time `t` (the highest-ordered fork
    /// whose `fork_time <= t`), or `None` if `t` precedes `ApricotPhase1`.
    ///
    /// Mirrors Go `upgrade.go:ForkAt` (if such a function existed — derived from
    /// the `IsXActivated` structure). The scan is over `Fork::ALL` in reverse.
    pub fn fork_at(&self, t: DateTime<Utc>) -> Option<Fork> {
        Fork::ALL.iter().rev().copied().find(|&f| self.is_active(f, t))
    }

    /// Validates that the 15 time-gated fork times are monotonically non-decreasing
    /// in `Fork::ALL` order. Returns `Err(Error::InvalidUpgradeTimes)` if any
    /// adjacent pair is out of order.
    ///
    /// The three non-time side-params (`apricot_phase_4_min_p_chain_height`,
    /// `cortina_x_chain_stop_vertex_id`, `granite_epoch_duration`) are **excluded**
    /// from this check (mirrors Go's `upgrades` slice which only holds time fields).
    ///
    /// Mirrors Go `upgrade.go:Verify`.
    pub fn validate(&self) -> Result<()> {
        for w in Fork::ALL.windows(2) {
            if self.fork_time(w[0]) > self.fork_time(w[1]) {
                return Err(Error::InvalidUpgradeTimes);
            }
        }
        Ok(())
    }

    // ── Per-phase thin forwarders (mirrors Go `IsXActivated` methods) ──────

    /// `upgrade.go:IsApricotPhase1Activated`
    pub fn is_apricot_phase_1_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::ApricotPhase1, t)
    }

    /// `upgrade.go:IsApricotPhase2Activated`
    pub fn is_apricot_phase_2_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::ApricotPhase2, t)
    }

    /// `upgrade.go:IsApricotPhase3Activated`
    pub fn is_apricot_phase_3_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::ApricotPhase3, t)
    }

    /// `upgrade.go:IsApricotPhase4Activated`
    pub fn is_apricot_phase_4_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::ApricotPhase4, t)
    }

    /// `upgrade.go:IsApricotPhase5Activated`
    pub fn is_apricot_phase_5_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::ApricotPhase5, t)
    }

    /// `upgrade.go:IsApricotPhasePre6Activated`
    pub fn is_apricot_phase_pre_6_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::ApricotPhasePre6, t)
    }

    /// `upgrade.go:IsApricotPhase6Activated`
    pub fn is_apricot_phase_6_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::ApricotPhase6, t)
    }

    /// `upgrade.go:IsApricotPhasePost6Activated`
    pub fn is_apricot_phase_post_6_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::ApricotPhasePost6, t)
    }

    /// `upgrade.go:IsBanffActivated`
    pub fn is_banff_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::Banff, t)
    }

    /// `upgrade.go:IsCortinaActivated`
    pub fn is_cortina_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::Cortina, t)
    }

    /// `upgrade.go:IsDurangoActivated`
    pub fn is_durango_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::Durango, t)
    }

    /// `upgrade.go:IsEtnaActivated`
    pub fn is_etna_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::Etna, t)
    }

    /// `upgrade.go:IsFortunaActivated`
    pub fn is_fortuna_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::Fortuna, t)
    }

    /// `upgrade.go:IsGraniteActivated`
    pub fn is_granite_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::Granite, t)
    }

    /// `upgrade.go:IsHeliconActivated`
    pub fn is_helicon_activated(&self, t: DateTime<Utc>) -> bool {
        self.is_active(Fork::Helicon, t)
    }
}

// ── Network configs ───────────────────────────────────────────────────────────

/// Returns the [`UpgradeConfig`] for the given network ID.
///
/// - `MAINNET_ID (1)` → Mainnet constants
/// - `FUJI_ID (5)` → Fuji (testnet) constants
/// - any other ID → Default (local) config — all phases = `InitiallyActiveTime`
///   except Helicon = `UnscheduledActivationTime`
///
/// Mirrors Go `upgrade.go:GetConfig(networkID uint32) Config`.
pub fn get_config(network_id: u32) -> UpgradeConfig {
    match network_id {
        id if id == MAINNET_ID => mainnet_config(),
        id if id == FUJI_ID => fuji_config(),
        _ => default_config(),
    }
}

/// Mainnet upgrade schedule.
///
/// Verbatim constants from `upgrade/upgrade.go:20–41` (Go source).
/// All times are UTC.
fn mainnet_config() -> UpgradeConfig {
    UpgradeConfig {
        // `upgrade.go:ApricotPhase1Time` — 2021-03-31 14:00:00 UTC
        apricot_phase_1_time: Utc
            .with_ymd_and_hms(2021, 3, 31, 14, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:ApricotPhase2Time` — 2021-05-10 11:00:00 UTC
        apricot_phase_2_time: Utc
            .with_ymd_and_hms(2021, 5, 10, 11, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:ApricotPhase3Time` — 2021-08-24 14:00:00 UTC
        apricot_phase_3_time: Utc
            .with_ymd_and_hms(2021, 8, 24, 14, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:ApricotPhase4Time` — 2021-09-22 21:00:00 UTC
        apricot_phase_4_time: Utc
            .with_ymd_and_hms(2021, 9, 22, 21, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:ApricotPhase4MinPChainHeight` = 793005
        apricot_phase_4_min_p_chain_height: 793_005,
        // `upgrade.go:ApricotPhase5Time` — 2021-12-02 18:00:00 UTC
        apricot_phase_5_time: Utc
            .with_ymd_and_hms(2021, 12, 2, 18, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:ApricotPhasePre6Time` — 2022-09-05 01:30:00 UTC
        apricot_phase_pre_6_time: Utc
            .with_ymd_and_hms(2022, 9, 5, 1, 30, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:ApricotPhase6Time` — 2022-09-06 20:00:00 UTC
        apricot_phase_6_time: Utc
            .with_ymd_and_hms(2022, 9, 6, 20, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:ApricotPhasePost6Time` — 2022-09-07 03:00:00 UTC
        apricot_phase_post_6_time: Utc
            .with_ymd_and_hms(2022, 9, 7, 3, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:BanffTime` — 2022-10-18 16:00:00 UTC
        banff_time: Utc
            .with_ymd_and_hms(2022, 10, 18, 16, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:CortinaTime` — 2023-04-25 15:00:00 UTC
        cortina_time: Utc
            .with_ymd_and_hms(2023, 4, 25, 15, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:CortinaXChainStopVertexID` — CB58: jrGWDh5Po9FMj54depyunNixpia5PN4aAYxfmNzU8n752Rjga
        cortina_x_chain_stop_vertex_id: cb58_to_id(
            "jrGWDh5Po9FMj54depyunNixpia5PN4aAYxfmNzU8n752Rjga",
        ),
        // `upgrade.go:DurangoTime` — 2024-03-06 16:00:00 UTC
        durango_time: Utc
            .with_ymd_and_hms(2024, 3, 6, 16, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:EtnaTime` — 2024-12-16 17:00:00 UTC
        etna_time: Utc
            .with_ymd_and_hms(2024, 12, 16, 17, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FortunaTime` — 2025-04-08 15:00:00 UTC
        fortuna_time: Utc
            .with_ymd_and_hms(2025, 4, 8, 15, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:GraniteTime` — 2025-11-19 16:00:00 UTC
        granite_time: Utc
            .with_ymd_and_hms(2025, 11, 19, 16, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:GraniteEpochDuration` = 5 minutes
        granite_epoch_duration: Duration::from_secs(5 * 60),
        // `upgrade.go:HeliconTime` — 9999-12-01 00:00:00 UTC (unscheduled)
        helicon_time: unscheduled_activation_time(),
    }
}

/// Fuji (testnet) upgrade schedule.
///
/// Verbatim constants from `upgrade/upgrade.go:44–65` (Go source).
/// All times are UTC.
fn fuji_config() -> UpgradeConfig {
    UpgradeConfig {
        // `upgrade.go:FujiApricotPhase1Time` — 2021-03-26 14:00:00 UTC
        apricot_phase_1_time: Utc
            .with_ymd_and_hms(2021, 3, 26, 14, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiApricotPhase2Time` — 2021-05-05 14:00:00 UTC
        apricot_phase_2_time: Utc
            .with_ymd_and_hms(2021, 5, 5, 14, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiApricotPhase3Time` — 2021-08-16 19:00:00 UTC
        apricot_phase_3_time: Utc
            .with_ymd_and_hms(2021, 8, 16, 19, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiApricotPhase4Time` — 2021-09-16 21:00:00 UTC
        apricot_phase_4_time: Utc
            .with_ymd_and_hms(2021, 9, 16, 21, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiApricotPhase4MinPChainHeight` = 47437
        apricot_phase_4_min_p_chain_height: 47_437,
        // `upgrade.go:FujiApricotPhase5Time` — 2021-11-24 15:00:00 UTC
        apricot_phase_5_time: Utc
            .with_ymd_and_hms(2021, 11, 24, 15, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiApricotPhasePre6Time` — 2022-09-06 20:00:00 UTC
        apricot_phase_pre_6_time: Utc
            .with_ymd_and_hms(2022, 9, 6, 20, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiApricotPhase6Time` — 2022-09-06 20:00:00 UTC
        apricot_phase_6_time: Utc
            .with_ymd_and_hms(2022, 9, 6, 20, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiApricotPhasePost6Time` — 2022-09-07 06:00:00 UTC
        apricot_phase_post_6_time: Utc
            .with_ymd_and_hms(2022, 9, 7, 6, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiBanffTime` — 2022-10-03 14:00:00 UTC
        banff_time: Utc
            .with_ymd_and_hms(2022, 10, 3, 14, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiCortinaTime` — 2023-04-06 15:00:00 UTC
        cortina_time: Utc
            .with_ymd_and_hms(2023, 4, 6, 15, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiCortinaXChainStopVertexID` — CB58: 2D1cmbiG36BqQMRyHt4kFhWarmatA1ighSpND3FeFgz3vFVtCZ
        cortina_x_chain_stop_vertex_id: cb58_to_id(
            "2D1cmbiG36BqQMRyHt4kFhWarmatA1ighSpND3FeFgz3vFVtCZ",
        ),
        // `upgrade.go:FujiDurangoTime` — 2024-02-13 16:00:00 UTC
        durango_time: Utc
            .with_ymd_and_hms(2024, 2, 13, 16, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiEtnaTime` — 2024-11-25 16:00:00 UTC
        etna_time: Utc
            .with_ymd_and_hms(2024, 11, 25, 16, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiFortunaTime` — 2025-03-13 15:00:00 UTC
        fortuna_time: Utc
            .with_ymd_and_hms(2025, 3, 13, 15, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiGraniteTime` — 2025-10-29 15:00:00 UTC
        granite_time: Utc
            .with_ymd_and_hms(2025, 10, 29, 15, 0, 0)
            .single()
            .expect("static: always valid"),
        // `upgrade.go:FujiGraniteEpochDuration` = 5 minutes
        granite_epoch_duration: Duration::from_secs(5 * 60),
        // `upgrade.go:FujiHeliconTime` — 9999-12-01 00:00:00 UTC (unscheduled)
        helicon_time: unscheduled_activation_time(),
    }
}

/// Default (local/custom) upgrade schedule.
///
/// All time-gated phases are set to `InitiallyActiveTime` (2020-12-05 05:00:00 UTC),
/// meaning they are all effectively activated from genesis. Helicon is unscheduled.
///
/// Mirrors Go `upgrade.go:GetConfig` default branch.
fn default_config() -> UpgradeConfig {
    let t = initially_active_time();
    UpgradeConfig {
        apricot_phase_1_time: t,
        apricot_phase_2_time: t,
        apricot_phase_3_time: t,
        apricot_phase_4_time: t,
        apricot_phase_4_min_p_chain_height: 0,
        apricot_phase_5_time: t,
        apricot_phase_pre_6_time: t,
        apricot_phase_6_time: t,
        apricot_phase_post_6_time: t,
        banff_time: t,
        cortina_time: t,
        cortina_x_chain_stop_vertex_id: Id::EMPTY,
        durango_time: t,
        etna_time: t,
        fortuna_time: t,
        granite_time: t,
        granite_epoch_duration: Duration::from_secs(30),
        helicon_time: unscheduled_activation_time(),
    }
}
