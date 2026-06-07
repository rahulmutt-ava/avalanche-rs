// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Unit tests for Simplex [`Parameters`] (port of `parameters_test.go`-style
//! checks against `Verify` and `DefaultParameters`).

#![allow(unused_crate_dependencies, clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use assert_matches::assert_matches;

use ava_simplex::Error;
use ava_simplex::parameters::{
    DEFAULT_MAX_NETWORK_DELAY, DEFAULT_MAX_REBROADCAST_WAIT, Parameters, ValidatorInfo,
};
use ava_types::node_id::NodeId;

fn one_validator() -> Vec<ValidatorInfo> {
    vec![ValidatorInfo {
        node_id: NodeId::from([1u8; 20]),
        public_key: vec![0u8; 48],
    }]
}

/// `parameters_defaults` — `DefaultParameters` matches Go's 5s/5s values and an
/// empty initial validator set; verify fails until validators are supplied.
#[test]
fn parameters_defaults() {
    let p = Parameters::default();
    assert_eq!(p.max_network_delay, Duration::from_secs(5));
    assert_eq!(p.max_rebroadcast_wait, Duration::from_secs(5));
    assert_eq!(p.max_network_delay, DEFAULT_MAX_NETWORK_DELAY);
    assert_eq!(p.max_rebroadcast_wait, DEFAULT_MAX_REBROADCAST_WAIT);
    assert!(p.initial_validators.is_empty());

    // Default params have no validators => Verify fails on that branch.
    assert_matches!(p.verify(), Err(Error::InvalidParameters(_)));
}

/// `parameters_verify` — each invalid branch in Go's exact order.
#[test]
fn parameters_verify_branches() {
    // zero network delay.
    let p = Parameters {
        max_network_delay: Duration::ZERO,
        max_rebroadcast_wait: Duration::from_secs(5),
        initial_validators: one_validator(),
    };
    assert_matches!(p.verify(), Err(Error::InvalidParameters(m)) if m.contains("maxNetworkDelay"));

    // zero rebroadcast wait.
    let p = Parameters {
        max_network_delay: Duration::from_secs(5),
        max_rebroadcast_wait: Duration::ZERO,
        initial_validators: one_validator(),
    };
    assert_matches!(p.verify(), Err(Error::InvalidParameters(m)) if m.contains("maxRebroadcastWait"));

    // empty validators.
    let p = Parameters {
        max_network_delay: Duration::from_secs(5),
        max_rebroadcast_wait: Duration::from_secs(5),
        initial_validators: Vec::new(),
    };
    assert_matches!(p.verify(), Err(Error::InvalidParameters(m)) if m.contains("initialValidators"));

    // valid.
    let p = Parameters {
        max_network_delay: Duration::from_secs(5),
        max_rebroadcast_wait: Duration::from_secs(5),
        initial_validators: one_validator(),
    };
    assert_matches!(p.verify(), Ok(()));
}
