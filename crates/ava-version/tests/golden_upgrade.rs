// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden test for M0.23: `UpgradeConfig` + `Fork` activation schedule.
//! Mirrors `upgrade/upgrade_test.go` — verifies the `t >= fork_time` rule
//! and the verbatim Mainnet/Fuji fork constants against Go-extracted vectors.

use chrono::{DateTime, TimeZone};
use serde::Deserialize;

use ava_version::upgrade::{Fork, get_config};

// ── Vector types ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Sample {
    at_rfc3339_nano: String,
    is_active: bool,
}

#[derive(Deserialize)]
struct Case {
    network: String,
    fork: String,
    fork_time_rfc3339_nano: String,
    samples: Vec<Sample>,
}

#[derive(Deserialize)]
struct VectorFile {
    cases: Vec<Case>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_network_id(name: &str) -> u32 {
    match name {
        "mainnet" => ava_types::constants::MAINNET_ID,
        "fuji" => ava_types::constants::FUJI_ID,
        other => panic!("unknown network in test vector: {other}"),
    }
}

fn parse_fork(name: &str) -> Fork {
    match name {
        "apricot_phase_1" => Fork::ApricotPhase1,
        "apricot_phase_2" => Fork::ApricotPhase2,
        "apricot_phase_3" => Fork::ApricotPhase3,
        "apricot_phase_4" => Fork::ApricotPhase4,
        "apricot_phase_5" => Fork::ApricotPhase5,
        "apricot_phase_pre_6" => Fork::ApricotPhasePre6,
        "apricot_phase_6" => Fork::ApricotPhase6,
        "apricot_phase_post_6" => Fork::ApricotPhasePost6,
        "banff" => Fork::Banff,
        "cortina" => Fork::Cortina,
        "durango" => Fork::Durango,
        "etna" => Fork::Etna,
        "fortuna" => Fork::Fortuna,
        "granite" => Fork::Granite,
        "helicon" => Fork::Helicon,
        other => panic!("unknown fork in test vector: {other}"),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn upgrade_activation() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/upgrade/activation.json"
    ))
    .expect("activation.json missing");

    let vectors: VectorFile = serde_json::from_str(&raw).expect("invalid JSON");

    for case in &vectors.cases {
        let network_id = parse_network_id(&case.network);
        let fork = parse_fork(&case.fork);
        let config = get_config(network_id);

        let fork_time = config.fork_time(fork);
        let expected_fork_time: DateTime<chrono::Utc> = case
            .fork_time_rfc3339_nano
            .parse()
            .unwrap_or_else(|e| panic!("bad fork_time in vector: {e}"));

        assert_eq!(
            fork_time, expected_fork_time,
            "fork_time mismatch for network={} fork={:?}",
            case.network, fork
        );

        for sample in &case.samples {
            let t: DateTime<chrono::Utc> = sample
                .at_rfc3339_nano
                .parse()
                .unwrap_or_else(|e| panic!("bad sample time: {e}"));

            let got = config.is_active(fork, t);
            assert_eq!(
                got, sample.is_active,
                "is_active mismatch: network={} fork={:?} at={} expected={} got={}",
                case.network, fork, sample.at_rfc3339_nano, sample.is_active, got
            );
        }
    }
}

#[test]
fn validate_accepts_shipped_configs() {
    // Both mainnet and fuji configs must pass validate().
    get_config(ava_types::constants::MAINNET_ID)
        .validate()
        .expect("mainnet config should be valid");
    get_config(ava_types::constants::FUJI_ID)
        .validate()
        .expect("fuji config should be valid");
    // Default config should also be valid (all same time).
    get_config(ava_types::constants::LOCAL_ID)
        .validate()
        .expect("default config should be valid");
}

#[test]
fn validate_rejects_out_of_order_config() {
    // Build a mainnet config and swap two fork times to make it invalid.
    let mut cfg = get_config(ava_types::constants::MAINNET_ID);
    // Swap ApricotPhase1 and ApricotPhase2 times (phase1 must be <= phase2).
    let t1 = cfg.apricot_phase_1_time;
    let t2 = cfg.apricot_phase_2_time;
    cfg.apricot_phase_1_time = t2;
    cfg.apricot_phase_2_time = t1;
    // This makes phase1 > phase2, violating monotonicity.
    cfg.validate()
        .expect_err("out-of-order config should fail validate");
}

#[test]
fn fork_at_returns_correct_fork() {
    let config = get_config(ava_types::constants::MAINNET_ID);

    // Before ApricotPhase1 → None
    let before_genesis = chrono::Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    assert_eq!(config.fork_at(before_genesis), None);

    // At ApricotPhase1 time → ApricotPhase1
    let at_p1 = config.fork_time(Fork::ApricotPhase1);
    assert_eq!(config.fork_at(at_p1), Some(Fork::ApricotPhase1));

    // After everything → Helicon
    let far_future = chrono::Utc.with_ymd_and_hms(9999, 12, 2, 0, 0, 0).unwrap();
    assert_eq!(config.fork_at(far_future), Some(Fork::Helicon));
}

#[test]
fn fork_ordering_is_chronological() {
    // Fork::ALL must be in chronological order by the mainnet times.
    let config = get_config(ava_types::constants::MAINNET_ID);
    let times: Vec<_> = Fork::ALL.iter().map(|&f| config.fork_time(f)).collect();
    for w in times.windows(2) {
        assert!(
            w[0] <= w[1],
            "Fork::ALL order violated: {:?} > {:?}",
            w[0],
            w[1]
        );
    }
}

#[test]
fn is_active_thin_forwarders() {
    let config = get_config(ava_types::constants::MAINNET_ID);
    let t = config.fork_time(Fork::Durango);
    // The thin forwarder should agree with is_active.
    assert_eq!(
        config.is_durango_activated(t),
        config.is_active(Fork::Durango, t)
    );
}
