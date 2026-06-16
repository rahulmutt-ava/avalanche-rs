// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `acceptance`: the M9.23 final acceptance gate — the project's definition of
//! done (specs/16 §5, specs/02 §10.1/§13, specs/00 §11.7).
//!
//! A static, deterministic checker (NOT a test runner, like `saevm_exit_gate`).
//! It greps the test sources so the gate is fast and CI-cheap; the actual test
//! *runs* are the per-PR / nightly nextest passes. It asserts two things:
//!
//!   1. **Every `16` §5 DoD item maps to a named exit test that EXISTS** (file +
//!      `fn <name>(` needle probes). Each DoD item carries both an OFFLINE arm
//!      (runs every CI: recorded-oracle / determinism / config-snapshot) and,
//!      where a live Go node is needed, a LIVE arm (`#[cfg(feature="live")]`
//!      `#[ignore]`, run nightly/pre-release). The gate verifies BOTH arms are
//!      *present*, not that the live arm runs.
//!   2. **Zero `wip` rows across every `tests/PORTING.md`** (shares
//!      [`crate::porting`] — specs/02 §10.1: "zero `wip` rows").
//!
//! ## CI cadence (specs/00 §11.7, specs/02 §11.7)
//!
//! Per-PR CI runs the **offline arms** of every DoD test (recorded-Go-oracle
//! differentials, reexecute, the in-process plugin-handshake, config/genesis
//! goldens, bench-guard). The **live two-binary** arms (`mixed_network`,
//! `plugin_go_in_rust`, `test-upgrade`, `test-load`) are `#[cfg(feature="live")]`
//! plus `#[ignore]` and need an external Go `avalanchego` binary
//! (`$AVALANCHEGO_PATH`); they run nightly / pre-release, matching the entire M9
//! offline-arm precedent. This gate proves the named tests EXIST (both arms),
//! satisfying the structural half of the DoD; the live runs are a separate
//! scheduled job.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::bail;

use crate::porting;

/// One `16` §5 DoD item → the named exit test(s) backing it.
struct DodItem {
    /// `16` §5 clause label (for the PASS/FAIL report).
    label: &'static str,
    /// Repo-relative source file the test(s) must live in.
    file: &'static str,
    /// Substrings that must ALL appear in the file — typically the offline-arm
    /// `fn <name>(` plus, where applicable, the live-arm `fn <name>(`.
    needles: &'static [&'static str],
}

/// The `16` §5 definition-of-done checklist, each clause mapped to its named
/// exit test(s). File paths + fn names were verified by grepping the repo.
const DOD: &[DodItem] = &[
    // (1) Joins Mainnet & Fuji, tracks tip, no fork. Aggregates the mixed-network
    // no-fork property (the offline replay proves Go+Rust reach the same tip).
    DodItem {
        label: "16 §5(1) joins Mainnet & Fuji, tracks tip, no fork",
        file: "tests/differential/tests/mixed_network.rs",
        needles: &[
            "fn mixed_network_replay_is_deterministic(",
            "fn mixed_network(",
        ],
    },
    // (2) Interoperates indistinguishably (mixed Go+Rust net). Offline:
    // mixed_network replay + Observation normalization round-trip. Live: mixed_network.
    DodItem {
        label: "16 §5(2) indistinguishable mixed network",
        file: "tests/differential/tests/mixed_network_smoke.rs",
        needles: &[
            "fn mixed_network_config_is_deterministic(",
            "fn observation_normalization_round_trips(",
            "fn mixed_network_bringup_smoke(",
        ],
    },
    // (3a) Full differential incl. reexecute — C-Chain recorded mainnet range.
    DodItem {
        label: "16 §5(3) reexecute — C-Chain range (Go-identical roots)",
        file: "tests/reexecute/tests/cchain_range.rs",
        needles: &["fn reexecute_cchain_range("],
    },
    // (3b) reexecute — P/X recorded range.
    DodItem {
        label: "16 §5(3) reexecute — P/X range (Go-identical roots)",
        file: "tests/reexecute/tests/px_range.rs",
        needles: &["fn reexecute_px_range("],
    },
    // (4) Flag parity — zero diff vs the Go config flag snapshot.
    DodItem {
        label: "16 §5(4) golden::flag_parity (zero diff)",
        file: "crates/ava-config/tests/golden_flag_parity.rs",
        needles: &["fn flag_parity("],
    },
    // (5) API parity — structural-JSON equality per spec 14.
    DodItem {
        label: "16 §5(5) differential::api_parity",
        file: "crates/ava-api/tests/differential_api_parity.rs",
        needles: &["fn info_parity(", "fn platform_and_avm_method_sets_pinned("],
    },
    // (6) Genesis parity — exact Mainnet + Fuji genesis block IDs/bytes.
    DodItem {
        label: "16 §5(6) golden::genesis_block_id (Mainnet+Fuji)",
        file: "crates/ava-genesis/tests/golden_genesis_block_id.rs",
        needles: &["fn genesis_block_id("],
    },
    // (7a) Plugin interop — Rust VM in Go host (offline build/serve + live arm).
    DodItem {
        label: "16 §5(7) differential::plugin_rust_in_go (v45)",
        file: "tests/differential/tests/plugin_rust_in_go.rs",
        needles: &[
            "fn plugin_rust_in_go_builds_and_serves(",
            "fn plugin_rust_in_go_live(",
        ],
    },
    // (7b) Plugin interop — Go VM in Rust host (offline host-dial-back + live arm).
    DodItem {
        label: "16 §5(7) differential::plugin_go_in_rust (v45)",
        file: "tests/differential/tests/plugin_go_in_rust.rs",
        needles: &[
            "fn plugin_go_in_rust_host_dial_back(",
            "fn plugin_go_in_rust_live(",
        ],
    },
    // (8) Upgrade continuity — Go→Rust across an activation height incl. Go-dir
    // → RocksDB import. Offline: rolling-swap import + no-fork-across-cutover.
    // Live: go_to_rust.
    DodItem {
        label: "16 §5(8) test-upgrade (Go→Rust, Go-dir import)",
        file: "tests/upgrade/tests/go_to_rust.rs",
        needles: &[
            "fn rolling_swap_imports_each_node_byte_identically(",
            "fn no_fork_holds_across_cutover_and_a_divergence_is_caught(",
            "fn go_to_rust(",
        ],
    },
    // (9) Perf gates hold — bench-guard criterion baselines.
    DodItem {
        label: "16 §5(9) bench-guard (criterion baselines)",
        file: "xtask/src/bench_guard.rs",
        needles: &["fn run(", "fn over_threshold("],
    },
    // Sustained-load SLOs (specs/02 §10.3) — offline pipeline + metric-name SLOs;
    // live sustained_load arm (nightly).
    DodItem {
        label: "16 §5 supporting: test-load (metric-name SLOs)",
        file: "tests/load/tests/sustained_load.rs",
        needles: &["fn sustained_load_pipeline_offline(", "fn sustained_load("],
    },
];

