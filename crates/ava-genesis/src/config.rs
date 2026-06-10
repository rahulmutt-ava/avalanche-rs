// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The parsed genesis [`Config`] (+ [`Allocation`] / [`LockedAmount`] /
//! [`Staker`]) and the embedded Mainnet/Fuji/Local configs (`genesis/config.go`,
//! specs 23 §1/§5.1).
//!
//! The on-disk/embedded form is JSON with string-encoded addresses (the
//! [`unparsed`](crate::unparsed) form); the build pipeline operates on this
//! parsed form with [`ShortId`] addresses.

use std::cmp::Ordering;
use std::sync::LazyLock;

use ava_platformvm::signer::ProofOfPossession;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;

use crate::error::{GenesisError, Result};
use crate::unparsed::UnparsedConfig;

/// `genesis/genesis_mainnet.json`, embedded verbatim.
pub static MAINNET_GENESIS_CONFIG_JSON: &str = include_str!("../data/genesis_mainnet.json");
/// `genesis/genesis_fuji.json`, embedded verbatim.
pub static FUJI_GENESIS_CONFIG_JSON: &str = include_str!("../data/genesis_fuji.json");
/// `genesis/genesis_local.json`, embedded verbatim.
pub static LOCAL_GENESIS_CONFIG_JSON: &str = include_str!("../data/genesis_local.json");

/// `config.go::LockedAmount` — one entry of an allocation's unlock schedule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LockedAmount {
    /// `amount` — nAVAX locked until `locktime`.
    pub amount: u64,
    /// `locktime` — unix seconds; 0 = unlocked.
    pub locktime: u64,
}

/// `config.go::Allocation` (parsed form — 20-byte [`ShortId`] addresses).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Allocation {
    /// `ethAddr` — the claiming Ethereum address (`0x`-hex in JSON).
    pub eth_addr: ShortId,
    /// `avaxAddr` — the Avalanche address (bech32 in JSON).
    pub avax_addr: ShortId,
    /// `initialAmount` — nAVAX minted on the X-Chain at genesis.
    pub initial_amount: u64,
    /// `unlockSchedule` — P-Chain UTXOs (possibly time-locked).
    pub unlock_schedule: Vec<LockedAmount>,
}

impl Allocation {
    /// `Allocation.Compare` — primary key `initial_amount` ascending, tie-break
    /// `avax_addr` ascending (byte compare). This is the sort that fixes the
    /// X-Chain UTXO order (specs 23 §3.1).
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        self.initial_amount
            .cmp(&other.initial_amount)
            .then_with(|| self.avax_addr.cmp(&other.avax_addr))
    }
}

/// `config.go::Staker` (parsed form).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Staker {
    /// `nodeID`.
    pub node_id: NodeId,
    /// `rewardAddress` (bech32 in JSON).
    pub reward_address: ShortId,
    /// `delegationFee` — millionths (e.g. `20000` = 2%).
    pub delegation_fee: u32,
    /// `signer` — BLS proof of possession; `None` ⇒ legacy `AddValidatorTx`.
    pub signer: Option<ProofOfPossession>,
}

/// `config.go::Config` (parsed form) — genesis is fully determined by this
/// struct (specs 23 §1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// `networkID`.
    pub network_id: u32,
    /// `allocations`.
    pub allocations: Vec<Allocation>,
    /// `startTime` (unix seconds).
    pub start_time: u64,
    /// `initialStakeDuration` (seconds).
    pub initial_stake_duration: u64,
    /// `initialStakeDurationOffset` (seconds).
    pub initial_stake_duration_offset: u64,
    /// `initialStakedFunds` (bech32 in JSON).
    pub initial_staked_funds: Vec<ShortId>,
    /// `initialStakers`.
    pub initial_stakers: Vec<Staker>,
    /// `cChainGenesis` — a JSON **string** holding a go-ethereum genesis doc.
    pub c_chain_genesis: String,
    /// `message`.
    pub message: String,
}

impl Config {
    /// `Config.InitialSupply` — Σ over allocations of `initial_amount` + Σ
    /// `unlock_schedule[].amount`, with checked `u64` adds.
    ///
    /// # Errors
    /// Returns [`GenesisError::SupplyOverflow`] if the sum exceeds `u64::MAX`
    /// (mirrors Go `math.Add` failure).
    pub fn initial_supply(&self) -> Result<u64> {
        let mut initial_supply: u64 = 0;
        for allocation in &self.allocations {
            let mut new_supply = initial_supply
                .checked_add(allocation.initial_amount)
                .ok_or(GenesisError::SupplyOverflow)?;
            for unlock in &allocation.unlock_schedule {
                new_supply = new_supply
                    .checked_add(unlock.amount)
                    .ok_or(GenesisError::SupplyOverflow)?;
            }
            initial_supply = new_supply;
        }
        Ok(initial_supply)
    }
}

