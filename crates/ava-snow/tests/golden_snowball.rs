// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::snowball_unit_vectors` — byte/behaviour-exact ports of the Go
//! `snow/consensus/snowball/*_test.go` corpus (record-poll sequences →
//! preference / finalization / confidence transitions). Provenance: pinned
//! `avalanchego` tree, `snow/consensus/snowball/{parameters,binary_snowflake,
//! binary_snowball,unary_snowflake,nnary_snowflake}_test.go`. These exercise
//! only the public API; the helper-suite confidence-vector sub-tests that need
//! private state are covered via the public `confidence()` accessors where Go
//! asserts them.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::time::Duration;

use assert_matches::assert_matches;
use ava_snow::error::Error;
use ava_snow::snowball::{
    BinarySnowball, BinarySnowflake, DEFAULT_PARAMETERS, NnarySnowflake, Parameters,
    TerminationCondition, UnarySnowflake,
};
use ava_types::id::Id;

const VALID: Parameters = Parameters {
    k: 1,
    alpha_preference: 1,
    alpha_confidence: 1,
    beta: 1,
    concurrent_repolls: 1,
    optimal_processing: 1,
    max_outstanding_items: 1,
    max_item_processing_time: Duration::from_nanos(1),
};

/// Port of Go `TestParametersVerify` — the full 16-case table, including the
/// "fun alphaConfidence" cases that probe the easter-egg branch ordering.
#[test]
fn golden_parameters_verify() {
    // (name, mutate-from-VALID, expect_ok)
    let cases: &[(&str, Parameters, bool)] = &[
        ("valid", VALID, true),
        ("invalid K", Parameters { k: 0, ..VALID }, false),
        (
            "invalid AlphaPreference 1",
            Parameters {
                k: 2,
                alpha_preference: 1,
                alpha_confidence: 1,
                ..VALID
            },
            false,
        ),
        (
            "invalid AlphaPreference 0",
            Parameters {
                alpha_preference: 0,
                ..VALID
            },
            false,
        ),
        (
            "invalid AlphaConfidence",
            Parameters {
                k: 3,
                alpha_preference: 3,
                alpha_confidence: 2,
                ..VALID
            },
            false,
        ),
        ("invalid beta", Parameters { beta: 0, ..VALID }, false),
        (
            "first half fun alphaConfidence",
            Parameters {
                k: 30,
                alpha_preference: 28,
                alpha_confidence: 30,
                beta: 2,
                ..VALID
            },
            true,
        ),
        (
            "second half fun alphaConfidence",
            Parameters {
                k: 3,
                alpha_preference: 2,
                alpha_confidence: 3,
                beta: 2,
                ..VALID
            },
            true,
        ),
        (
            "fun invalid alphaConfidence",
            Parameters {
                k: 1,
                alpha_preference: 28,
                alpha_confidence: 3,
                beta: 2,
                ..VALID
            },
            false,
        ),
        (
            "too few ConcurrentRepolls",
            Parameters {
                concurrent_repolls: 0,
                ..VALID
            },
            false,
        ),
        (
            "too many ConcurrentRepolls",
            Parameters {
                beta: 1,
                concurrent_repolls: 2,
                ..VALID
            },
            false,
        ),
        (
            "invalid OptimalProcessing",
            Parameters {
                optimal_processing: 0,
                ..VALID
            },
            false,
        ),
        (
            "invalid MaxOutstandingItems",
            Parameters {
                max_outstanding_items: 0,
                ..VALID
            },
            false,
        ),
        (
            "invalid MaxItemProcessingTime",
            Parameters {
                max_item_processing_time: Duration::ZERO,
                ..VALID
            },
            false,
        ),
    ];

    for (name, params, expect_ok) in cases {
        let got = params.verify();
        if *expect_ok {
            assert_matches!(got, Ok(()), "case {name:?} should be valid");
        } else {
            assert_matches!(
                got,
                Err(Error::ParametersInvalid(_)),
                "case {name:?} should be ParametersInvalid"
            );
        }
    }

    // DEFAULT_PARAMETERS must itself be valid (Go DefaultParameters).
    assert_matches!(DEFAULT_PARAMETERS.verify(), Ok(()));
}

/// Port of Go `TestBinarySnowflake`.
#[test]
fn golden_binary_snowflake() {
    let (blue, red) = (0u8, 1u8);
    let (alpha_preference, alpha_confidence, beta) = (1u32, 2u32, 2u32);
    let conditions = TerminationCondition::single(alpha_confidence, beta);

    let mut sf = BinarySnowflake::new(alpha_preference, conditions, red);
    assert_eq!(sf.preference(), red);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence, blue);
    assert_eq!(sf.preference(), blue);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence, red);
    assert_eq!(sf.preference(), red);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence, blue);
    assert_eq!(sf.preference(), blue);
    assert!(!sf.finalized());

    // alpha_preference (< alpha_confidence): changes preference, no confidence.
    sf.record_poll(alpha_preference, red);
    assert_eq!(sf.preference(), red);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence, blue);
    assert_eq!(sf.preference(), blue);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence, blue);
    assert_eq!(sf.preference(), blue);
    assert!(sf.finalized());
}

