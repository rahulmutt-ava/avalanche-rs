// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Tests for the feature-gated Simplex engine stub (`#[cfg(feature =
//! "simplex")]`). These only compile when the `simplex` feature is enabled.

// The `allow` must precede the `cfg` gate: when the `simplex` feature is off the
// crate body is stripped, leaving an empty crate that still links the dev-deps —
// keeping the allow active suppresses the `unused_crate_dependencies` lint.
#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]
#![cfg(feature = "simplex")]

use std::time::Duration;

use ava_engine::Engine;
use ava_engine::common::handler::{Handler, SimplexHandler};
use ava_simplex::SimplexEngine;
use ava_simplex::parameters::{Parameters, ValidatorInfo};
use ava_types::node_id::NodeId;

fn valid_params() -> Parameters {
    Parameters {
        max_network_delay: Duration::from_secs(5),
        max_rebroadcast_wait: Duration::from_secs(5),
        initial_validators: vec![ValidatorInfo {
            node_id: NodeId::from([1u8; 20]),
            public_key: vec![0u8; 48],
        }],
    }
}

/// The stub presents the object-safe `Handler` surface and constructs only from
/// verified parameters.
#[test]
fn stub_is_object_safe_handler() {
    fn _assert(_: &dyn Handler) {}
    let engine = SimplexEngine::new(valid_params()).expect("valid params");
    _assert(&engine);
}

/// Construction rejects invalid parameters (empty validator set).
#[test]
fn stub_rejects_invalid_params() {
    let bad = Parameters::default(); // no validators
    assert!(SimplexEngine::new(bad).is_err());
}

/// The simplex op and lifecycle methods drop cleanly (log-and-drop stub).
#[tokio::test]
async fn stub_drops_simplex_message_and_starts() {
    let mut engine = SimplexEngine::new(valid_params()).expect("valid params");
    engine.start(1).await.expect("start");
    engine
        .simplex(NodeId::from([9u8; 20]), &[0xde, 0xad])
        .await
        .expect("simplex op drops");
    let health = engine.health_check().expect("health");
    assert_eq!(health["engine"], "simplex-stub");
    assert_eq!(health["validators"], 1);
}
