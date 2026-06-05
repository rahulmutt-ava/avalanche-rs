// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Network ids + human-readable prefixes (HRPs).
//!
//! Ported from Go `utils/constants/network_ids.go`. Owning spec:
//! `specs/03-core-primitives.md` §1.4.

use crate::id::Id;

// ---- Network IDs (`utils/constants/network_ids.go`) ----

/// The Mainnet network id.
pub const MAINNET_ID: u32 = 1;
/// The Cascade (historical) network id.
pub const CASCADE_ID: u32 = 2;
/// The Denali (historical) network id.
pub const DENALI_ID: u32 = 3;
/// The Everest (historical) network id.
pub const EVEREST_ID: u32 = 4;
/// The Fuji (testnet) network id.
pub const FUJI_ID: u32 = 5;
/// Alias of [`FUJI_ID`].
pub const TESTNET_ID: u32 = FUJI_ID;
/// The unit-test network id.
pub const UNIT_TEST_ID: u32 = 10;
/// The local network id.
pub const LOCAL_ID: u32 = 12345;

// ---- Network names ----

/// Mainnet network name.
pub const MAINNET_NAME: &str = "mainnet";
/// Cascade network name.
pub const CASCADE_NAME: &str = "cascade";
/// Denali network name.
pub const DENALI_NAME: &str = "denali";
/// Everest network name.
pub const EVEREST_NAME: &str = "everest";
/// Fuji network name.
pub const FUJI_NAME: &str = "fuji";
/// Testnet network name (alias of Fuji).
pub const TESTNET_NAME: &str = "testnet";
/// Unit-test network name.
pub const UNIT_TEST_NAME: &str = "testing";
/// Local network name.
pub const LOCAL_NAME: &str = "local";

// ---- HRPs (bech32 human-readable parts) ----

/// Mainnet HRP.
pub const MAINNET_HRP: &str = "avax";
/// Cascade HRP.
pub const CASCADE_HRP: &str = "cascade";
/// Denali HRP.
pub const DENALI_HRP: &str = "denali";
/// Everest HRP.
pub const EVEREST_HRP: &str = "everest";
/// Fuji HRP.
pub const FUJI_HRP: &str = "fuji";
/// Unit-test HRP.
pub const UNIT_TEST_HRP: &str = "testing";
/// Local HRP.
pub const LOCAL_HRP: &str = "local";
/// Fallback HRP for unknown networks (`GetHRP` default).
pub const FALLBACK_HRP: &str = "custom";

/// The primary network id. Mirrors Go `constants.PrimaryNetworkID` (`ids.Empty`).
pub const PRIMARY_NETWORK_ID: Id = Id::EMPTY;

/// The prefix recognized by [`network_id`] for `network-<n>` style names.
/// Mirrors Go `constants.ValidNetworkPrefix`.
pub const VALID_NETWORK_PREFIX: &str = "network-";

/// `(network_id, hrp)` table. Mirrors Go `NetworkIDToHRP`.
const NETWORK_ID_TO_HRP: &[(u32, &str)] = &[
    (MAINNET_ID, MAINNET_HRP),
    (CASCADE_ID, CASCADE_HRP),
    (DENALI_ID, DENALI_HRP),
    (EVEREST_ID, EVEREST_HRP),
    (FUJI_ID, FUJI_HRP),
    (UNIT_TEST_ID, UNIT_TEST_HRP),
    (LOCAL_ID, LOCAL_HRP),
];

/// `(network_id, name)` table. Mirrors Go `NetworkIDToNetworkName`.
const NETWORK_ID_TO_NAME: &[(u32, &str)] = &[
    (MAINNET_ID, MAINNET_NAME),
    (CASCADE_ID, CASCADE_NAME),
    (DENALI_ID, DENALI_NAME),
    (EVEREST_ID, EVEREST_NAME),
    (FUJI_ID, FUJI_NAME),
    (UNIT_TEST_ID, UNIT_TEST_NAME),
    (LOCAL_ID, LOCAL_NAME),
];

/// `(name, network_id)` table. Mirrors Go `NetworkNameToNetworkID` (includes the
/// `testnet` alias of Fuji).
const NETWORK_NAME_TO_ID: &[(&str, u32)] = &[
    (MAINNET_NAME, MAINNET_ID),
    (CASCADE_NAME, CASCADE_ID),
    (DENALI_NAME, DENALI_ID),
    (EVEREST_NAME, EVEREST_ID),
    (FUJI_NAME, FUJI_ID),
    (TESTNET_NAME, TESTNET_ID),
    (UNIT_TEST_NAME, UNIT_TEST_ID),
    (LOCAL_NAME, LOCAL_ID),
];

/// Returns the bech32 HRP for `network_id`, or [`FALLBACK_HRP`] if unknown.
/// Mirrors Go `constants.GetHRP`.
#[must_use]
pub fn get_hrp(network_id: u32) -> &'static str {
    NETWORK_ID_TO_HRP
        .iter()
        .find_map(|&(id, hrp)| (id == network_id).then_some(hrp))
        .unwrap_or(FALLBACK_HRP)
}

/// Returns a human-readable name for `network_id`. Unknown networks return
/// `network-<id>`. Mirrors Go `constants.NetworkName`.
#[must_use]
pub fn network_name(network_id: u32) -> String {
    NETWORK_ID_TO_NAME
        .iter()
        .find_map(|&(id, name)| (id == network_id).then_some(name.to_string()))
        .unwrap_or_else(|| format!("{VALID_NETWORK_PREFIX}{network_id}"))
}

/// Returns the id for the named network, or `None` if the name is not a known
/// network name.
///
/// Note: this covers the named-map portion of Go `constants.NetworkID`. The Go
/// version additionally parses numeric / `network-<n>` strings; that fallback is
/// deferred (the node-config layer owns it) and is not needed by M0.
#[must_use]
pub fn network_id(network_name: impl AsRef<str>) -> Option<u32> {
    let lower = network_name.as_ref().to_lowercase();
    NETWORK_NAME_TO_ID
        .iter()
        .find_map(|&(name, id)| (name == lower).then_some(id))
}
