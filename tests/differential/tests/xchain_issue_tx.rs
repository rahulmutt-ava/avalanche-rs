// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `differential::xchain_issue_tx` — the X-Chain tx-issuance determinism gate
//! (specs 00 §6.1, 02 §11).
//!
//! ## What this gate proves (today)
//!
//! A seed derives a program of X-Chain `BaseTx` issuances whose tx/key bytes are
//! FULLY seed-derived. The program is run through the REAL `ava-avm` VM/block
//! pipeline (seed genesis state → admit txs → build → verify → accept) on two
//! INDEPENDENT VM instances, each producing a normalized [`Observation`]
//! (last-accepted block id + height + the full sorted UTXO set). The SAME seed
//! must yield a BYTE-IDENTICAL `Observation` across the two instances — the
//! determinism / total-order property (specs 00 §6.1, 02 §11).
//!
//! ## Deferred (the Go-oracle arms — X.13/X.15)
//!
//! The Go recorded-oracle and the live two-binary `differential::xchain_issue_tx`
//! arms are gated on the (unimplemented) [`LockstepDriver`](ava_differential::LockstepDriver)
//! (`replay_recorded` is owned by tier-X task X.13, and there is no Go
//! recorded-oracle / live two-binary mode yet). Until then THIS proptest is the
//! per-PR DETERMINISM gate (self-vs-self) — the per-subsystem `Observation`
//! collector entry point the X.13 spec calls for ("each subsystem adds its
//! collector — M5 X-Chain"). The Go comparison, richer tx kinds (CreateAsset /
//! Operation / Import / Export), and 10k-case scaling land with X.13/X.15.
//!
//! On mismatch the failing seed is printed as `DIFFERENTIAL_SEED=<seed>`
//! (specs 02 §11.5) so the failure is replayable.

// This target consumes only `ava-differential` + `proptest`; the other crate
// deps are used by the lib / other integration targets. Per the established
// `unused_crate_dependencies` idiom, each integration-test file that does not
// consume them opts out of the per-binary false positive (see `smoke.rs`).
#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod differential {
    use ava_differential::xchain;
    use proptest::prelude::*;

    proptest! {
        // Start small (the plan's TDD entry point is a single BaseTx program)
        // and scale to a meaningful-but-fast count. 512 cases run in ~2s — well
        // under the leashed differential-package nextest timeout (900s) — while
        // exercising a broad spread of seeds (chain length 1..=4, seed-derived
        // amounts). Scaling to 10k + the Go-oracle comparison + richer tx kinds
        // lands with X.13/X.15.
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn xchain_issue_tx(seed in any::<u64>()) {
            // Two independent VM instances seeded identically from `seed` must
            // produce byte-identical normalized observations.
            let a = xchain::run_program(seed);
            let b = xchain::run_program(seed);

            prop_assert_eq!(
                &a, &b,
                "DIFFERENTIAL_SEED={} — nondeterministic X-Chain observation across two VM instances",
                seed
            );

            // The observation must be non-trivial: a block was accepted at
            // height >= 1 (the seeded BaseTx issuance produced a StandardBlock).
            let height: u64 = a
                .fields
                .iter()
                .find(|(k, _)| k == "xchain.last_accepted.height")
                .map(|(_, v)| v.parse().unwrap())
                .expect("height field present");
            prop_assert!(
                height >= 1,
                "DIFFERENTIAL_SEED={seed} — expected >=1 accepted block, got height {height}"
            );
        }
    }
}
