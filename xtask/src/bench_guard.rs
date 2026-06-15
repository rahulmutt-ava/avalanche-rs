// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `bench-guard` — the critical-path criterion perf gate (specs/02 §9, 16 §5(9),
//! 00 §9).
//!
//! Runs a SMALL representative set of critical-path criterion benches (codec
//! encode/decode round-trip + secp256k1 signature verify — the operations spec
//! §9 calls out), reads criterion's per-bench mean point estimate from
//! `target/criterion/<bench>/new/estimates.json`, compares each against the
//! committed advisory baseline under `.config/criterion-baseline/<bench>.json`,
//! and FAILS if any bench's mean exceeds its baseline by more than the threshold
//! (default 10%).
//!
//! The committed baselines are *advisory local* numbers (machine-specific); real
//! CI baselines are regenerated per-runner. See
//! `.config/criterion-baseline/README.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

/// Default regression threshold: a bench failing if it is >10% slower than the
/// committed baseline.
const DEFAULT_THRESHOLD: f64 = 0.10;

/// One critical-path bench under guard: the crate it lives in, the criterion
/// `--bench` target name, and the individual benchmark id criterion writes under
/// `target/criterion/<id>/`.
struct GuardedBench {
    /// Crate the bench harness lives in (`cargo bench -p <crate>`).
    crate_name: &'static str,
    /// `[[bench]] name = ...` / `--bench <target>` value.
    bench_target: &'static str,
    /// The criterion benchmark id (the directory under `target/criterion/`).
    /// Equal to the `c.bench_function("<id>", ..)` name in the harness.
    bench_id: &'static str,
}

/// The committed critical-path set (specs/02 §9). Kept deliberately small so the
/// guard finishes well under a minute in CI.
const GUARDED: &[GuardedBench] = &[
    GuardedBench {
        crate_name: "ava-codec",
        bench_target: "codec",
        bench_id: "codec_roundtrip",
    },
    GuardedBench {
        crate_name: "ava-crypto",
        bench_target: "signature",
        bench_id: "secp256k1_verify",
    },
];

/// Whether `new` regresses past `base` by more than `threshold` (fractional,
/// e.g. `0.10` == 10%).
///
/// A faster or equal measurement (`new <= base`) never trips the gate; only a
/// slowdown beyond the threshold does. A non-positive or non-finite `base` is
/// treated as "no usable baseline" and never trips (the driver warns instead).
fn over_threshold(base: f64, new: f64, threshold: f64) -> bool {
    if !base.is_finite() || !new.is_finite() || base <= 0.0 {
        return false;
    }
    (new - base) / base > threshold
}

/// Repo root: xtask lives at `<root>/xtask`, so its manifest dir's parent is the
/// workspace root regardless of the current working directory.
fn repo_root() -> anyhow::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest dir has no parent (cannot locate repo root)")
}

/// Read a single `f64` mean point estimate (nanoseconds) from a JSON file shaped
/// like criterion's `estimates.json` (`{"mean":{"point_estimate":<f64>}}`) or
/// the committed baseline (`{"mean_ns":<f64>}`). Both shapes are accepted so the
/// committed baselines stay human-legible while still matching criterion output.
fn read_mean_ns(path: &Path) -> anyhow::Result<f64> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading mean estimate from {}", path.display()))?;
    parse_mean_ns(&raw).with_context(|| format!("parsing mean estimate from {}", path.display()))
}

/// Extract the mean point estimate (ns) from either supported JSON shape without
/// pulling in a JSON dependency: scan for `"point_estimate"` (criterion) first,
/// then `"mean_ns"` (committed baseline).
fn parse_mean_ns(raw: &str) -> anyhow::Result<f64> {
    for key in ["point_estimate", "mean_ns"] {
        if let Some(v) = scan_number_after(raw, key) {
            return Ok(v);
        }
    }
    bail!("no `point_estimate` or `mean_ns` number found in JSON")
}

/// Find `"<key>"`, skip the following `:` and whitespace, and parse the numeric
/// literal that follows. Minimal, dependency-free, sufficient for the two flat
/// shapes above.
fn scan_number_after(raw: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\"");
    let start = raw.find(&needle)?.checked_add(needle.len())?;
    let rest = raw.get(start..)?;
    let after_colon = rest.trim_start().strip_prefix(':')?.trim_start();
    let num: String = after_colon
        .chars()
        .take_while(|c| {
            c.is_ascii_digit() || *c == '.' || *c == 'e' || *c == 'E' || *c == '-' || *c == '+'
        })
        .collect();
    num.parse::<f64>().ok()
}

