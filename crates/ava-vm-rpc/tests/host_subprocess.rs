// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Host-side out-of-process rpcchainvm proof — the symmetric mirror of M9.3
//! (`differential::plugin_rust_in_go`) for M9.12 (`plugin_go_in_rust`):
//! a Rust [`RpcChainVm`] host drives a **real subprocess plugin** over the v45
//! reverse-dial handshake (specs 07 §5.2/§5.3/§5.4; specs 26 §5 interop both
//! directions; specs 16 §5(7)).
//!
//! * `rust_host_drives_subprocess_plugin` — the genuinely-new proof: the host
//!   builds the `testvm_plugin` example and **spawns it as a separate OS
//!   process** (not an in-process `tokio::spawn` of `guest::serve_with_addr`, as
//!   `tests/vm_initialize.rs::rust_host_initializes_rust_guest` does). It then
//!   drives the host flow across the real process boundary: the v45 handshake
//!   completes, and a build→verify→accept→parse cycle advances `last_accepted`,
//!   every call an RPC to the subprocess. This exercises the host-side
//!   OS-process reverse-dial that the in-process M9.11 test cannot. (The
//!   `VM.Initialize`-over-the-wire leg is proven separately in-process in
//!   `tests/vm_initialize.rs`; see the NOTE on the body for why it is not
//!   re-driven against the `testvm_plugin` example here.)
//! * `rust_host_rejects_protocol_44` — a guest reporting the *previous*
//!   rpcchainvm protocol (44) must surface
//!   [`Error::ProtocolVersionMismatch`] through `RpcChainVm::start`, identical to
//!   a Go host. `tests/handshake.rs::handshake_protocol_mismatch` already covers
//!   the generic `protocol != 45` Runtime-level path (with `45 + 1`); this test
//!   pins the concrete "old node, 44" scenario end-to-end at the
//!   `RpcChainVm::start` boundary.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

use ava_vm::block::ChainVm;
use ava_vm::error::Error;

use ava_vm_rpc::host::RpcChainVm;
use ava_vm_rpc::{DEFAULT_HANDSHAKE_TIMEOUT, RPC_CHAIN_VM_PROTOCOL, guest};

// Pulled in by `tonic-build`/`tonic` transitively; referenced so the test
// binary's `unused_crate_dependencies` lint stays quiet.
use {tokio_stream as _, tonic as _};

/// The rpcchainvm protocol version a node *one major behind* would report.
/// `RPC_CHAIN_VM_PROTOCOL` is 45; 44 is the concrete "old node" version.
const PREVIOUS_PROTOCOL: u32 = RPC_CHAIN_VM_PROTOCOL - 1;

/// Resolves the workspace `target/` directory, honoring `CARGO_TARGET_DIR` when
/// set, else `<workspace-root>/target`. Mirrors the path logic in
/// `tests/differential/src/plugin.rs` so both halves of the M9.3/M9.12 mirror
/// locate the same built binary.
fn target_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    workspace_root().join("target")
}

/// The workspace root, derived from this crate's `CARGO_MANIFEST_DIR`
/// (`<root>/crates/ava-vm-rpc`).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // crates/
        .and_then(|p| p.parent()) // <root>
        .map(PathBuf::from)
        .unwrap_or(manifest)
}

#[cfg(windows)]
fn exe_name(stem: &str) -> String {
    format!("{stem}.exe")
}

#[cfg(not(windows))]
fn exe_name(stem: &str) -> String {
    stem.to_string()
}

/// The path the `testvm_plugin` example is built to (`<target>/debug/examples`).
fn plugin_binary_path() -> PathBuf {
    let mut p = target_dir();
    p.push("debug");
    p.push("examples");
    p.push(exe_name("testvm_plugin"));
    p
}

/// Builds the `testvm_plugin` example as a real binary and returns its path.
/// Mirrors `tests/differential/src/plugin.rs::build_rust_plugin`.
fn build_plugin() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = std::process::Command::new(cargo)
        .current_dir(workspace_root())
        .args(["build", "-p", "ava-vm-rpc", "--example", "testvm_plugin"])
        .status()
        .expect("spawn cargo build of testvm_plugin");
    assert!(status.success(), "cargo build of testvm_plugin failed");

    let path = plugin_binary_path();
    assert!(
        path.exists(),
        "built testvm_plugin binary missing at {}",
        path.display()
    );
    path
}

/// A spawned plugin subprocess, kept alive (kill-on-drop) for the test's
/// duration so the host can keep the gRPC channel open.
struct PluginProcess {
    _child: Child,
}

