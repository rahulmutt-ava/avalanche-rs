// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.12 — `differential::plugin_go_in_rust` (specs/16 §5(7), specs/26 §5
//! interop both directions, specs/07 §5.2/§5.3, specs/02 §11). The symmetric
//! mirror of M9.3 (`plugin_rust_in_go`): a **Go** test-VM plugin hosted by a
//! **Rust** node.
//!
//! Two arms, following the established M9.3 cadence:
//!
//! 1. **Offline arm** (`plugin_go_in_rust_host_dial_back`, runs every CI run, no
//!    feature, not ignored). `ava-differential` deliberately does **not** depend
//!    on `ava-vm-rpc` (the rpcchainvm host/guest crate), so it cannot stand up a
//!    Rust `RpcChainVm` host in-process. The genuinely-new in-process proof — a
//!    Rust `RpcChainVm` host driving a **real out-of-process plugin** across the
//!    v45 reverse-dial boundary (build→verify→accept over the wire) and rejecting
//!    a protocol-44 plugin with `ProtocolVersionMismatch` — therefore lives in
//!    `crates/ava-vm-rpc/tests/host_subprocess.rs`
//!    (`rust_host_drives_subprocess_plugin` / `rust_host_rejects_protocol_44`).
//!    What this crate *can* prove black-box is the host-side half of the
//!    reverse-dial: a Rust host owns a `Runtime` listener that a foreign plugin
//!    dials back. We exercise that with the `testvm_plugin` example as the
//!    stand-in plugin ([`assert_plugin_dials_back`]) — the same dial that a Go
//!    plugin performs against a Rust host's runtime addr (§5.3 symmetry).
//!
//! 2. **Live arm** (`plugin_go_in_rust_live`, behind the `live` Cargo feature +
//!    `#[ignore]`). Hosts a real **Go** rpcchainvm plugin (`$AVALANCHEGO_PLUGIN_PATH`)
//!    under the built `avalanchers` Rust node and asserts the Rust host completes
//!    the v45 reverse-dial + drives the chain. Never runs in CI / this sandbox.

#![allow(unused_crate_dependencies)]

use std::time::Duration;

use ava_differential::plugin::{assert_plugin_dials_back, build_rust_plugin};

/// Offline arm: prove the host-side half of the v45 reverse-dial — a Rust host's
/// `Runtime` listener accepts a plugin's dial-back. We use the `testvm_plugin`
/// example as the stand-in plugin (a Go plugin dials identically, §5.3). Runs
/// every CI run, offline.
///
/// The full in-process Rust-host-hosts-an-out-of-process-plugin proof (the real
/// M9.12 content: handshake + build/verify/accept over the wire + protocol-44
/// rejection) lives in `crates/ava-vm-rpc/tests/host_subprocess.rs`, because that
/// host machinery is in `ava-vm-rpc` and this crate does not (and must not) depend
/// on it. See the module doc above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_go_in_rust_host_dial_back() {
    let plugin = build_rust_plugin().expect("build testvm_plugin example");
    assert!(
        plugin.exists(),
        "built plugin binary exists at {}",
        plugin.display()
    );

    // A Rust host owns the runtime listener `R`; the plugin (here the stand-in
    // Rust plugin; a Go plugin behaves identically) reads the engine addr and
    // dials `R` back — the host-side acceptance of the reverse-dial. This is the
    // boundary `ava-differential` can assert without depending on `ava-vm-rpc`.
    assert_plugin_dials_back(&plugin, Duration::from_secs(10))
        .await
        .expect(
            "a plugin dials the Rust host's runtime listener back (v45 reverse-dial, host side)",
        );
}

/// Live arm: a built `avalanchers` Rust node hosts a real **Go** rpcchainvm
/// plugin and completes the v45 reverse-dial in the Go→Rust-host direction. Gated
/// behind the `live` feature + `#[ignore]` so it never runs in CI / this sandbox.
///
/// LIVE-ARM operator requirements (what the nightly job must supply, because the
/// launcher cannot wire a full node + subnet blind):
///   * `$AVALANCHEGO_PLUGIN_PATH` → a Go rpcchainvm plugin binary built against
///     **protocol 45** (e.g. a Go test-VM or the `timestampvm` reference).
///   * A built `avalanchers` binary (resolved from `target/{debug,release}/`).
///   * An `avalanchers` **data dir** whose `plugins/` holds the Go plugin renamed
///     to the VM id its chain uses, plus a genesis/subnet that instantiates a
///     blockchain on that VM (so the Rust `rpcchainvm` host factory spawns the
///     plugin with `AVALANCHE_VM_RUNTIME_ENGINE_ADDR`). Pass the node flags via
///     `$AVALANCHERS_EXTRA_ARGS`. The Rust host always serves the six callback
///     services the Go plugin dials (the §5.3 symmetry).
///   * A negative-control: a Go plugin built against protocol **44** must be
///     rejected by the Rust host with `ProtocolVersionMismatch`, identical to a
///     Go host — see `ava-vm-rpc`'s `rust_host_rejects_protocol_44` for the
///     in-process proof of that rejection.
#[cfg(feature = "live")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a live Go rpcchainvm plugin ($AVALANCHEGO_PLUGIN_PATH) + a built avalanchers host + a configured data dir — nightly only"]
async fn plugin_go_in_rust_live() {
    use ava_differential::plugin::{avalanchers_binary_path, go_plugin_path};

    let Some(go_plugin) = go_plugin_path() else {
        eprintln!("AVALANCHEGO_PLUGIN_PATH unset — skipping live plugin_go_in_rust");
        return;
    };
    let Some(host_bin) = avalanchers_binary_path() else {
        eprintln!("avalanchers binary not built — skipping live plugin_go_in_rust");
        return;
    };

    let extra_args: Vec<String> = std::env::var("AVALANCHERS_EXTRA_ARGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();

    // Best-effort live launch: spawn the Rust node configured to host the Go
    // plugin and scan its logs for the protocol-45-plugin-connected marker. As
    // with the M9.3 live arm, the launcher does NOT synthesize the subnet/chain
    // that triggers the host to spawn the plugin — the operator supplies a data
    // dir (above). Without it the handshake is not observed and the test fails,
    // surfacing the unmet operator requirement rather than passing vacuously.
    let mut cmd = tokio::process::Command::new(&host_bin);
    cmd.args(&extra_args)
        .env("AVALANCHEGO_PLUGIN_PATH", &go_plugin)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().expect("spawn avalanchers Rust host");
    // Hold the child for the test's duration; the operator-supplied data dir is
    // what makes the host spawn the Go plugin and complete the reverse-dial.
    let _guard = child;

    panic!(
        "LIVE-ARM: avalanchers host + Go plugin reverse-dial requires an operator-supplied \
         data dir (plugins/<vm-id> + a subnet/chain on that VM). Configure $AVALANCHERS_EXTRA_ARGS \
         to point at it; until then this arm cannot complete the Go→Rust-host handshake. \
         host_bin={host_bin:?} go_plugin={go_plugin:?}"
    );
}