/// `bench-guard`: run the guarded benches and fail on a >threshold regression.
pub fn run(threshold: Option<f64>) -> anyhow::Result<()> {
    let threshold = threshold.unwrap_or(DEFAULT_THRESHOLD);
    if !threshold.is_finite() || threshold < 0.0 {
        bail!("threshold must be a non-negative, finite fraction (got {threshold})");
    }
    let root = repo_root()?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let mut regressions = Vec::new();
    for b in GUARDED {
        eprintln!(
            "==> bench-guard: {} :: {} ({})",
            b.crate_name, b.bench_target, b.bench_id
        );
        run_one_bench(&cargo, &root, b)?;

        let estimate_path = root
            .join("target/criterion")
            .join(b.bench_id)
            .join("new/estimates.json");
        let new_ns = read_mean_ns(&estimate_path)?;

        let baseline_path = root
            .join(".config/criterion-baseline")
            .join(format!("{}.json", b.bench_id));
        let base_ns = read_mean_ns(&baseline_path)?;

        let delta_pct = if base_ns > 0.0 {
            (new_ns - base_ns) / base_ns * 100.0
        } else {
            f64::NAN
        };
        let tripped = over_threshold(base_ns, new_ns, threshold);
        eprintln!(
            "    baseline {base_ns:.1} ns, new {new_ns:.1} ns ({delta_pct:+.2}%) — {}",
            if tripped { "REGRESSION" } else { "ok" }
        );
        if tripped {
            regressions.push(format!(
                "{}: {base_ns:.1} ns -> {new_ns:.1} ns ({delta_pct:+.2}%, > {:.1}% threshold)",
                b.bench_id,
                threshold * 100.0
            ));
        }
    }

    if !regressions.is_empty() {
        bail!(
            "bench-guard: {} bench(es) regressed beyond threshold:\n  {}",
            regressions.len(),
            regressions.join("\n  ")
        );
    }
    eprintln!(
        "bench-guard: all {} critical-path benches within threshold.",
        GUARDED.len()
    );
    Ok(())
}

/// `cargo bench -p <crate> --bench <target>` (stdio inherited). Criterion writes
/// the mean estimate to `target/criterion/<id>/new/estimates.json`.
fn run_one_bench(cargo: &str, root: &Path, b: &GuardedBench) -> anyhow::Result<()> {
    let status = Command::new(cargo)
        .current_dir(root)
        .args(["bench", "-p", b.crate_name, "--bench", b.bench_target])
        .status()
        .with_context(|| {
            format!(
                "spawning `cargo bench -p {} --bench {}`",
                b.crate_name, b.bench_target
            )
        })?;
    if !status.success() {
        bail!(
            "`cargo bench -p {} --bench {}` failed: {status}",
            b.crate_name,
            b.bench_target
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_threshold_trips_on_regression() {
        // A 2x slowdown is far beyond the 10% threshold -> must trip the gate.
        assert!(
            over_threshold(100.0, 200.0, 0.10),
            "over_threshold() must trip on a 2x regression"
        );
        // A 1% drift is under the 10% threshold -> must NOT trip.
        assert!(
            !over_threshold(100.0, 101.0, 0.10),
            "over_threshold() must not trip on a 1% drift"
        );
    }

    #[test]
    fn over_threshold_allows_faster_and_boundary() {
        // Faster than baseline never trips.
        assert!(!over_threshold(100.0, 50.0, 0.10), "faster must not trip");
        // Exactly at the threshold is allowed (strict `>`), just over trips.
        assert!(
            !over_threshold(100.0, 110.0, 0.10),
            "exactly +10% must not trip"
        );
        assert!(
            over_threshold(100.0, 110.01, 0.10),
            "just over +10% must trip"
        );
    }

    #[test]
    fn over_threshold_handles_bad_baseline() {
        // Non-positive / non-finite baselines are "no usable baseline": never trip.
        assert!(
            !over_threshold(0.0, 1.0, 0.10),
            "zero baseline must not trip"
        );
        assert!(
            !over_threshold(-1.0, 1.0, 0.10),
            "negative baseline must not trip"
        );
        assert!(
            !over_threshold(f64::NAN, 1.0, 0.10),
            "NaN baseline must not trip"
        );
    }

    #[test]
    fn parse_mean_ns_reads_criterion_shape() {
        let raw = r#"{"mean":{"confidence_interval":{"lower_bound":1.0},"point_estimate":42.5,"standard_error":0.1}}"#;
        let got = parse_mean_ns(raw).expect("parse criterion estimates.json shape");
        assert!(
            (got - 42.5).abs() < 1e-9,
            "criterion point_estimate, got {got}"
        );
    }

    #[test]
    fn parse_mean_ns_reads_baseline_shape() {
        let raw = r#"{ "bench_id": "codec_roundtrip", "mean_ns": 123.75 }"#;
        let got = parse_mean_ns(raw).expect("parse committed baseline shape");
        assert!((got - 123.75).abs() < 1e-9, "baseline mean_ns, got {got}");
    }
}
