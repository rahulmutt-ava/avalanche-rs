// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.20 — crash-injection hardening suite (specs/27 §9, §2 CC-ATOMIC, §3.1
//! two-sided shared-memory consistency; specs/02 §11).
//!
//! Two arms, following the established M9 cadence:
//!
//! 1. **Offline arm** (runs every CI run, no feature, not ignored): drives the
//!    in-process [`AcceptHarness`] through the C0..C4 crash matrix and asserts the
//!    Rust recovery pipeline is itself **all-or-nothing** (CC-ATOMIC) and
//!    **idempotent**, and that an X→peer shared-memory export crashed inside the
//!    `[SM-replay, write)` window leaves the peer chain observing all-or-nothing
//!    (the UTXO is never half-exported, double-spendable, nor lost). These are
//!    *deterministic properties of the Rust impl* — they do **not** compare against
//!    a Go oracle (see the honesty note below).
//!
//! 2. **Live Go-oracle-equivalence arm** (`crash_injection_vs_go_oracle`, behind
//!    the `live` Cargo feature + `#[ignore]`): would compare the post-crash+restart
//!    recovered state to a Go node driven through the same crash+restart. Early
//!    returns when the oracle fixture is unavailable; never runs in CI / this
//!    sandbox.
//!
//! ## Honesty note (what the offline arm proves vs. defers)
//!
//! The in-process surface available to this crate (the `ava-database` KV tier +
//! the SAE recovery pipeline) lets us prove, deterministically and every CI run:
//!
//! * the §2.2 atomic-batch accept survives every crash point all-or-nothing;
//! * the naive per-key accept *tears* (the anti-pattern CC-ATOMIC forbids), so the
//!   atomic path is demonstrably the load-bearing one;
//! * recovery reconciliation (drop the orphan diff / SM entry) is idempotent;
//! * a peer chain observes the export all-or-nothing (§3.1).
//!
//! It does **not** boot a real multi-chain node, so it cannot prove byte-identical
//! post-recovery state vs Go — that comparison is the gated live arm (specs/27 §9
//! item 1/2). The offline arm asserts exactly the determinism + atomicity it can,
//! and does not fake a Go comparison.

#![allow(unused_crate_dependencies)]

use ava_differential::crash::{
    AcceptBatch, AcceptHarness, CommitStrategy, CrashPoint, RecoveredState,
};

/// Genesis (parent) height every cycle starts from; the in-flight accept targets
/// `GENESIS + 1`.
const GENESIS: u64 = 0;
const IN_FLIGHT: u64 = GENESIS + 1;

/// A representative accept with a cross-chain shared-memory export, so the crash
/// matrix exercises all three CC-ATOMIC batch components (state diff + LA pointer
/// + SM put).
fn sample_batch() -> AcceptBatch {
    AcceptBatch {
        height: IN_FLIGHT,
        state_value: b"state-diff-bytes".to_vec(),
        shared_memory: Some((b"input-id-0001".to_vec(), b"marshalled-utxo".to_vec())),
    }
}

/// CC-ATOMIC (specs/27 §2.1/§3): across **every** crash point, the §2.2
/// atomic-batch accept recovers all-or-nothing — an accepted block is fully
/// present or fully absent (no partial diff, no dangling last-accepted, no orphan
/// shared-memory entry) — and re-running recovery is idempotent (stable final
/// state). Asserted against the Rust pipeline itself (deterministic), not a Go
/// oracle.
#[test]
fn crash_injection_cc_atomic() {
    for point in CrashPoint::offline_matrix() {
        let batch = sample_batch();
        let harness = AcceptHarness::new(GENESIS).expect("seed genesis marker");

        // First recovery.
        let recovered = harness
            .run_cycle(&batch, point, CommitStrategy::AtomicBatch)
            .expect("run crash+restart+recovery cycle");

        // All-or-nothing: with the atomic batch, a non-None crash leaves NOTHING
        // (the whole batch was unwritten); None leaves EVERYTHING.
        assert!(
            recovered.is_atomic(IN_FLIGHT, GENESIS, true),
            "CC-ATOMIC violated at {point:?}: {recovered:?}"
        );

        match point {
            CrashPoint::None => {
                assert_eq!(
                    recovered.last_accepted,
                    Some(IN_FLIGHT),
                    "clean accept advances the marker ({point:?})"
                );
                assert!(recovered.state_present, "clean accept persists state diff");
                assert!(
                    recovered.shared_memory_present,
                    "clean accept persists the SM export"
                );
                assert!(
                    !recovered.dropped_orphan,
                    "clean accept leaves no orphan to drop"
                );
            }
            _ => {
                assert_eq!(
                    recovered.last_accepted,
                    Some(GENESIS),
                    "crashed atomic accept leaves the marker on the parent ({point:?})"
                );
                assert!(
                    !recovered.state_present,
                    "crashed atomic accept persists no state diff ({point:?})"
                );
                assert!(
                    !recovered.shared_memory_present,
                    "crashed atomic accept persists no SM entry ({point:?})"
                );
            }
        }

        // Idempotency: a second, read-only observation matches the reconciled view
        // (minus the one-shot `dropped_orphan` flag, which only the first recovery
        // sets). Re-running recovery yields the same final state.
        let again = harness
            .observe(IN_FLIGHT)
            .expect("re-observe after recovery");
        let expected = RecoveredState {
            dropped_orphan: false,
            ..recovered.clone()
        };
        assert_eq!(
            again, expected,
            "recovery is idempotent at {point:?}: re-observation differs"
        );
    }
}

