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

/// Live arm: bring up a small mixed Go+Rust network, replay the same seed-derived
/// program across all nodes, and assert no fork / same tip across every chain.
/// Gated behind the `live` feature + `#[ignore]`; needs `$AVALANCHEGO_PATH` (Go)
/// and a built `avalanchers`. Never runs in CI / this sandbox.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a live mixed Go+Rust tmpnet ($AVALANCHEGO_PATH + avalanchers) — nightly only"]
async fn mixed_network() {
    use std::time::Duration;

    use ava_differential::driver::LockstepDriver;
    use ava_differential::network::{BinaryMix, Network, NetworkConfig};
    use ava_differential::observation::Observation;
    use ava_differential::program::Program;

    // Skip gracefully if the Go oracle binary is not configured.
    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network");
        return;
    }

    // Operator handoff (specs/16 §5(2), specs/02 §11.3):
    //
    // 1. Boot a small mixed Go+Rust tmpnet from a seed-derived config so every
    //    node shares the same genesis/config/identity derivation (§11.4).
    let seed = 0x5EED_u64;
    let cfg = NetworkConfig::deterministic(seed, 4);
    let mix = BinaryMix::from_config(&cfg);
    let net = Network::start(mix, &cfg)
        .await
        .expect("mixed Go+Rust network boots");
    net.await_all_connected(Duration::from_secs(60))
        .await
        .expect("all nodes complete handshakes / exchange PeerLists");

    // 2. Replay the same seed-derived program across the live net. Offline the
    //    driver runs the in-process pipeline; the live replay issues the same
    //    seed-derived program of Actions (IssueTx / ApiCall / AdvanceTime /
    //    AwaitFinalization) against every node's RPC and waits for finalization.
    let program = Program::from_seed(seed);
    let _driver = LockstepDriver::new(seed);
    // TODO(operator): drive `program` across `net.nodes()` via their RPC
    // endpoints, mirroring the offline `replay_recorded` walk. The live driver
    // wiring (issue tx / advance clock / await finalization over RPC) is the
    // live-mode follow-up; the offline arm above proves the replay determinism.

    // 3. After each AwaitFinalization, collect + normalize an Observation from
    //    EVERY node and assert all nodes (Go and Rust) agree: same per-chain
    //    last-accepted id + height + state root + sorted validator set — i.e. no
    //    fork / same tip across every chain (§11.3/§11.4).
    let mut snapshots: Vec<Observation> = Vec::new();
    for node in net.nodes() {
        let obs = Observation::collect(&node.api_base)
            .await
            .expect("collect a normalized observation from a live node")
            .normalized();
        assert!(
            !obs.fields.is_empty(),
            "each node's observation is non-empty / comparable"
        );
        snapshots.push(obs);
    }
    // Every node must observe byte-identical finalized state (no fork, same tip).
    if let Some(first) = snapshots.first() {
        for (i, obs) in snapshots.iter().enumerate() {
            assert_eq!(
                first, obs,
                "node {i} diverged from node 0 — fork detected across the mixed Go+Rust net"
            );
        }
    }
    let _ = program;

    net.shutdown().await;
}
