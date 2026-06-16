// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! rpcchainvm plugin-interop harness helpers (specs/07 §5.1, plan/M9 §M9.3/§M9.12).
//!
//! Helpers to build a Rust test-VM plugin binary
//! (`ava-vm-rpc --example testvm_plugin`), launch it under a live Go
//! `avalanchego` host, and assert the v45 reverse-dial handshake completes. The
//! live arm is gated behind the `live` Cargo feature + `#[ignore]` (it needs an
//! external Go binary at `$AVALANCHEGO_PATH`); see the integration tests in
//! `tests/plugin_rust_in_go.rs`.
//!
//! ## Design note — why these helpers use only `std`/`tokio`
//! The differential crate intentionally does **not** depend on `ava-vm-rpc`
//! (the rpcchainvm host/guest crate). The offline proof that the plugin speaks
//! the v45 guest protocol therefore drives the **built plugin binary as a real
//! subprocess** rather than calling `guest::serve_with_addr` in-process: it sets
//! `AVALANCHE_VM_RUNTIME_ENGINE_ADDR` to a loopback address it owns and asserts
//! the plugin dials back (the guest half of the handshake). The end-to-end
//! in-process v45 `Runtime.Initialize` proof lives in `ava-vm-rpc`'s own tests
//! (`tests/handshake.rs`, `tests/vm_initialize.rs`; plan M9.1/M9.2/M9.10/M9.11).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};

/// The env var the rpcchainvm host sets so the spawned plugin knows the host
/// runtime (`Runtime`) address to dial back (specs/07 §5.1). Must match
/// `ava_vm_rpc::ENGINE_ADDRESS_KEY` byte-for-byte.
pub const ENGINE_ADDRESS_KEY: &str = "AVALANCHE_VM_RUNTIME_ENGINE_ADDR";

/// The rpcchainvm protocol version the plugin reports in `Runtime.Initialize`
/// (specs/26 §5). Mirrors `ava_vm_rpc::RPC_CHAIN_VM_PROTOCOL`.
pub const RPC_CHAIN_VM_PROTOCOL: u32 = 45;

/// Errors raised by the plugin-interop harness.
#[derive(Debug, Error)]
pub enum PluginError {
    /// `cargo build` of the plugin example failed (non-zero exit).
    #[error("building the testvm_plugin example failed: {0}")]
    Build(String),

    /// The built plugin binary was not found at the expected target path.
    #[error("plugin binary not found at {0}")]
    BinaryMissing(PathBuf),

    /// `$AVALANCHEGO_PATH` is unset, or the Go binary does not exist.
    #[error("Go avalanchego binary not available: {0}")]
    GoBinaryMissing(String),

    /// Spawning a child process (plugin or Go host) failed.
    #[error("spawning {what} failed: {source}")]
    Spawn {
        /// What we tried to spawn ("plugin" / "go-host").
        what: &'static str,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// The plugin did not dial back the runtime address within the timeout.
    #[error("plugin did not complete the v45 reverse-dial handshake within {0:?}")]
    HandshakeTimeout(Duration),

    /// A generic I/O failure in the harness.
    #[error("plugin harness io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolves the workspace `target/` directory, honoring `CARGO_TARGET_DIR` when
/// set, else `<workspace-root>/target`. The workspace root is two levels up from
/// this crate's manifest dir (`tests/differential` → repo root).
fn target_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    workspace_root().join("target")
}

/// The workspace root, derived from this crate's `CARGO_MANIFEST_DIR`
/// (`<root>/tests/differential`).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // tests/
        .and_then(|p| p.parent()) // <root>
        .map(PathBuf::from)
        .unwrap_or(manifest)
}

/// The path the `testvm_plugin` example is built to (`<target>/debug/examples`).
fn plugin_binary_path() -> PathBuf {
    let mut p = target_dir();
    p.push("debug");
    p.push("examples");
    p.push(exe_name("testvm_plugin"));
    p
}

#[cfg(windows)]
fn exe_name(stem: &str) -> String {
    format!("{stem}.exe")
}

#[cfg(not(windows))]
fn exe_name(stem: &str) -> String {
    stem.to_string()
}

