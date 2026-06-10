// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `saevm-exit-gate`: the M7 (SAE VM / ACP-194) milestone exit gate
//! (specs/11 §10 invariants + the exit gate, specs/02 §13 per-crate contracts).
//!
//! A static, deterministic checker (NOT a test runner) that asserts the M7
//! deliverables are present and complete:
//!   1. every named exit-gate test exists and is referenced (determinism,
//!      recovery + streaming differentials, the golden block-hash vector, and
//!      all eleven `invariant::*` functions);
//!   2. the shared `crates/ava-saevm/tests/PORTING.md` matrix has no `wip` /
//!      `⬜` / placeholder rows, and its Summary counts match the actual rows;
//!   3. the golden-vector corpus (`tests/vectors/saevm/{blocks,settlement,
//!      recovery,recovery_differential,streaming_differential}` + `MANIFEST.json`)
//!      and the block-decode fuzz target are present.
//!
//! It greps the test files rather than invoking cargo so the gate is fast and
//! deterministic; the actual test *runs* are the orchestrator's full nextest
//! pass (see the M7.32 gate-run steps).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// One `(file, needle)` existence assertion against a test source.
struct TestProbe {
    /// Human-readable name used in the PASS/FAIL report (`path::test`).
    label: &'static str,
    /// Repo-relative file the test must live in.
    file: &'static str,
    /// Substrings that must ALL appear in the file (e.g. the `mod` + the `fn`).
    needles: &'static [&'static str],
}

/// The named exit-gate tests (M7.32 plan text). The eleven `invariant::*`
/// functions live under `mod invariant { … }` in the core invariants harness.
const EXIT_TESTS: &[TestProbe] = &[
    TestProbe {
        label: "golden::sae_block_hash",
        file: "crates/ava-saevm/core/tests/golden.rs",
        needles: &["mod golden", "fn sae_block_hash"],
    },
    TestProbe {
        label: "prop::sae_execution_determinism",
        file: "crates/ava-saevm/exec/tests/determinism.rs",
        needles: &["mod prop", "fn sae_execution_determinism"],
    },
    TestProbe {
        label: "differential::sae_recovery",
        file: "tests/differential/tests/sae_recovery.rs",
        needles: &["mod differential", "fn sae_recovery"],
    },
    TestProbe {
        label: "differential::sae_streaming",
        file: "tests/differential/tests/sae_streaming.rs",
        needles: &["mod differential", "fn sae_streaming"],
    },
];

/// The eleven §10 invariants — one named `invariant::<name>` test each, all in
/// the core invariants harness.
const INVARIANTS: &[&str] = &[
    "atomics_before_broadcast",
    "determinism",
    "frontier_ordering",
    "gc_settled_ancestry",
    "no_reorg",
    "persist_order_accept",
    "persist_order_execute",
    "receipt_root_match",
    "recovery_equivalence",
    "settle_in_order",
    "stage_causality",
];

const INVARIANTS_FILE: &str = "crates/ava-saevm/core/tests/invariants.rs";
const PORTING_FILE: &str = "crates/ava-saevm/tests/PORTING.md";
const FUZZ_TARGET: &str = "crates/ava-saevm/blocks/fuzz/fuzz_targets/decode_block.rs";
const FUZZ_SMOKE: &str = "crates/ava-saevm/blocks/tests/parse_block_fuzz_smoke.rs";
const MANIFEST: &str = "tests/vectors/saevm/MANIFEST.json";

/// Golden-vector directories that must each hold at least one `*.json`.
const VECTOR_DIRS: &[&str] = &[
    "tests/vectors/saevm/blocks",
    "tests/vectors/saevm/settlement",
    "tests/vectors/saevm/recovery",
    "tests/vectors/saevm/recovery_differential",
    "tests/vectors/saevm/streaming_differential",
];

