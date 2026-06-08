// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `check-sae-lints`: structural guard for the SAE stricter-lint bar (M7.1,
//! specs/11 §3, 00 §7.7). Thin shell over `scripts/check-sae-lints.sh`, which
//! greps each `crates/ava-saevm/*/src/lib.rs` for the required inner attributes
//! (`#![forbid(unsafe_code)]`, `#![warn(clippy::pedantic)]`, the license header,
//! and — for the gas-time crates — `#![deny(clippy::arithmetic_side_effects)]`).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

/// Run `scripts/check-sae-lints.sh`, inheriting stdio, and fail on non-zero exit.
pub fn run() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask manifest dir has no parent (cannot locate repo root)")?;
    let script = repo_root.join("scripts/check-sae-lints.sh");

    let status = Command::new("bash")
        .arg(&script)
        .status()
        .with_context(|| format!("failed to spawn `bash {}`", script.display()))?;
    if !status.success() {
        bail!("`bash {}` failed: {status}", script.display());
    }
    Ok(())
}
