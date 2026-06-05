// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Test orchestration subcommands (specs/02 §1, §11; tier X / X.4, X.13, X.16).

use std::process::Command;

use anyhow::{Context, bail};

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

/// `test-fuzz`: brief smoke of every cargo-fuzz target.
///
/// SCAFFOLD: per-parser fuzz targets + the smoke loop are owned by tier-X task
/// X.16 (`ava-codec` first). Until then this is a no-op success.
pub fn test_fuzz() -> anyhow::Result<()> {
    eprintln!("xtask test-fuzz: no fuzz smoke loop yet (owned by tier-X task X.16).");
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