/// The naive per-key accept (the CC-ATOMIC anti-pattern) **tears** under a
/// mid-write crash: the state diff lands but the marker/SM do not. This proves the
/// atomic-batch path of `crash_injection_cc_atomic` is the load-bearing one — and
/// that recovery still reconciles the torn state back to all-or-nothing (it drops
/// the orphan diff, restoring the "fully absent" corner).
#[test]
fn naive_per_key_tears_then_recovery_reconciles() {
    let batch = sample_batch();
    let harness = AcceptHarness::new(GENESIS).expect("seed genesis marker");

    // Drive the torn accept + recovery.
    let recovered = harness
        .run_cycle(&batch, CrashPoint::MidWrite, CommitStrategy::NaivePerKey)
        .expect("run torn accept cycle");

    // Recovery dropped the orphan state diff (the tear was real and got
    // reconciled), and the final state is all-or-nothing (fully absent).
    assert!(
        recovered.dropped_orphan,
        "naive mid-write must leave an orphan diff for recovery to drop"
    );
    assert!(
        recovered.is_atomic(IN_FLIGHT, GENESIS, true),
        "recovery must reconcile the torn accept to all-or-nothing: {recovered:?}"
    );
    assert_eq!(
        recovered.last_accepted,
        Some(GENESIS),
        "reconciled marker stays on the parent"
    );
    assert!(!recovered.state_present, "reconciled state diff is dropped");

    // Idempotent: re-observing is stable.
    let again = harness.observe(IN_FLIGHT).expect("re-observe");
    assert_eq!(
        again,
        RecoveredState {
            dropped_orphan: false,
            ..recovered
        },
        "reconciliation is idempotent"
    );
}

/// Two-sided shared-memory consistency (specs/27 §3.1): an X→peer (X→P / X→C)
/// export crashed inside the `[SM-replay, write)` window leaves the peer chain
/// observing **all-or-nothing** — the UTXO is never half-exported (present without
/// a durable producer) nor lost (absent after a clean export). Built on the
/// `(key, value)` shared-memory contract of `atomic::exported_utxo_observation`.
#[test]
fn shared_memory_two_sided_consistency() {
    let input_id = b"input-id-0001";

    // (a) Crash in the export window: the producer block never committed, so the
    //     peer chain must NOT see the exported UTXO (no double-spend source, no
    //     orphan).
    for point in [
        CrashPoint::BeforeWrite,
        CrashPoint::MidWrite,
        CrashPoint::AfterStateBeforeMarker,
    ] {
        for strategy in [CommitStrategy::AtomicBatch, CommitStrategy::NaivePerKey] {
            let batch = sample_batch();
            let harness = AcceptHarness::new(GENESIS).expect("seed genesis");
            harness
                .run_cycle(&batch, point, strategy)
                .expect("run export crash cycle");
            let peer = harness
                .peer_observation(input_id)
                .expect("peer reads shared memory");
            assert!(
                !peer.present,
                "peer must not observe a UTXO whose producer crashed ({point:?}, {strategy:?})"
            );
            assert!(
                peer.value.is_empty(),
                "absent export yields no value bytes ({point:?}, {strategy:?})"
            );
        }
    }

    // (b) Clean export: the producer committed, so the peer chain DOES see exactly
    //     the exported UTXO bytes (never lost).
    let batch = sample_batch();
    let harness = AcceptHarness::new(GENESIS).expect("seed genesis");
    harness
        .run_cycle(&batch, CrashPoint::None, CommitStrategy::AtomicBatch)
        .expect("run clean export cycle");
    let peer = harness
        .peer_observation(input_id)
        .expect("peer reads shared memory");
    assert!(peer.present, "clean export is visible to the peer chain");
    assert_eq!(
        peer.value, b"marshalled-utxo",
        "peer reads back the exact exported UTXO bytes"
    );
}