/// Port of Go `TestBinarySnowball`.
#[test]
fn golden_binary_snowball() {
    let (red, blue) = (0u8, 1u8);
    let (alpha_preference, alpha_confidence, beta) = (2u32, 3u32, 2u32);
    let conditions = TerminationCondition::single(alpha_confidence, beta);

    let mut sb = BinarySnowball::new(alpha_preference, conditions, red);
    assert_eq!(sb.preference(), red);
    assert!(!sb.finalized());

    sb.record_poll(alpha_confidence, blue);
    assert_eq!(sb.preference(), blue);
    assert!(!sb.finalized());

    sb.record_poll(alpha_confidence, red);
    assert_eq!(sb.preference(), blue);
    assert!(!sb.finalized());

    sb.record_poll(alpha_confidence, blue);
    assert_eq!(sb.preference(), blue);
    assert!(!sb.finalized());

    sb.record_poll(alpha_confidence, blue);
    assert_eq!(sb.preference(), blue);
    assert!(sb.finalized());
}

/// Port of Go `TestBinarySnowballRecordPollPreference` (preference flips back to
/// the popularity-leading choice, then finalizes there; asserts the final
/// preference-strength split `[4, 1]`).
#[test]
fn golden_binary_snowball_record_poll_preference() {
    let (red, blue) = (0u8, 1u8);
    let (alpha_preference, alpha_confidence, beta) = (1u32, 2u32, 2u32);
    let conditions = TerminationCondition::single(alpha_confidence, beta);

    let mut sb = BinarySnowball::new(alpha_preference, conditions, red);
    assert_eq!(sb.preference(), red);

    sb.record_poll(alpha_confidence, blue);
    assert_eq!(sb.preference(), blue);
    assert!(!sb.finalized());

    sb.record_poll(alpha_confidence, red);
    assert_eq!(sb.preference(), blue);
    assert!(!sb.finalized());

    sb.record_poll(alpha_preference, red);
    assert_eq!(sb.preference(), red);
    assert!(!sb.finalized());

    sb.record_poll(alpha_confidence, red);
    assert_eq!(sb.preference(), red);
    assert!(!sb.finalized());

    sb.record_poll(alpha_confidence, red);
    assert_eq!(sb.preference(), red);
    assert!(sb.finalized());

    // Go String() asserts PreferenceStrength[0]=4, [1]=1.
    assert_eq!(sb.preference_strength(), [4, 1]);
}

/// Port of Go `TestUnarySnowflake` (the unary record-poll / unsuccessful-poll
/// confidence transitions, asserting the `confidence` vector like Go's
/// `UnarySnowflakeStateTest`).
#[test]
fn golden_unary_snowflake() {
    let (alpha_preference, alpha_confidence, beta) = (1u32, 2u32, 2u32);
    let conditions = TerminationCondition::single(alpha_confidence, beta);

    let mut sf = UnarySnowflake::new(alpha_preference, conditions);

    sf.record_poll(alpha_confidence);
    assert_eq!(sf.confidence(), &[1]);
    assert!(!sf.finalized());

    sf.record_unsuccessful_poll();
    assert_eq!(sf.confidence(), &[0]);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence);
    assert_eq!(sf.confidence(), &[1]);
    assert!(!sf.finalized());

    // Extend into a binary instance rooted at choice 0 (Go Clone+Extend).
    let mut bsf = sf.extend(0);
    bsf.record_unsuccessful_poll();
    bsf.record_poll(alpha_confidence, 1);
    assert!(!bsf.finalized());
    bsf.record_poll(alpha_confidence, 1);
    assert_eq!(bsf.preference(), 1);
    assert!(bsf.finalized());

    // The original unary instance is unaffected by the extension, and finalizes.
    sf.record_poll(alpha_confidence);
    assert_eq!(sf.confidence(), &[2]);
    assert!(sf.finalized());

    sf.record_unsuccessful_poll();
    assert_eq!(sf.confidence(), &[0]);
    assert!(sf.finalized());

    sf.record_poll(alpha_confidence);
    assert_eq!(sf.confidence(), &[1]);
    assert!(sf.finalized());
}

/// Port of Go `TestNnarySnowflake`.
#[test]
fn golden_nnary_snowflake() {
    let red = Id::from([0x01; 32]);
    let blue = Id::from([0x02; 32]);
    let green = Id::from([0x03; 32]);
    let (alpha_preference, alpha_confidence, beta) = (1u32, 2u32, 2u32);
    let conditions = TerminationCondition::single(alpha_confidence, beta);

    let mut sf = NnarySnowflake::new(alpha_preference, conditions, red);
    sf.add(blue);
    sf.add(green);

    assert_eq!(sf.preference(), red);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence, blue);
    assert_eq!(sf.preference(), blue);
    assert!(!sf.finalized());

    // alpha_preference only: flips preference, resets confidence.
    sf.record_poll(alpha_preference, red);
    assert_eq!(sf.preference(), red);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence, red);
    assert_eq!(sf.preference(), red);
    assert!(!sf.finalized());

    sf.record_poll(alpha_confidence, red);
    assert_eq!(sf.preference(), red);
    assert!(sf.finalized());

    // Polls after finalization are no-ops.
    sf.record_poll(alpha_preference, blue);
    assert_eq!(sf.preference(), red);
    assert!(sf.finalized());

    sf.record_poll(alpha_confidence, blue);
    assert_eq!(sf.preference(), red);
    assert!(sf.finalized());
}
