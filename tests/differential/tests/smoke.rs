// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Skeleton differential smoke test (tier-X task X.13).
//!
//! The real `differential_recorded_oracle_agrees` proptest (replay an
//! `arb_program()` against the Rust impl vs the Go-recorded oracle) is filled in
//! by X.13. Until then this exercises the harness skeleton API so the
//! `ava-differential` crate stays buildable-&-green and the CI `differential`
//! job has a target. Named `differential_*` so the nextest CI override leashes it.

// The networking dev-deps (ava-network/message/crypto/types/version, tokio*) are
// used only by the `interop_handshake` integration target (M2.22); per the
// established `unused_crate_dependencies` idiom, each integration-test file that
// does not consume them opts out of the per-binary false positive.
#![allow(unused_crate_dependencies)]

use arbitrary::{Arbitrary, Unstructured};
use ava_differential::observation::Observation;
use ava_differential::{Action, Binary, LockstepDriver, NetworkConfig, Program};

#[test]
fn differential_skeleton_api_is_wired() {
    let driver = LockstepDriver::new(42);
    assert_eq!(driver.seed(), 42);

    let cfg = NetworkConfig::deterministic(7, 5);
    assert_eq!(cfg.seed, 7);
    assert_eq!(cfg.nodes, 5);
    assert_ne!(Binary::Go, Binary::Rust);
}

#[test]
fn differential_observation_normalizes_deterministically() {
    let obs = Observation {
        fields: vec![
            ("height".to_owned(), "10".to_owned()),
            ("block_id".to_owned(), "abc".to_owned()),
        ],
    };
    // Normalization sorts collections so two correct impls compare equal.
    let a = obs.normalized();
    let b = obs.normalized();
    assert_eq!(a, b);
    assert_eq!(a.fields.first().map(|(k, _)| k.as_str()), Some("block_id"));
}

#[test]
fn differential_program_is_arbitrary_derivable() {
    // The `arbitrary` derive on Action/Program is what `arb_program()` will build
    // on in X.13; smoke-check that it decodes from a deterministic byte source.
    let bytes = [0xA5u8; 64];
    let mut u = Unstructured::new(&bytes);
    let program = Program::arbitrary(&mut u).expect("decode Program from bytes");
    // Every action decodes to a known variant.
    for action in &program.actions {
        assert!(matches!(
            action,
            Action::IssueTx
                | Action::ApiCall
                | Action::AdvanceTime
                | Action::RestartNode
                | Action::Partition
                | Action::AwaitFinalization
        ));
    }
}
