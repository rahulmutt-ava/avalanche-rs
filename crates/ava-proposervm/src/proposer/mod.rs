// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The proposer scheduling layer — the windower (M3.22).

pub mod windower;

pub use windower::{
    MAX_BUILD_WINDOWS, MAX_LOOK_AHEAD_SLOTS, MAX_VERIFY_WINDOWS, ValidatorData, WINDOW_DURATION,
    Windower, chain_source, delay_for, expected_proposer_from, min_delay_for_proposer_from,
    proposers_from, time_to_slot,
};
