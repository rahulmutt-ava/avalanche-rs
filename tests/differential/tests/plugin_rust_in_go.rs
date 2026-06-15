// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.3 — `differential::plugin_rust_in_go` (specs/16 §3, specs/07 §5.1,
//! specs/02 §11).
//!
//! Proves a minimal Rust test-VM **plugin binary**
//! (`ava-vm-rpc --example testvm_plugin`) speaks the rpcchainvm v45 **guest**
//! protocol and can be hosted by a foreign rpcchainvm host, in two arms:
//!
//! 1. **Offline arm** (`plugin_rust_in_go_builds_and_serves`, runs every CI run,
//!    no feature, not ignored): builds the plugin via [`build_rust_plugin`], then
//!    spawns the built binary as a real subprocess and drives the guest half of
//!    the v45 reverse-dial handshake — it sets `AVALANCHE_VM_RUNTIME_ENGINE_ADDR`
//!    to a loopback listener it owns and asserts the plugin dials back
//!    ([`assert_plugin_dials_back`]), and that without the env var the plugin
//!    fails fast ([`assert_plugin_fails_without_env`]). The full in-process v45
//!    `Runtime.Initialize` + `VM`/health serve proof lives in `ava-vm-rpc`'s own
//!    tests (`tests/handshake.rs`, `tests/vm_initialize.rs`; plan
//!    M9.1/M9.2/M9.10/M9.11) — this crate does not depend on `ava-vm-rpc`, so the
//!    offline proof here is the subprocess black-box.
//!
//! 2. **Live arm** (`plugin_rust_in_go_live`, behind the `live` Cargo feature +
//!    `#[ignore]`): builds the plugin, launches a real Go `avalanchego` host
//!    (`$AVALANCHEGO_PATH`) configured to run it as a custom VM, and asserts the
//!    Go host completes the v45 reverse-dial handshake. Never runs in CI / this
//!    sandbox (needs an external Go binary + a configured data dir; see below).

#![allow(unused_crate_dependencies)]

use std::time::Duration;

use ava_differential::plugin::{
    assert_plugin_dials_back, assert_plugin_fails_without_env, build_rust_plugin,
};

/// Offline arm: build the plugin and prove it speaks the v45 guest protocol as a
/// real subprocess. Runs every CI run, offline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_rust_in_go_builds_and_serves() {
    // Build the Rust test-VM plugin (cargo build of the example). The binary path
    // is returned and asserted to exist.
    let plugin = build_rust_plugin().expect("build testvm_plugin example");
    assert!(
        plugin.exists(),
        "built plugin binary exists at {}",
        plugin.display()
    );

    // Guest half of the v45 handshake: with the engine address set, the plugin
    // must dial it back (read env, bind V, dial R, attempt Runtime.Initialize).
    assert_plugin_dials_back(&plugin, Duration::from_secs(10))
        .await
        .expect("plugin dials back the runtime address (v45 guest handshake)");

    // Without the engine address the plugin must fail fast (Go's rpcchainvm host
    // relies on this), not hang.
    assert_plugin_fails_without_env(&plugin, Duration::from_secs(10))
        .await
        .expect("plugin fails fast without AVALANCHE_VM_RUNTIME_ENGINE_ADDR");
}

/// Live arm: a real Go `avalanchego` node hosts the Rust plugin and completes the
/// v45 reverse-dial handshake. Gated behind the `live` feature + `#[ignore]` so
/// it never runs in CI / this sandbox; a scheduled/nightly job (or
/// `AVALANCHEGO_PATH=<bin> cargo nextest run -p ava-differential --features live
/// -- --ignored`) runs it.
///
/// LIVE-ARM operator requirements (what the nightly job must supply, because the
/// launcher cannot wire it blind):
///   * `$AVALANCHEGO_PATH` → a Go `avalanchego` binary (rpcchainvm protocol 45).
///   * A Go node **data dir** whose `plugins/` directory contains the built Rust
///     plugin binary renamed to the **VM id** the chain uses, plus a
///     genesis/subnet config that instantiates a blockchain on that VM (so the Go
///     `rpcchainvm` runtime spawns the plugin with
///     `AVALANCHE_VM_RUNTIME_ENGINE_ADDR`). Pass the node flags that point at it
///     via `$AVALANCHEGO_EXTRA_ARGS` (space-separated), e.g.
///     `--data-dir=<dir> --plugin-dir=<dir>/plugins --network-id=local`.
///   * Without that data dir the Go node boots but never spawns the plugin, so
///     the handshake is not observed and this test fails — by design, surfacing
///     the unmet operator requirement rather than passing vacuously.
#[cfg(feature = "live")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a live Go avalanchego host ($AVALANCHEGO_PATH) + a configured plugin data dir — nightly only"]
async fn plugin_rust_in_go_live() {
    use ava_differential::plugin::{
        assert_handshake_complete, go_binary_path, launch_go_host_with_plugin,
    };

    // Skip gracefully if the Go binary is not configured.
    let Some(go_bin) = go_binary_path() else {
        eprintln!("AVALANCHEGO_PATH unset — skipping live plugin_rust_in_go");
        return;
    };

    let plugin = build_rust_plugin().expect("build testvm_plugin example");

    let extra_args: Vec<String> = std::env::var("AVALANCHEGO_EXTRA_ARGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();

    let host = launch_go_host_with_plugin(&plugin, &go_bin, &extra_args, Duration::from_secs(60))
        .await
        .expect("launch Go host with Rust plugin");

    assert_handshake_complete(&host)
        .expect("Go host completes the v45 reverse-dial handshake with the Rust plugin");
}
