// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `xtask gen-genesis` — re-freeze the `ava-genesis` golden vectors from the
//! Go oracle (specs 23 §7/§9.2, 02 §6.2; M8.8).
//!
//! Copies the committed emitter
//! (`crates/ava-genesis/tests/go-oracle/genesis_dump_oracle_test.go`) into the
//! avalanchego checkout's `genesis/` package (it needs the unexported
//! `unmodifiedLocalConfig` + the embedded `genesis_test.json`), runs the
//! env-gated `TestEmitGenesisVectors`, writes
//! `crates/ava-genesis/tests/vectors/genesis/{block_ids.json,p_chain_bytes_*.bin}`,
//! then removes the copy so the Go tree stays clean. The avalanchego checkout
//! defaults to `../avalanchego` (override with `AVALANCHEGO_DIR`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

const ORACLE_FILE: &str = "genesis_dump_oracle_test.go";

/// Re-extract the genesis golden vectors from the Go tree.
pub fn run() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest dir has no parent (cannot locate repo root)")?;
    let avalanchego_dir = std::env::var_os("AVALANCHEGO_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.join("../avalanchego"));
    if !avalanchego_dir.join("genesis").is_dir() {
        bail!(
            "avalanchego checkout not found at {} (set AVALANCHEGO_DIR)",
            avalanchego_dir.display()
        );
    }

    let oracle_src = repo_root
        .join("crates/ava-genesis/tests/go-oracle")
        .join(ORACLE_FILE);
    let oracle_dst = avalanchego_dir.join("genesis").join(ORACLE_FILE);
    let out_dir = repo_root.join("crates/ava-genesis/tests/vectors/genesis");

    let commit = Command::new("git")
        .args([
            "-C",
            &avalanchego_dir.to_string_lossy(),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    std::fs::copy(&oracle_src, &oracle_dst)
        .with_context(|| format!("copy {} -> {}", oracle_src.display(), oracle_dst.display()))?;
    // Always remove the dropped-in copy, success or failure.
    let result = (|| -> anyhow::Result<()> {
        let status = Command::new("go")
            .args([
                "test",
                "./genesis/",
                "-run",
                "TestEmitGenesisVectors",
                "-count=1",
            ])
            .current_dir(&avalanchego_dir)
            .env("CGO_ENABLED", "1")
            .env("GENESIS_EMIT_VECTORS", &out_dir)
            .env("AVALANCHEGO_COMMIT", &commit)
            .status()
            .context("failed to spawn `go test` (is Go on PATH?)")?;
        if !status.success() {
            bail!("go test TestEmitGenesisVectors failed: {status}");
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_file(&oracle_dst)
        .with_context(|| format!("remove {}", oracle_dst.display()));
    result?;
    cleanup?;

    eprintln!(
        "xtask gen-genesis: vectors re-frozen under {} (avalanchego @ {commit}); \
         re-run `cargo nextest run -p ava-genesis -E 'binary(golden_genesis_block_id)'`.",
        out_dir.display()
    );
    Ok(())
}