/// Run the acceptance gate: probe every DoD test, then the PORTING zero-`wip`
/// invariant; print a PASS/FAIL report and bail non-zero on any failure.
///
/// # Errors
/// Returns an error if any DoD exit test is missing/renamed, any
/// `tests/PORTING.md` cannot be read, or any matrix carries a `wip` row.
pub fn run() -> anyhow::Result<()> {
    let root = porting::repo_root()?;

    let mut report = String::new();
    let mut failures: Vec<String> = Vec::new();

    check_dod(&root, &mut report, &mut failures);
    check_porting(&root, &mut report, &mut failures);

    print!("{report}");

    if failures.is_empty() {
        println!("\nacceptance: ALL CHECKS PASSED (16 §5 definition of done)");
        Ok(())
    } else {
        bail!(
            "acceptance: {} check(s) FAILED:\n  - {}",
            failures.len(),
            failures.join("\n  - "),
        );
    }
}

/// (1) Each `16` §5 DoD item maps to a named exit test that exists with all its
/// needles (offline + live arms) present.
fn check_dod(root: &Path, report: &mut String, failures: &mut Vec<String>) {
    report.push_str("[16 §5 definition of done — named exit tests]\n");
    for item in DOD {
        let path = root.join(item.file);
        let Ok(src) = fs::read_to_string(&path) else {
            let _ = writeln!(
                report,
                "  FAIL  {} — source {} missing",
                item.label, item.file
            );
            failures.push(format!("{}: {} missing", item.label, item.file));
            continue;
        };
        if let Some(missing) = item.needles.iter().find(|n| !src.contains(**n)) {
            let _ = writeln!(
                report,
                "  FAIL  {} — `{missing}` not found in {}",
                item.label, item.file,
            );
            failures.push(format!(
                "{}: `{missing}` missing from {}",
                item.label, item.file
            ));
        } else {
            let _ = writeln!(report, "  PASS  {}", item.label);
        }
    }
}

/// (2) Zero `wip` rows across every `tests/PORTING.md` (shares [`crate::porting`]).
fn check_porting(root: &Path, report: &mut String, failures: &mut Vec<String>) {
    report.push_str("[PORTING.md — zero wip rows (specs/02 §10.1)]\n");
    let files = match porting::collect(root) {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(report, "  FAIL  PORTING.md scan — {e}");
            failures.push(format!("PORTING.md scan: {e}"));
            return;
        }
    };
    let mut any_wip = false;
    for f in &files {
        if f.counts.wip_lines.is_empty() {
            continue;
        }
        any_wip = true;
        let lines = f
            .counts
            .wip_lines
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(report, "  FAIL  {} — wip row(s) @ lines {lines}", f.rel);
        failures.push(format!("{}: wip row(s) @ lines {lines}", f.rel));
    }
    if !any_wip {
        let _ = writeln!(
            report,
            "  PASS  zero wip rows across {} matrices",
            files.len(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dod_probes_resolve_against_the_repo() {
        // Every DoD item's source file must exist and contain all its needles —
        // this is the gate's own self-check (it would otherwise silently rot if a
        // test were renamed). Run from the real repo root.
        let root = porting::repo_root().expect("repo_root");
        let mut report = String::new();
        let mut failures = Vec::new();
        check_dod(&root, &mut report, &mut failures);
        assert!(
            failures.is_empty(),
            "DoD probes must resolve; failures: {failures:?}\nreport:\n{report}",
        );
    }

    #[test]
    fn acceptance_gate_is_green() {
        // The full gate (DoD probes + zero-wip) must pass in the committed tree.
        run().expect("acceptance gate must be green");
    }
}