/// Forbidden status tokens / placeholder markers in PORTING.md rows.
const FORBIDDEN_ROW_MARKERS: &[&str] = &["⬜", "| wip ", "_(seeded"];

/// Run the gate: collect per-item PASS/FAIL, print a report, fail on any FAIL.
pub fn run() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask manifest dir has no parent (cannot locate repo root)")?
        .to_path_buf();

    let mut failures: Vec<String> = Vec::new();
    let mut report = String::new();

    check_named_tests(&repo_root, &mut report, &mut failures);
    check_invariants(&repo_root, &mut report, &mut failures);
    check_porting(&repo_root, &mut report, &mut failures);
    check_corpus(&repo_root, &mut report, &mut failures);

    print!("{report}");

    if failures.is_empty() {
        println!("\nsaevm-exit-gate: ALL CHECKS PASSED");
        Ok(())
    } else {
        bail!(
            "saevm-exit-gate: {} check(s) FAILED:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

/// Read a repo-relative file, recording a failure (and returning `None`) if it
/// is missing.
fn read_rel(root: &Path, rel: &str, failures: &mut Vec<String>) -> Option<String> {
    let path = root.join(rel);
    match fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) => {
            failures.push(format!("{rel}: cannot read ({e})"));
            None
        }
    }
}

fn pass(report: &mut String, label: &str) {
    let _ = writeln!(report, "  PASS  {label}");
}

fn fail(report: &mut String, failures: &mut Vec<String>, label: &str, why: &str) {
    let _ = writeln!(report, "  FAIL  {label} — {why}");
    failures.push(format!("{label}: {why}"));
}

/// (1a) Each named exit-gate test exists with all its needles present.
fn check_named_tests(root: &Path, report: &mut String, failures: &mut Vec<String>) {
    report.push_str("[exit tests]\n");
    for probe in EXIT_TESTS {
        let Some(src) = read_rel(root, probe.file, failures) else {
            fail(report, failures, probe.label, "test file missing");
            continue;
        };
        if let Some(missing) = probe.needles.iter().find(|n| !src.contains(**n)) {
            fail(
                report,
                failures,
                probe.label,
                &format!("`{missing}` not found in {}", probe.file),
            );
        } else {
            pass(report, probe.label);
        }
    }
}

/// (1b) All eleven `invariant::*` functions exist in the invariants harness,
/// referenced as `fn <name>(` inside a `mod invariant { … }`.
fn check_invariants(root: &Path, report: &mut String, failures: &mut Vec<String>) {
    report.push_str("[§10 invariants]\n");
    let Some(src) = read_rel(root, INVARIANTS_FILE, failures) else {
        for inv in INVARIANTS {
            fail(
                report,
                failures,
                &format!("invariant::{inv}"),
                "invariants harness missing",
            );
        }
        return;
    };
    if !src.contains("mod invariant") {
        fail(
            report,
            failures,
            "invariant module",
            "`mod invariant` not found",
        );
    }
    for inv in INVARIANTS {
        let needle = format!("fn {inv}(");
        if src.contains(&needle) {
            pass(report, &format!("invariant::{inv}"));
        } else {
            fail(
                report,
                failures,
                &format!("invariant::{inv}"),
                "function not found",
            );
        }
    }
}

