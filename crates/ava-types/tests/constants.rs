// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M0.8 — network ids, HRPs, and the bidirectional name/id maps.

use ava_types::constants::{
    get_hrp, network_id, network_name, FUJI_ID, LOCAL_ID, MAINNET_ID, PRIMARY_NETWORK_ID,
};
use ava_types::id::Id;

#[test]
fn network_hrp_and_ids() {
    assert_eq!(MAINNET_ID, 1);
    assert_eq!(FUJI_ID, 5);
    assert_eq!(LOCAL_ID, 12345);

    assert_eq!(get_hrp(MAINNET_ID), "avax");
    assert_eq!(get_hrp(FUJI_ID), "fuji");
    assert_eq!(get_hrp(LOCAL_ID), "local");
    // Unknown network falls back to "custom".
    assert_eq!(get_hrp(9999), "custom");

    assert_eq!(PRIMARY_NETWORK_ID, Id::EMPTY);

    assert_eq!(network_id(network_name(MAINNET_ID)), Some(MAINNET_ID));
    assert_eq!(network_id(network_name(FUJI_ID)), Some(FUJI_ID));
}

#[test]
fn network_names() {
    assert_eq!(network_name(MAINNET_ID), "mainnet");
    assert_eq!(network_name(FUJI_ID), "fuji");
    assert_eq!(network_name(LOCAL_ID), "local");
    // Unknown networks get a synthetic name.
    assert_eq!(network_name(9999), "network-9999");
}

#[test]
fn historical_hrps() {
    assert_eq!(get_hrp(2), "cascade");
    assert_eq!(get_hrp(3), "denali");
    assert_eq!(get_hrp(4), "everest");
    assert_eq!(get_hrp(10), "testing");
}

#[test]
fn network_id_unknown_name() {
    assert_eq!(network_id("not-a-network"), None);
}