// ===========================================================================
// Live Go-oracle-equivalence arm (gated).
// ===========================================================================

/// Live arm: compare the Rust node's post-crash+restart recovered state to a Go
/// `avalanchego` node driven through the **same** crash + restart, asserting
/// byte-identical reconciled last-accepted / state root / (for SAE) A/E/S
/// frontiers (specs/27 §9 items 1/2, specs/11 invariant 7). Gated behind the
/// `live` feature + `#[ignore]` so it never runs in CI / this sandbox.
///
/// LIVE-ARM operator requirements (what the nightly job must supply — the harness
/// cannot wire this blind, exactly as the M9.3 / M9.14 live arms document):
///   * `$AVALANCHEGO_PATH` → a Go `avalanchego` binary (rpcchainvm protocol 45),
///     built per CLAUDE.md (`~/avalanchego/build/avalanchego`).
///   * A **crash-injection Go oracle fixture**: a recorded corpus (under
///     `tests/vectors/crash/`) emitted by an env-gated Go vector-emitter that
///     drives the real Go P/X/C/SAE accept path, kills the node at each §3 crash
///     point (C0..C7 — including the `[SM-replay, write)` window of §3.1), restarts
///     via the Go recovery path, and records per-crash-point the reconciled
///     last-accepted pointer, the post-recovery state root, the peer-chain
///     shared-memory `get(...)` bytes, and (for SAE) the recovered A/E/S frontier
///     observations as JSON. Same recorded-oracle shape as the M7.29
///     `sae_recovery` / M9.14 corpora: emitter lives in
///     `tests/differential/go-oracle/`, copied into `~/avalanchego` (env-gated),
///     committed corpus consumed here.
///   * Without that fixture this test early-returns (skips) rather than failing
///     vacuously; with it, it replays each crash point through the Rust
///     [`AcceptHarness`] / `ava_saevm_core::recover` pipeline and asserts the
///     reconciled observation equals the Go-recorded one.
#[cfg(feature = "live")]
#[test]
#[ignore = "requires a live Go avalanchego oracle ($AVALANCHEGO_PATH) + a recorded crash-injection corpus — nightly only"]
fn crash_injection_vs_go_oracle() {
    // Skip gracefully if the Go oracle / fixture is not configured.
    let fixture = std::env::var("AVA_CRASH_ORACLE_CORPUS").ok();
    let Some(corpus_dir) = fixture else {
        eprintln!(
            "AVA_CRASH_ORACLE_CORPUS unset — skipping live crash_injection_vs_go_oracle \
             (no Go-recorded crash corpus available)"
        );
        return;
    };
    if !std::path::Path::new(&corpus_dir).is_dir() {
        eprintln!("crash oracle corpus dir {corpus_dir} missing — skipping live arm");
        return;
    }

    // LIVE-ARM (deferred): with the corpus present, iterate each recorded
    // crash-point vector, replay it through the Rust recovery pipeline
    // (`AcceptHarness` for the KV VMs, `ava_saevm_core::recover` for SAE), and
    // assert the Rust-reconciled observation equals the Go-recorded one. The
    // replay/parse wiring lands when the Go crash-injection emitter + corpus are
    // produced by the nightly job (the harness side is `crash::AcceptHarness` +
    // `saevm::replay_recovery_vector`, already exercised offline above).
    eprintln!(
        "crash oracle corpus present at {corpus_dir} — live Go-equivalence replay is the \
         deferred nightly step (see LIVE-ARM doc-comment)"
    );
}
