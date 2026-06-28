// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 — `differential::mixed_network` (specs/16 §5(2), specs/02 §11.3,
//! specs/00 §6.1).
//!
//! Two-arm shape (mirrors `mixed_network_smoke.rs` + the `xchain_issue_tx.rs`
//! determinism gate):
//!
//! 1. **Offline arm** (runs every CI run, no feature, not `#[ignore]`):
//!    * `mixed_network_replay_is_deterministic` — a [`Program`] generated
//!      deterministically from a seed, replayed TWICE through
//!      [`LockstepDriver::replay_recorded`] against the REAL in-process Rust
//!      `ava-avm` pipeline, yields byte-identical normalized [`Observation`]
//!      sequences (the determinism / total-order property, specs/00 §6.1), and
//!      at least one `AwaitFinalization` produced a non-empty observation (the
//!      pipeline really ran). A deliberately perturbed observation is shown to
//!      break the comparison, so the equality check genuinely catches a fork.
//!    * `mixed_network_replay_is_deterministic_proptest` — the same property
//!      over a spread of seeds.
//!
//!    Pure Rust, no node spawn, no Go oracle (there is none offline).
//!
//! 2. **Live arm** (`#[cfg(feature = "live")]` + `#[ignore]`):
//!    `mixed_network` — boot a small mixed Go+Rust tmpnet via [`Network::start`],
//!    replay the same seed-derived program across all nodes, collect+normalize an
//!    [`Observation`] per node per finalization, and assert no fork / same tip
//!    across every chain. Needs `$AVALANCHEGO_PATH` (Go) + a built `avalanchers`;
//!    returns early if unset. Never runs in CI / this sandbox — a
//!    scheduled/nightly job runs it via
//!    `cargo nextest run -p ava-differential --features live -- --ignored`.

// This target consumes only `ava-differential` (+ `proptest`); the other crate
// deps are used by the lib / other integration targets. Per the established
// `unused_crate_dependencies` idiom, each integration-test file that does not
// consume them opts out of the per-binary false positive.
#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod differential {
    use ava_differential::driver::LockstepDriver;
    use ava_differential::program::Program;
    use proptest::prelude::*;

    /// Offline: a seed-derived program replayed twice against the real in-process
    /// Rust pipeline produces byte-identical normalized observation sequences.
    #[test]
    fn mixed_network_replay_is_deterministic() {
        let seed = 0xA11CE_u64;
        let program = Program::from_seed(seed);
        let driver = LockstepDriver::new(seed);

        let run_a = driver
            .replay_recorded(&program)
            .expect("replay_recorded run A");
        let run_b = driver
            .replay_recorded(&program)
            .expect("replay_recorded run B");

        // The KEY property: same seed + actions ⇒ byte-identical observation
        // sequence (specs/00 §6.1 total order / determinism).
        assert_eq!(
            run_a, run_b,
            "DIFFERENTIAL_SEED={seed} — replaying the same program twice must \
             produce identical normalized observation sequences"
        );

        // The pipeline really ran: at least one AwaitFinalization produced a
        // non-empty observation with an accepted block (height >= 1).
        assert!(
            !run_a.is_empty(),
            "DIFFERENTIAL_SEED={seed} — expected at least one finalization observation"
        );
        let any_non_empty = run_a.iter().any(|obs| {
            obs.fields
                .iter()
                .any(|(k, _)| k == "xchain.last_accepted.height")
                && obs
                    .fields
                    .iter()
                    .find(|(k, _)| k == "xchain.last_accepted.height")
                    .and_then(|(_, v)| v.parse::<u64>().ok())
                    .is_some_and(|h| h >= 1)
        });
        assert!(
            any_non_empty,
            "DIFFERENTIAL_SEED={seed} — expected at least one finalization with an accepted block"
        );

        // Sanity-check the equality assertion genuinely catches a fork: inject a
        // deliberate divergence into a copy of run B and confirm it no longer
        // matches run A.
        let mut forked = run_b.clone();
        let first = forked.first_mut().expect("at least one observation");
        first.set_field("xchain.last_accepted.id", "deadbeef");
        let forked_norm: Vec<_> = forked.iter().map(|o| o.normalized()).collect();
        let a_norm: Vec<_> = run_a.iter().map(|o| o.normalized()).collect();
        assert_ne!(
            a_norm, forked_norm,
            "an injected last-accepted divergence must break the sequence comparison"
        );
    }

    proptest! {
        // A spread of seeds, kept fast (each case drives the real VM pipeline a
        // few times). 64 cases run well under the leashed differential-package
        // nextest timeout.
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn mixed_network_replay_is_deterministic_proptest(seed in any::<u64>()) {
            let program = Program::from_seed(seed);
            let driver = LockstepDriver::new(seed);

            let a = driver
                .replay_recorded(&program)
                .expect("replay_recorded run A");
            let b = driver
                .replay_recorded(&program)
                .expect("replay_recorded run B");

            prop_assert_eq!(
                &a, &b,
                "DIFFERENTIAL_SEED={} — nondeterministic mixed-network replay observation sequence",
                seed
            );
            prop_assert!(
                !a.is_empty(),
                "DIFFERENTIAL_SEED={seed} — expected at least one finalization observation"
            );
        }
    }
}

