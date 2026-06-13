// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M8.31 — the `avalanchers` binary is the full-node entrypoint: it answers
//! `--version` / `--version-json` / `--help` (printed-and-quit flags, 12 §9)
//! and builds the resolved [`ava_config::node::Config`] for mainnet and Fuji
//! without spawning the (blocking) node.
//!
//! The parse-only smoke lives as a unit test inside `app.rs`
//! (`build_config_for_mainnet_and_fuji`): running the full node would block, so
//! we exercise the config-building helper directly rather than the process.

use std::process::Command;

/// `--version` exits 0 and prints a human-readable version string that still
/// contains the local `avalanchers/` identity (the M0 invariant), while also
/// reporting the compatible avalanchego version + database/rpcchainvm versions
/// (Go `version.GetVersions().String()`). `--version-json` exits 0 and prints
/// parseable pretty JSON. Both flags together is an error → exit 1 (12 §9).
#[test]
fn version_flags() {
    let exe = env!("CARGO_BIN_EXE_avalanchers");

    let v = Command::new(exe)
        .arg("--version")
        .output()
        .expect("run --version");
    assert!(v.status.success(), "--version exits 0");
    let stdout = String::from_utf8_lossy(&v.stdout);
    assert!(
        stdout.contains("avalanchers/"),
        "--version stdout carries the avalanchers/ identity, got {stdout:?}"
    );

    let vj = Command::new(exe)
        .arg("--version-json")
        .output()
        .expect("run --version-json");
    assert!(vj.status.success(), "--version-json exits 0");
    let json: serde_json::Value =
        serde_json::from_slice(&vj.stdout).expect("--version-json stdout parses as JSON");
    assert!(
        json.get("application").is_some(),
        "--version-json has an application field, got {json:?}"
    );

    let both = Command::new(exe)
        .args(["--version", "--version-json"])
        .output()
        .expect("run --version --version-json");
    assert!(
        !both.status.success(),
        "--version + --version-json together is an error (exit 1)"
    );
    assert_eq!(both.status.code(), Some(1), "both flags → exit code 1");
}

/// `--help` exits 0 (clap prints the generated help).
#[test]
fn help_exits_0() {
    let exe = env!("CARGO_BIN_EXE_avalanchers");
    let h = Command::new(exe)
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(h.status.success(), "--help exits 0");
}