/// (2) PORTING.md completeness: no forbidden rows, and the Summary counts match
/// the actual ✅ / 🟡 / ⬜ / n-a status-cell counts across all matrix rows.
fn check_porting(root: &Path, report: &mut String, failures: &mut Vec<String>) {
    report.push_str("[PORTING.md]\n");
    let Some(src) = read_rel(root, PORTING_FILE, failures) else {
        fail(report, failures, "PORTING.md", "matrix file missing");
        return;
    };

    // No forbidden markers (wip / not-ported / placeholder skeleton rows). Only
    // table rows (lines starting with `|`) are scanned, so the prose Legend line
    // is free to *name* the symbols without tripping the gate.
    let mut forbidden_hits: Vec<String> = Vec::new();
    for (lineno, line) in src.lines().enumerate() {
        if !line.trim_start().starts_with('|') {
            continue;
        }
        for marker in FORBIDDEN_ROW_MARKERS {
            if line.contains(marker) {
                forbidden_hits.push(format!(
                    "line {}: `{}`",
                    lineno.saturating_add(1),
                    marker.trim()
                ));
            }
        }
    }
    if forbidden_hits.is_empty() {
        pass(report, "no wip/⬜/placeholder rows");
    } else {
        fail(
            report,
            failures,
            "forbidden rows",
            &forbidden_hits.join("; "),
        );
    }

    // Count status cells in matrix rows (a `| status |` middle column). We count
    // by scanning data rows: lines that start with `| ` but are not the header
    // (`| Go test`) or separator (`|---`).
    let (mut ported, mut partial, mut not_ported, mut na) = (0usize, 0usize, 0usize, 0usize);
    for line in src.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("| ") {
            continue;
        }
        if trimmed.starts_with("| Go test") || trimmed.starts_with("|---") {
            continue;
        }
        // Status is the second pipe-delimited cell.
        let cells: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        // cells[0] == "" (before first pipe), cells[1] == Go test, cells[2] == status.
        let Some(status) = cells.get(2) else {
            continue;
        };
        match *status {
            "✅" => ported = ported.saturating_add(1),
            "🟡" => partial = partial.saturating_add(1),
            "⬜" => not_ported = not_ported.saturating_add(1),
            "n/a" => na = na.saturating_add(1),
            _ => {}
        }
    }
    let _ = writeln!(
        report,
        "  ----  matrix rows: {ported} ✅ / {partial} 🟡 / {not_ported} ⬜ / {na} n/a"
    );

    // The Summary line declares the counts; assert they match.
    let summary = src
        .lines()
        .find(|l| l.contains("**Summary") && l.contains("ported"));
    match summary {
        None => fail(report, failures, "Summary line", "not found"),
        Some(line) => {
            let want = (
                first_number_before(line, "ported ✅").or_else(|| first_number_before(line, "✅")),
                first_number_before(line, "partial 🟡").or_else(|| first_number_before(line, "🟡")),
                first_number_before(line, "n/a"),
            );
            let got = (Some(ported), Some(partial), Some(na));
            if want == got {
                pass(
                    report,
                    &format!("Summary matches rows ({ported} ✅ / {partial} 🟡 / {na} n/a)"),
                );
            } else {
                fail(
                    report,
                    failures,
                    "Summary counts",
                    &format!(
                        "declared {:?} but rows show (Some({ported}), Some({partial}), Some({na}))",
                        want
                    ),
                );
            }
        }
    }
}

/// Find the last run of ASCII digits that ends immediately before `marker`
/// appears in `line` (i.e. the count token preceding a `N ported ✅` phrase).
fn first_number_before(line: &str, marker: &str) -> Option<usize> {
    let idx = line.find(marker)?;
    let prefix = line[..idx].trim_end();
    let digits: String = prefix
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

/// (3) Vector corpus + fuzz target presence.
fn check_corpus(root: &Path, report: &mut String, failures: &mut Vec<String>) {
    report.push_str("[golden vectors + fuzz]\n");

    for rel in [FUZZ_TARGET, FUZZ_SMOKE, MANIFEST] {
        let path: PathBuf = root.join(rel);
        if path.is_file() {
            pass(report, rel);
        } else {
            fail(report, failures, rel, "missing");
        }
    }

    for dir in VECTOR_DIRS {
        let path = root.join(dir);
        let has_json = fs::read_dir(&path)
            .ok()
            .map(|entries| {
                entries.flatten().any(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
                })
            })
            .unwrap_or(false);
        if has_json {
            pass(report, dir);
        } else {
            fail(report, failures, dir, "no *.json vectors found");
        }
    }
}
