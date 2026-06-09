// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Test orchestration subcommands (specs/02 §1, §11; tier X / X.4, X.13, X.16).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

/// Per-target libFuzzer wall-clock budget (seconds): brief smoke vs `--long`.
const FUZZ_SMOKE_SECS: u64 = 10;
const FUZZ_LONG_SECS: u64 = 300;

/// Run a cargo subcommand, inheriting stdio, and fail on non-zero exit.
fn cargo(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(env!("CARGO"))
        .args(args)
        .status()
        .with_context(|| format!("failed to spawn `cargo {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`cargo {}` failed: {status}", args.join(" "));
    }
    Ok(())
}

/// `test-unit`: nextest CI profile + doctests (mirrors the Taskfile task).
pub fn test_unit() -> anyhow::Result<()> {
    cargo(&[
        "nextest",
        "run",
        "--workspace",
        "--all-features",
        "--profile",
        "ci",
    ])?;
    cargo(&["test", "--doc", "--workspace", "--all-features"])
}

/// `test-unit-fast`: quick local nextest run.
pub fn test_unit_fast() -> anyhow::Result<()> {
    cargo(&["nextest", "run", "--workspace"])
}

/// `test-fuzz`: run every `crates/*/fuzz` cargo-fuzz target for a bounded
/// duration (`--long` extends the per-target budget). Each target is fuzzed for
/// `-max_total_time` seconds; a libFuzzer crash makes the target — and the task
/// — fail.
///
/// Must be invoked inside the nightly `fuzz` dev shell (the Taskfile sets
/// `NIX_DEV_SHELL=fuzz`); cargo-fuzz needs nightly for `-Zsanitizer`/`-Zbuild-std`.
/// On the stable shell `cargo fuzz` errors out — use `tests/prop_fuzz_smoke.rs`
/// (run by `cargo nextest`) for stable coverage there.
pub fn test_fuzz(long: bool) -> anyhow::Result<()> {
    let secs = if long {
        FUZZ_LONG_SECS
    } else {
        FUZZ_SMOKE_SECS
    };
    let mode = if long { "long" } else { "smoke" };
    // Resolve the cargo invoking us at runtime (nightly inside the fuzz shell);
    // env!("CARGO") would bake in whatever toolchain last compiled xtask.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let crates = discover_fuzz_crates(&repo_root()?)?;
    if crates.is_empty() {
        eprintln!("xtask test-fuzz: no fuzz crates found under crates/*/fuzz.");
        return Ok(());
    }

    for crate_dir in &crates {
        let name = crate_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for target in list_fuzz_targets(&cargo, crate_dir)? {
            eprintln!("==> fuzz {mode}: {name} :: {target} (-max_total_time={secs})");
            run_fuzz_target(&cargo, crate_dir, &target, secs)?;
        }
    }
    Ok(())
}

/// Repo root: xtask lives at `<root>/xtask`, so its manifest dir's parent is the
/// workspace root regardless of the current working directory.
fn repo_root() -> anyhow::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest dir has no parent (cannot locate repo root)")
}

/// Every `crates/<crate>/fuzz` directory that holds a cargo-fuzz manifest,
/// sorted for stable ordering.
///
/// Discovers two levels:
/// - `crates/<crate>/fuzz/Cargo.toml` — top-level crate fuzz dirs.
/// - `crates/ava-saevm/<subcrate>/fuzz/Cargo.toml` — SAE sub-workspace fuzz
///   dirs (one extra nesting level, matching `crates/ava-saevm/*/fuzz`).
fn discover_fuzz_crates(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let crates_dir = root.join("crates");
    let mut out = Vec::new();

    // Top-level: crates/<crate>/fuzz
    for entry in std::fs::read_dir(&crates_dir)
        .with_context(|| format!("reading {}", crates_dir.display()))?
    {
        let path = entry?.path();
        if path.join("fuzz/Cargo.toml").is_file() {
            out.push(path);
        }
    }

    // SAE sub-workspace: crates/ava-saevm/<subcrate>/fuzz
    let saevm_dir = crates_dir.join("ava-saevm");
    if saevm_dir.is_dir() {
        for entry in std::fs::read_dir(&saevm_dir)
            .with_context(|| format!("reading {}", saevm_dir.display()))?
        {
            let path = entry?.path();
            if path.join("fuzz/Cargo.toml").is_file() {
                out.push(path);
            }
        }
    }

    out.sort();
    Ok(out)
}

/// `cargo fuzz list` for one crate's `fuzz/` dir → its target names.
fn list_fuzz_targets(cargo: &str, crate_dir: &Path) -> anyhow::Result<Vec<String>> {
    let fuzz_dir = crate_dir.join("fuzz");
    let output = Command::new(cargo)
        .args(["fuzz", "list", "--fuzz-dir"])
        .arg(&fuzz_dir)
        .output()
        .with_context(|| format!("spawning `cargo fuzz list` for {}", fuzz_dir.display()))?;
    if !output.status.success() {
        bail!(
            "`cargo fuzz list --fuzz-dir {}` failed: {}",
            fuzz_dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// `cargo fuzz run <target> -- -max_total_time=<secs>` (stdio inherited).
fn run_fuzz_target(cargo: &str, crate_dir: &Path, target: &str, secs: u64) -> anyhow::Result<()> {
    let fuzz_dir = crate_dir.join("fuzz");
    let status = Command::new(cargo)
        .args(["fuzz", "run", target, "--fuzz-dir"])
        .arg(&fuzz_dir)
        .arg("--")
        .arg(format!("-max_total_time={secs}"))
        .status()
        .with_context(|| format!("spawning `cargo fuzz run {target}`"))?;
    if !status.success() {
        bail!("fuzz target `{target}` failed: {status}");
    }
    Ok(())
}

/// `test-differential`: run the differential harness.
///
/// SCAFFOLD: the `ava-differential` crate is a skeleton (tier-X task X.13);
/// recorded-oracle / live two-binary modes and seed replay are filled in there
/// (X.13/X.14/X.15). For now this runs the crate's (skeleton) test suite.
pub fn test_differential(seed: Option<u64>, recorded: bool) -> anyhow::Result<()> {
    if let Some(seed) = seed {
        eprintln!(
            "xtask test-differential --seed {seed}: single-seed replay is owned by X.13; \
             running the ava-differential skeleton suite instead."
        );
    }
    if recorded {
        eprintln!("xtask test-differential --recorded: recorded-oracle mode is owned by X.13.");
    }
    cargo(&["test", "-p", "ava-differential"])
}

/// `test-reexecute`: reexecute golden block ranges, compare state roots.
///
/// SCAFFOLD: owned by tier-X task X.13 (and deepened by the VM milestones).
pub fn test_reexecute() -> anyhow::Result<()> {
    eprintln!(
        "xtask test-reexecute: reexecute suite is owned by tier-X task X.13 (deepened M4–M7)."
    );
    Ok(())
}