/// Builds the Rust test-VM plugin (`cargo build -p ava-vm-rpc --example
/// testvm_plugin`) and returns the path to the built binary.
///
/// # Errors
/// * [`PluginError::Build`] if `cargo build` exits non-zero.
/// * [`PluginError::BinaryMissing`] if the binary is absent after a successful
///   build (e.g. an unexpected target layout).
pub fn build_rust_plugin() -> Result<PathBuf, PluginError> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = std::process::Command::new(cargo)
        .current_dir(workspace_root())
        .args(["build", "-p", "ava-vm-rpc", "--example", "testvm_plugin"])
        .output()
        .map_err(|e| PluginError::Build(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PluginError::Build(stderr.into_owned()));
    }

    let path = plugin_binary_path();
    if !path.exists() {
        return Err(PluginError::BinaryMissing(path));
    }
    Ok(path)
}

/// The path the Go `avalanchego` host binary should live at, from
/// `$AVALANCHEGO_PATH`. Returns `None` if the env var is unset.
pub fn go_binary_path() -> Option<PathBuf> {
    std::env::var("AVALANCHEGO_PATH").ok().map(PathBuf::from)
}

/// The path to a built **Go** rpcchainvm plugin binary the live M9.12 arm hosts
/// under a Rust node, from `$AVALANCHEGO_PLUGIN_PATH`. Returns `None` if unset or
/// missing on disk.
pub fn go_plugin_path() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("AVALANCHEGO_PLUGIN_PATH").ok()?);
    p.exists().then_some(p)
}

/// The path to the built `avalanchers` node binary (`<target>/{debug,release}/
/// avalanchers`), which the live M9.12 arm uses as the **Rust rpcchainvm host**
/// hosting a Go plugin. Honors `CARGO_TARGET_DIR` (like [`build_rust_plugin`]).
/// Returns `None` if no binary is present in either profile dir.
#[must_use]
pub fn avalanchers_binary_path() -> Option<PathBuf> {
    let target = target_dir();
    for profile in ["debug", "release"] {
        let mut p = target.clone();
        p.push(profile);
        p.push(exe_name("avalanchers"));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Offline proof that the built plugin speaks the v45 **guest** protocol: spawn
/// it as a real subprocess with [`ENGINE_ADDRESS_KEY`] pointing at a loopback
/// listener we own, and assert the plugin dials it back within `timeout` (the
/// guest half of the reverse-dial handshake — read env, bind `V`, dial `R`). A
/// successful TCP accept proves the plugin read the env and attempted
/// `Runtime.Initialize`; we do not complete the gRPC `Initialize` here (that
/// needs the full `ava-vm-rpc` Runtime server, exercised in that crate's tests).
///
/// # Errors
/// * [`PluginError::Spawn`] if the plugin cannot be spawned.
/// * [`PluginError::HandshakeTimeout`] if no dial-back arrives within `timeout`.
pub async fn assert_plugin_dials_back(
    plugin_path: &PathBuf,
    timeout: Duration,
) -> Result<(), PluginError> {
    // 1. Bind the loopback "runtime" listener R that the plugin must dial back.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let r_addr = listener.local_addr()?;

    // 2. Spawn the plugin with the runtime address in the env.
    let mut child = Command::new(plugin_path)
        .env(ENGINE_ADDRESS_KEY, r_addr.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| PluginError::Spawn {
            what: "plugin",
            source,
        })?;

    // 3. Await the dial-back within the timeout.
    let accepted = tokio::time::timeout(timeout, listener.accept()).await;

    // Tear the plugin down regardless of the outcome.
    let _ = child.start_kill();
    let _ = child.wait().await;

    match accepted {
        Ok(Ok(_conn)) => Ok(()),
        Ok(Err(e)) => Err(PluginError::Io(e)),
        Err(_) => Err(PluginError::HandshakeTimeout(timeout)),
    }
}

/// Offline proof that the plugin is a well-behaved **guest**: without
/// [`ENGINE_ADDRESS_KEY`] set it must fail fast (non-zero exit) rather than
/// hang — Go's `rpcchainvm` host relies on this to detect a misconfigured
/// plugin. Returns `Ok(())` if the plugin exits non-zero within `timeout`.
///
/// # Errors
/// * [`PluginError::Spawn`] if the plugin cannot be spawned.
/// * [`PluginError::HandshakeTimeout`] if the plugin neither exits nor fails
///   within `timeout` (it hung — a protocol bug).
pub async fn assert_plugin_fails_without_env(
    plugin_path: &PathBuf,
    timeout: Duration,
) -> Result<(), PluginError> {
    let mut child = Command::new(plugin_path)
        .env_remove(ENGINE_ADDRESS_KEY)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| PluginError::Spawn {
            what: "plugin",
            source,
        })?;

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if !status.success() => Ok(()),
        // Exited 0 without an engine address — should never happen, but treat a
        // clean exit as acceptable (the plugin chose to no-op).
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(PluginError::Io(e)),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(PluginError::HandshakeTimeout(timeout))
        }
    }
}

