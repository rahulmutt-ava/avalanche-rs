// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `differential::determinism_repeat` — the headline determinism repeat-N
//! proptest (spec 24 §B.6, plan/X-cross-cutting.md task X.19 follow-up #2).
//!
//! ## What this gate proves
//!
//! For a generated workload (a seed-derived [`Program`] of messages/txs/blocks)
//! replayed against the REAL in-process Rust `ava-avm` pipeline via
//! [`LockstepDriver::replay_recorded`], running the subsystem **N ≥ 16 times**
//! (reconstructing a fresh driver each time, so no shared mutable state carries
//! determinism for free) yields **byte-identical** normalized [`Observation`]
//! sequences across every repeat:
//!
//! ```text
//! forall seed, workload, clock_script:
//!     run(seed, workload, clock) == run(seed, workload, clock)   (× N)
//! ```
//!
//! The encoded blocks, computed hashes, and persisted state root all fold into
//! each finalization's [`Observation`] (`xchain.last_accepted.{id,height}` + the
//! full sorted UTXO set), so equality across N runs is byte/value-identity of the
//! whole pipeline output. This is the determinism / total-order property
//! (specs/00 §6.1) — NOT oracle agreement, which the Go recorded-oracle arms
//! cover separately. The point here is run-to-run reproducibility of the Rust
//! path, so it guards against any future nondeterminism (a `HashMap` iteration
//! leak, an unpinned clock read, an RNG reseed) regressing in.
//!
//! ## Clock script
//!
//! `LockstepDriver` does not (yet) take an injectable `ava_utils::clock::Clock`;
//! the X-Chain `build_block` wall-clock read is pinned deterministically by the
//! harness's far-future genesis timestamp (see `xchain.rs` `GENESIS_TS` — every
//! built block inherits the fixed `parent_time`, so block ids are reproducible
//! without a `MockClock`). Threading a clock seam through the driver is the
//! separate X.19 follow-up; this gate therefore drives determinism via the seed
//! alone, and the "fixed clock script" of the spec is realized as that fixed
//! genesis-derived timestamp. When `replay_recorded` adopts a `Clock`, the same
//! N-repeat assertion below carries over unchanged.
//!
//! On mismatch the failing seed is printed as `DIFFERENTIAL_SEED=<seed>`
//! (specs/02 §11.5) so the failure replays forever.

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

    /// Number of independent re-runs per case. The spec requires N ≥ 16; 16 keeps
    /// each proptest case fast (each run drives the real VM pipeline once per
    /// `AwaitFinalization`) while honestly exercising many fresh-driver replays.
    const REPEATS: usize = 16;

    proptest! {
        // A spread of seeds, kept fast for a per-PR run: 24 cases × 16 repeats ×
        // (a few finalizations each) stays well under the leashed
        // differential-package nextest timeout while covering a broad seed range.
        #![proptest_config(ProptestConfig::with_cases(24))]

        /// The headline property: a seed-derived workload replayed N ≥ 16 times
        /// against the real pipeline (a fresh driver each time) is byte-identical
        /// across every repeat.
        #[test]
        fn determinism_repeat(seed in any::<u64>()) {
            // 1. Build the workload deterministically from the seed.
            let program = Program::from_seed(seed);

            // 2. Run the deterministic Rust replay path N times, reconstructing the
            //    driver each iteration so no shared mutable state can carry
            //    determinism for free — each run starts from scratch.
            let mut runs = Vec::with_capacity(REPEATS);
            for _ in 0..REPEATS {
                let driver = LockstepDriver::new(seed);
                let observations = driver
                    .replay_recorded(&program)
                    .expect("replay_recorded run");
                // Compare the NORMALIZED sequence (timestamps stripped, per-instance
                // ids masked, set-valued fields sorted) so only genuine protocol
                // divergence — never expected non-determinism — can break equality.
                let normalized: Vec<_> =
                    observations.iter().map(|o| o.normalized()).collect();
                runs.push(normalized);
            }

            // 3. Every run must equal the first — byte/value-identical across all N.
            let first = runs.first().expect("at least one run");
            for (i, run) in runs.iter().enumerate().skip(1) {
                prop_assert_eq!(
                    run, first,
                    "DIFFERENTIAL_SEED={} — run {} of {} diverged from run 0: \
                     nondeterministic replay observation sequence",
                    seed, i, REPEATS
                );
            }

            // The pipeline really ran N times: the first run captured at least one
            // finalization with an accepted block (height >= 1). Without this a
            // trivially-empty observation would pass the equality check vacuously.
            prop_assert!(
                !first.is_empty(),
                "DIFFERENTIAL_SEED={seed} — expected at least one finalization observation"
            );
            let accepted_a_block = first.iter().any(|obs| {
                obs.fields
                    .iter()
                    .find(|(k, _)| k == "xchain.last_accepted.height")
                    .and_then(|(_, v)| v.parse::<u64>().ok())
                    .is_some_and(|h| h >= 1)
            });
            prop_assert!(
                accepted_a_block,
                "DIFFERENTIAL_SEED={seed} — expected at least one finalization with an accepted block"
            );
        }
    }

    /// A focused, non-proptest companion that pins one seed and additionally proves
    /// the N-repeat equality check genuinely catches a fork: a deliberately
    /// perturbed copy of one run must no longer match the others. This guards the
    /// assertion itself from silently degenerating into a tautology.
    #[test]
    fn determinism_repeat_detects_a_fork() {
        let seed = 0xD37E_C7ED_u64;
        let program = Program::from_seed(seed);

        let mut runs = Vec::with_capacity(REPEATS);
        for _ in 0..REPEATS {
            let driver = LockstepDriver::new(seed);
            let observations = driver
                .replay_recorded(&program)
                .expect("replay_recorded run");
            runs.push(observations);
        }

        // Honest re-runs: every normalized sequence equals the first.
        let first_run = runs.first().expect("at least one run");
        let first_norm: Vec<_> = first_run.iter().map(|o| o.normalized()).collect();
        for (i, run) in runs.iter().enumerate().skip(1) {
            let run_norm: Vec<_> = run.iter().map(|o| o.normalized()).collect();
            assert_eq!(
                run_norm, first_norm,
                "DIFFERENTIAL_SEED={seed} — run {i} of {REPEATS} diverged from run 0"
            );
        }

        // Inject a deliberate divergence into a copy of the last run and confirm
        // the comparison breaks — the equality assertion is load-bearing.
        let mut forked = runs.last().expect("at least one run").clone();
        let head = forked.first_mut().expect("at least one observation");
        head.set_field("xchain.last_accepted.id", "deadbeef");
        let forked_norm: Vec<_> = forked.iter().map(|o| o.normalized()).collect();
        assert_ne!(
            forked_norm, first_norm,
            "an injected last-accepted divergence must break the N-repeat comparison"
        );
    }
}