/// Live arm: bring up a small mixed Go+Rust network, drive a C-chain transfer,
/// settle both nodes to the same height, and assert no fork / same tip.
/// Gated behind the `live` feature + `#[ignore]`; needs `$AVALANCHEGO_PATH` (Go)
/// and a built `avalanchers`. Never runs in CI / this sandbox.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a live mixed Go+Rust net ($AVALANCHEGO_PATH + avalanchers) — nightly only"]
async fn mixed_network() {
    use std::time::Duration;

    use ava_differential::livenet::{await_same_c_height, drive_c_transfer};
    use ava_differential::network::Network;
    use ava_differential::observation::Observation;

    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network");
        return;
    }

    // 1. Boot Go beacon + Rust follower; waits for the Rust node to bootstrap P/X/C from Go.
    let net = Network::boot_mixed(0x5EED)
        .await
        .expect("mixed Go+Rust net boots + bootstraps");
    net.await_all_connected(Duration::from_secs(60))
        .await
        .expect("Go and Rust complete the TLS handshake / exchange PeerLists");

    let go_api = net.go_beacon().expect("go beacon").api_base.clone();
    let rust_api = net.rust_follower().expect("rust follower").api_base.clone();

    // 2. Record the pre-tx C height, drive one tx on the Go validator, settle.
    let before = await_same_c_height(&go_api, &rust_api, 0, Duration::from_secs(30))
        .await
        .expect("nodes agree on a starting C height");
    drive_c_transfer(&go_api)
        .await
        .expect("issue + mine one C-chain tx on the Go validator");
    let after = await_same_c_height(
        &go_api,
        &rust_api,
        before.saturating_add(1),
        Duration::from_secs(60),
    )
    .await
    .expect("both nodes advance to the same C height after the tx");
    assert!(
        after > before,
        "tx must advance the C-chain tip: {before} -> {after}"
    );

    // 3. No fork / same tip: full normalized observation must match across impls.
    let go_obs = Observation::collect(&go_api)
        .await
        .expect("collect Go observation")
        .normalized();
    let rust_obs = Observation::collect(&rust_api)
        .await
        .expect("collect Rust observation")
        .normalized();
    assert_eq!(
        go_obs, rust_obs,
        "Go and Rust diverged — fork across the mixed net"
    );

    net.shutdown().await;
}

/// Bisection probe for M9.15 rung-3 (single Go beacon → `required_conns = 1`).
/// If the follower bootstraps here, the engine/gate/frontier path works against
/// real Go and the 5-validator failure is purely connectivity (forming 4/5).
/// Gated behind `live` + `#[ignore]`; needs `$AVALANCHEGO_PATH` + a built `avalanchers`.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a single-Go-beacon + Rust follower net — nightly only"]
async fn mixed_network_single_beacon() {
    use ava_differential::network::Network;

    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network_single_beacon");
        return;
    }
    let net = Network::boot_single_go_beacon(0x515E)
        .await
        .expect("single-Go-beacon net boots + follower bootstraps P/X/C");
    assert!(net.go_beacon().is_some(), "go beacon present");
    assert!(net.rust_follower().is_some(), "rust follower present");
}