/// Spawn the built plugin as a real OS process with the runtime engine address
/// in its env, returning the handle (kept so the child outlives the host calls).
fn spawn_plugin(plugin_path: &PathBuf, engine_addr: &str) -> PluginProcess {
    let child = Command::new(plugin_path)
        .env(ava_vm_rpc::ENGINE_ADDRESS_KEY, engine_addr)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn testvm_plugin subprocess");
    PluginProcess { _child: child }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_host_drives_subprocess_plugin() {
    // Build the plugin binary once, up front (outside the launcher closure).
    let plugin_path = build_plugin();

    let token = CancellationToken::new();

    // The child must outlive `RpcChainVm::start` (the host keeps a live VM
    // channel to it), so stash it where the closure can move it out.
    let child_slot: Arc<Mutex<Option<PluginProcess>>> = Arc::new(Mutex::new(None));
    let child_slot_launcher = Arc::clone(&child_slot);

    // The launcher spawns the plugin as a REAL subprocess (not in-process). The
    // plugin reads ENGINE_ADDRESS_KEY, dials R back, reports the v45 handshake,
    // and serves the VM service on its own listener — all across the OS-process
    // boundary.
    let host = RpcChainVm::start(&token, Duration::from_secs(20), move |engine_addr| {
        let proc = spawn_plugin(&plugin_path, engine_addr);
        *child_slot_launcher.lock() = Some(proc);
    })
    .await
    .expect("v45 handshake + dial VM across the process boundary");

    let mut host = host;

    // Drive build -> verify -> accept -> last_accepted across the real process
    // boundary. `RpcChainVm::start` already seeded the host's last-accepted
    // snapshot via the benign `SetState(Unspecified)` probe (the subprocess'
    // `FixedGenesisVm` is uninitialized, so that snapshot is `Id::EMPTY`).
    // `build_block` builds the first child (height 1) over the wire, `verify`
    // and `accept` are RPCs to the subprocess, and `last_accepted` reads back
    // the accepted id — every call crosses the OS-process boundary, which the
    // in-process M9.11 test (`tests/vm_initialize.rs`) does not exercise.
    //
    // NOTE: we deliberately do NOT drive `VM.Initialize` here. The host's
    // `initialize` serves a proxied `rpcdb` `Database` to the guest; the guest's
    // proxied `DatabaseClient` owns a current-thread runtime that must be dropped
    // off the async runtime thread (the in-process Initialize test's `DbProbeVm`
    // does so by consuming the db inside `spawn_blocking`). The `testvm_plugin`
    // example ignores its proxied db, so the last `Arc` drops on a tokio worker
    // and panics ("Cannot drop a runtime in a context where blocking is not
    // allowed") — a pre-existing guest/rpcdb-client characteristic in code this
    // test may not modify. The end-to-end `VM.Initialize`-over-the-wire proof
    // (with a db-consuming guest) lives in `tests/vm_initialize.rs`.
    let prev = host.last_accepted(&token).await.expect("last_accepted");
    let blk = host
        .build_block(&token)
        .await
        .expect("build_block over wire");
    assert_eq!(blk.parent(), prev, "built on the seeded last-accepted id");
    assert_eq!(blk.height(), 1, "first built block is at height 1");
    blk.verify(&token).await.expect("verify over wire");
    blk.accept(&token).await.expect("accept over wire");
    let last = host.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(
        last,
        blk.id(),
        "accept advances last_accepted over the process boundary"
    );

    // `parse_block` round-trips the bytes back over the wire to the subprocess.
    let parsed = host
        .parse_block(&token, blk.bytes())
        .await
        .expect("parse_block over wire");
    assert_eq!(
        parsed.id(),
        blk.id(),
        "parse_block round-trips the id over the process boundary"
    );

    // Keep the subprocess alive until here, then drop it (kill-on-drop).
    drop(child_slot.lock().take());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rust_host_rejects_protocol_44() {
    let token = CancellationToken::new();

    // A launcher whose guest reports the previous rpcchainvm protocol (44). The
    // host runtime records a version mismatch, and `RpcChainVm::start` must
    // surface `ProtocolVersionMismatch` end-to-end — identical to a Go host
    // refusing a too-old plugin.
    let res = RpcChainVm::start(&token, DEFAULT_HANDSHAKE_TIMEOUT, |engine_addr| {
        let engine_addr = engine_addr.to_string();
        tokio::spawn(async move {
            // Bind a throwaway VM listener so we have an addr to report.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind throwaway VM listener");
            let v_addr = listener.local_addr().expect("local addr");
            let _ =
                guest::report_handshake(&engine_addr, PREVIOUS_PROTOCOL, &v_addr.to_string()).await;
        });
    })
    .await;

    let res = res.map(|_| ()); // RpcChainVm isn't Debug; collapse the Ok arm.
    assert!(
        matches!(res, Err(Error::ProtocolVersionMismatch)),
        "host rejects a protocol-44 plugin via RpcChainVm::start, got {res:?}"
    );
}