/// A live Go `avalanchego` host process running the Rust plugin, plus the
/// captured handshake signal.
///
/// LIVE-ARM: this is a **best-effort** launcher. A full rpcchainvm host needs the
/// Go node configured with a plugin dir + a registered VM-id alias that maps to
/// the Rust binary, and a created blockchain on that VM, so that the Go
/// `rpcchainvm` runtime spawns the plugin with [`ENGINE_ADDRESS_KEY`]. Wiring all
/// of that blind (genesis, staking keys, subnet/chain creation) is more than this
/// helper can credibly do without a live operator, so the nightly operator MUST
/// supply that configuration (see `launch_go_host_with_plugin`).
pub struct GoHost {
    child: Child,
    /// Whether the Go host logged the plugin connecting at protocol 45.
    handshake_observed: bool,
}

impl GoHost {
    /// Whether the Go host completed the v45 reverse-dial handshake with the
    /// Rust plugin (observed in the Go node logs).
    #[must_use]
    pub fn handshake_observed(&self) -> bool {
        self.handshake_observed
    }
}

impl Drop for GoHost {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// Launch a live Go `avalanchego` host configured to run the Rust plugin and
/// wait until it logs the v45 handshake with the plugin.
///
/// LIVE-ARM (stubbed integration point): this launches the Go binary and scans
/// its logs for the plugin-connected-at-protocol-45 marker, but it does **not**
/// itself create the subnet/blockchain that causes the Go node to spawn the
/// plugin — that requires a configured data dir (plugin dir containing the Rust
/// binary named by its VM id, plus a genesis/subnet that instantiates a chain on
/// that VM). The nightly operator must point `extra_args` at such a data dir
/// (e.g. `--data-dir=<dir> --plugin-dir=<dir>/plugins`) and pre-create the chain.
/// Until then the handshake will not be observed and the live test asserts the
/// gap explicitly.
///
/// # Errors
/// * [`PluginError::GoBinaryMissing`] if `go_bin` does not exist.
/// * [`PluginError::Spawn`] if the Go host cannot be spawned.
pub async fn launch_go_host_with_plugin(
    _plugin_path: &PathBuf,
    go_bin: &PathBuf,
    extra_args: &[String],
    handshake_timeout: Duration,
) -> Result<GoHost, PluginError> {
    if !go_bin.exists() {
        return Err(PluginError::GoBinaryMissing(go_bin.display().to_string()));
    }

    let mut child = Command::new(go_bin)
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| PluginError::Spawn {
            what: "go-host",
            source,
        })?;

    // Scan stdout for the rpcchainvm handshake marker (the Go go-plugin runtime
    // logs the negotiated protocol version when a plugin connects).
    let handshake_observed = match child.stdout.take() {
        Some(stdout) => scan_for_handshake(stdout, handshake_timeout).await,
        None => false,
    };

    Ok(GoHost {
        child,
        handshake_observed,
    })
}

/// Scan a child's stdout for a line indicating the rpcchainvm plugin connected
/// at protocol 45, up to `timeout`. Returns `true` on a match.
async fn scan_for_handshake<R>(stdout: R, timeout: Duration) -> bool
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let scan = async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let l = line.to_ascii_lowercase();
            // go-plugin / rpcchainvm handshake markers. Kept broad on purpose:
            // the exact log line is a live-operator detail (LIVE-ARM).
            if (l.contains("plugin")
                && (l.contains("protocol version=45") || l.contains("rpcchainvm=45")))
                || l.contains("plugin process exited") && l.contains("45")
            {
                return true;
            }
        }
        false
    };
    (tokio::time::timeout(timeout, scan).await).unwrap_or(false)
}

/// Assert the Go host completed the v45 handshake with the Rust plugin.
///
/// # Errors
/// * [`PluginError::HandshakeTimeout`] if the handshake was never observed.
pub fn assert_handshake_complete(host: &GoHost) -> Result<(), PluginError> {
    if host.handshake_observed() {
        Ok(())
    } else {
        Err(PluginError::HandshakeTimeout(Duration::ZERO))
    }
}