/// Parses an embedded config, panicking on failure exactly like Go's
/// `genesis/config.go::init()` (the embedded JSON is a compile-time constant —
/// a parse failure is a build defect, not a runtime condition).
#[allow(clippy::expect_used)]
fn parse_embedded(json: &str, which: &str) -> Config {
    let unparsed: UnparsedConfig =
        serde_json::from_str(json).unwrap_or_else(|e| panic!("embedded {which} JSON: {e}"));
    unparsed
        .parse()
        .unwrap_or_else(|e| panic!("embedded {which} config: {e}"))
}

/// `MainnetConfig` — the parsed embedded mainnet genesis config.
pub static MAINNET_CONFIG: LazyLock<Config> =
    LazyLock::new(|| parse_embedded(MAINNET_GENESIS_CONFIG_JSON, "mainnet"));

/// `FujiConfig` — the parsed embedded fuji genesis config.
pub static FUJI_CONFIG: LazyLock<Config> =
    LazyLock::new(|| parse_embedded(FUJI_GENESIS_CONFIG_JSON, "fuji"));

/// `unmodifiedLocalConfig` — the parsed embedded local genesis config **before**
/// the start-time advance (specs 23 §5.1). This is the config the Local golden
/// IDs are computed over.
pub static UNMODIFIED_LOCAL_CONFIG: LazyLock<Config> =
    LazyLock::new(|| parse_embedded(LOCAL_GENESIS_CONFIG_JSON, "local"));

/// `LocalConfig` — the **live** local genesis config: the embedded config with
/// `startTime` advanced to the most recent 9-month chunk `<= now`
/// (`getRecentStartTime`, specs 23 §5.1). Its genesis id is therefore
/// time-dependent — the golden tests pin [`UNMODIFIED_LOCAL_CONFIG`] instead.
pub static LOCAL_CONFIG: LazyLock<Config> = LazyLock::new(|| {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let mut config = UNMODIFIED_LOCAL_CONFIG.clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    config.start_time = crate::recent_start::get_recent_start_time(
        config.start_time,
        now,
        crate::recent_start::LOCAL_NETWORK_UPDATE_START_TIME_PERIOD_SECS,
    );
    config
});

/// `genesis.GetConfig` — the embedded config for `network_id`; unknown ids use
/// the (live) local config as a template with the network id overridden.
#[must_use]
pub fn get_config(network_id: u32) -> Config {
    use ava_types::constants::{FUJI_ID, LOCAL_ID, MAINNET_ID};
    match network_id {
        MAINNET_ID => MAINNET_CONFIG.clone(),
        FUJI_ID => FUJI_CONFIG.clone(),
        LOCAL_ID => LOCAL_CONFIG.clone(),
        other => {
            let mut config = LOCAL_CONFIG.clone();
            config.network_id = other;
            config
        }
    }
}

#[cfg(test)]
mod tests {
    use ava_types::constants::{self, FUJI_ID, LOCAL_ID, MAINNET_ID};

    use super::*;

    /// M8.5 red test: parse each embedded JSON unparsed→parsed and sanity-check
    /// the protocol constants (specs 23 §1/§5.1).
    #[test]
    fn parse_embedded_configs() {
        let cases: [(&Config, u32, &str); 3] = [
            (&MAINNET_CONFIG, MAINNET_ID, "avax"),
            (&FUJI_CONFIG, FUJI_ID, "fuji"),
            (&UNMODIFIED_LOCAL_CONFIG, LOCAL_ID, "local"),
        ];
        for (config, want_id, want_hrp) in cases {
            assert_eq!(config.network_id, want_id);
            assert_eq!(constants::get_hrp(config.network_id), want_hrp);
            assert!(
                config.initial_supply().expect("initial supply") > 0,
                "network {want_id} has no supply"
            );
            assert!(
                !config.initial_stakers.is_empty(),
                "network {want_id} has no stakers"
            );
            assert!(!config.c_chain_genesis.is_empty());
        }
        // The Local config carries BLS signers; Mainnet/Fuji predate them.
        assert!(
            UNMODIFIED_LOCAL_CONFIG
                .initial_stakers
                .iter()
                .all(|s| s.signer.is_some())
        );
        assert!(
            MAINNET_CONFIG
                .initial_stakers
                .iter()
                .all(|s| s.signer.is_none())
        );
    }

    /// The known initial supplies, computed independently from the JSON:
    /// mainnet `359_999_999.999_990_21` AVAX, fuji/local exactly 360M AVAX.
    #[test]
    fn initial_supply_known_values() {
        const NANO_AVAX_PER_AVAX: u64 = 1_000_000_000;
        assert_eq!(
            MAINNET_CONFIG.initial_supply().expect("mainnet"),
            359_999_999_999_990_210
        );
        assert_eq!(
            FUJI_CONFIG.initial_supply().expect("fuji"),
            360_000_000 * NANO_AVAX_PER_AVAX
        );
        assert_eq!(
            UNMODIFIED_LOCAL_CONFIG.initial_supply().expect("local"),
            360_000_000 * NANO_AVAX_PER_AVAX
        );
    }
}
