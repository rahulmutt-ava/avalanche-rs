// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M7.29 SAE crash+restart recovery differential (`differential::sae_recovery`,
//! specs/11 §1.4 / §10 invariant 7, specs/27 §9, specs/02 §11).
//!
//! Drives the **real Rust SAE recovery pipeline** ([`ava_saevm_core::recover`])
//! over a corpus emitted by the **live Go `vms/saevm` node** and asserts the
//! post-recovery A/E/S frontiers + `LastSettled` choice + roots match the Go
//! node driven through the *same* crash + restart.
//!
//! # Mode (recorded-oracle, per-PR — specs/02 §11.1)
//!
//! Every per-PR / CI run replays the **committed JSON corpus** under
//! `tests/vectors/saevm/recovery_differential/` (produced once by the live Go
//! oracle; see *re-freezing* below). The corpus carries, per canonical height,
//! the Go block's **wire bytes** (RLP-encoded geth block) + the committed
//! `ExecutionResults` and the Go source + recovered frontier observations.
//!
//! ## What is genuinely cross-checked
//!
//! * **Block hashes** — the Rust `parse_block` decoder re-seals every Go-emitted
//!   block and recomputes `keccak256(RLP(header))`; recovery rebuilds the
//!   frontier from those hashes, so the recovered `LastSettled` hash equalling
//!   the Go node's proves wire + hash parity.
//! * **Settlement choice** — the Rust `last_to_settle_at` walk recomputes which
//!   height becomes `LastSettled` from the Go-emitted gas-times + parsed
//!   build-times. Matching the Go `LastSettled` height proves the settlement
//!   rule (`settle_at = BlockTime(head) − Tau`, specs/11 §1.2) is byte-faithful.
//! * **Crash-invariance** — three crash points (mid-execute/between-commit-and
//!   head, archival-after-commit, commit-interval-exactly) all reconstruct the
//!   SAME A/E/S, exactly as the Go node does (re-execution from the last
//!   committed root is pure, specs/11 §6.1).
//!
//! Real-EVM **state roots** are the Go-emitted values fed straight into the Rust
//! `RecoverySource`; they round-trip unchanged (verifying recovery restores the
//! same root, invariant 7) but are NOT independently recomputed by the Rust
//! executor here — that would require a real-EVM differential and is moot for
//! this header/settlement-level recovery test (the Go node pins
//! `firewood-go-ethhash` v0.6.0 vs the Rust workspace's v0.5.0; by never
//! recomputing a firewood root we sidestep that divergence — see the M7.29
//! status note).
//!
//! # Live mode (env-gated, not run in CI)
//!
//! Re-freeze the corpus from the live Go node (committed emitter at
//! `tests/differential/go-oracle/recovery_vector_emitter_test.go`):
//!
//! ```sh
//! # in the avalanchego checkout ($AVALANCHEGO_DIR, default ../avalanchego):
//! cp tests/differential/go-oracle/recovery_vector_emitter_test.go \
//!    $AVALANCHEGO_DIR/vms/saevm/sae/
//! SAE_EMIT_RECOVERY_VECTORS=$PWD/tests/vectors/saevm/recovery_differential \
//!   go test ./vms/saevm/sae/ -run TestEmitRecoveryVectors -count=1
//! ```

// This integration-test target consumes only `ava_differential` +
// `pretty_assertions` + `proptest` + `tokio`, but the crate's lib + dev deps
// are all linked; per the established `unused_crate_dependencies` idiom each
// such test file silences the lint locally (see tests/smoke.rs).
#![allow(unused_crate_dependencies)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ava_differential::{FrontierObservation, replay_recovery_vector};
use pretty_assertions::assert_eq;

/// The committed recovery-differential corpus directory (workspace-rooted).
fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/saevm/recovery_differential")
}

/// The crash points the Go oracle scripted (each = a distinct commit cadence,
/// specs/27 §3 C6): mid-execute (a committed root below the head), archival
/// (commit every block), and exactly-on-a-commit-boundary.
const CRASH_POINTS: &[&str] = &[
    "between_commit_and_head",
    "archival_after_commit",
    "commit_interval_exactly",
];

/// Replay one corpus file and assert the Rust-reconstructed frontier equals both
/// the Go source frontier (pre-crash) and the Go recovered frontier (post
/// restart). Returns the matched observation for the caller to cross-check.
async fn assert_vector_matches(crash_point: &str) -> FrontierObservation {
    let path = corpus_dir().join(format!("recovery_{crash_point}.json"));
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read recovery corpus {}: {e}", path.display()));

    let (rust, go_source, go_recovered) = replay_recovery_vector(&json)
        .await
        .unwrap_or_else(|e| panic!("replay recovery vector {crash_point}: {e}"));

    // The Go node's own crash+restart is consistent (source == recovered).
    assert_eq!(
        go_source, go_recovered,
        "[{}] Go source frontier != Go recovered frontier",
        crash_point,
    );

    // The headline cross-implementation assertion: the Rust recovery
    // reconstructs the EXACT A/E/S heights, LastSettled hash, and roots the Go
    // node did (specs/11 §10 invariant 7).
    assert_eq!(
        rust, go_recovered,
        "[{}] Rust-recovered frontier != Go-recovered frontier",
        crash_point,
    );

    rust
}

mod differential {
    use proptest::prelude::*;

    use super::{BTreeSet, CRASH_POINTS, FrontierObservation, assert_eq, assert_vector_matches};

    /// `differential::sae_recovery` — the M7.29 headline test.
    ///
    /// A proptest over the scripted crash points: for each `(crash-point)` the
    /// Rust SAE node is driven through recovery from the live-Go-emitted block
    /// stream + crash, restarted, and the post-recovery A/E/S frontiers + state
    /// roots are asserted equal to the Go SAE node driven through the same crash
    /// + restart (recorded-oracle mode).
    #[test]
    fn sae_recovery() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        proptest!(ProptestConfig::with_cases(48), |(idx in 0usize..CRASH_POINTS.len())| {
            let crash_point = *CRASH_POINTS
                .get(idx)
                .expect("idx strategy bounded to CRASH_POINTS.len()");
            let observed: FrontierObservation =
                runtime.block_on(assert_vector_matches(crash_point));

            // The reconstructed frontier ordering holds: S <= E <= A.
            prop_assert!(
                observed.settled_height <= observed.executed_height
                    && observed.executed_height <= observed.accepted_height,
                "[{}] frontier ordering S<=E<=A violated: {:?}",
                crash_point,
                observed,
            );
        });

        // Beyond the proptest sampling: assert ALL crash points reconstruct an
        // identical final A/E/S (the crash-invariance theorem, exhaustively).
        let mut finals: BTreeSet<(u64, u64, u64, String)> = BTreeSet::new();
        for &cp in CRASH_POINTS {
            let f = runtime.block_on(assert_vector_matches(cp));
            finals.insert((
                f.accepted_height,
                f.executed_height,
                f.settled_height,
                f.settled_hash,
            ));
        }
        assert_eq!(
            finals.len(),
            1,
            "all crash points must reconstruct an identical final A/E/S; got {:?}",
            finals,
        );
    }
}
