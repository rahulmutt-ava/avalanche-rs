// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Two-binary / mixed-net wiring (specs/02 §11.1, §11.4, §11.6).
//!
//! Brings up a network where the i-th slot runs either the reference Go
//! `avalanchego` binary (`$AVALANCHEGO_PATH`) or the Rust `avalanchers` binary,
//! alternating Go/Rust per slot (§11.4), all sharing identical genesis/config and
//! a per-slot identity derived deterministically from the network seed — so the
//! i-th Go and i-th Rust node get the same node-ID/TLS/staking-port and
//! peer-dependent fields match across implementations.
//!
//! [`BinaryMix`] (the pure, seed-derived slot plan + identities) is exercised by
//! the offline CI arm. [`Network::start`] (the live spawner) only runs under the
//! gated `live` arm — it spawns each node with `tokio::process::Command` and
//! exposes per-node HTTP API base URLs; a [`Drop`] / [`Network::shutdown`] kills
//! the children. It is normal (non-`cfg`-gated) code so it always compiles; only
//! the *test* that calls it is feature-gated + `#[ignore]`d.

use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::{Child, Command};

/// Which node implementation a network slot runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    /// The reference avalanchego binary (`$AVALANCHEGO_PATH`).
    Go,
    /// The avalanchers binary under test.
    Rust,
}

/// Deterministic network configuration: identical genesis/config/seed across
/// implementations, with the i-th Go and i-th Rust node assigned the same
/// seed-derived node IDs / TLS certs / staking ports (specs/02 §11.4).
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Seed driving all deterministic node identity derivation.
    pub seed: u64,
    /// Number of node slots in the network.
    pub nodes: u32,
}

impl NetworkConfig {
    /// Build a deterministic config for `nodes` validators from `seed`.
    #[must_use]
    pub fn deterministic(seed: u64, nodes: u32) -> Self {
        Self { seed, nodes }
    }
}

/// The deterministic per-slot identity of a node, derived from
/// `(seed, slot_index)` so the i-th Go and i-th Rust node match (§11.4).
///
/// The byte material here is a *credible deterministic sketch*: it is enough for
/// the offline determinism gate (reproducible-from-seed, distinct-per-slot,
/// seed-sensitive) and for the live spawner to hand each node a stable base port
/// plus a seed string. The live operator wires `node_seed` into the real
/// `staking.NewTLSCertFromSeed`-equivalent and the genesis allocation; see the
/// `Network::start` doc and the M9.15 handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeIdentity {
    /// Slot index within the network.
    pub slot: u32,
    /// The deterministic seed for this slot's TLS/staking-key derivation,
    /// rendered as a stable hex string. The live operator feeds this into the
    /// cert generator so the i-th Go and i-th Rust node share a node ID.
    pub node_seed: String,
    /// The derived node-ID string (a deterministic, masked-in-comparison value).
    pub node_id: String,
    /// The deterministic staking port for this slot.
    pub staking_port: u16,
    /// The deterministic HTTP API port for this slot.
    pub http_port: u16,
}

/// A deterministic Go/Rust slot assignment plus each slot's seed-derived
/// identity, computed purely from a [`NetworkConfig`] (specs/02 §11.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryMix {
    seed: u64,
    slots: Vec<Binary>,
    identities: Vec<NodeIdentity>,
}

/// Base port the deterministic staking ports are laid out above.
const STAKING_PORT_BASE: u16 = 9650;
/// Base port the deterministic HTTP API ports are laid out above.
const HTTP_PORT_BASE: u16 = 9750;

impl BinaryMix {
    /// Derive the slot plan + identities from the config. Alternates Go/Rust
    /// starting at Go for slot 0 (specs/02 §11.4); identities are a pure function
    /// of `(seed, slot)`.
    #[must_use]
    pub fn from_config(cfg: &NetworkConfig) -> BinaryMix {
        let mut slots = Vec::with_capacity(cfg.nodes as usize);
        let mut identities = Vec::with_capacity(cfg.nodes as usize);
        for slot in 0..cfg.nodes {
            // Even slots Go, odd slots Rust — the i-th Go and i-th Rust pair up
            // by sharing the same seed-derived identity below.
            slots.push(if slot % 2 == 0 {
                Binary::Go
            } else {
                Binary::Rust
            });
            identities.push(derive_identity(cfg.seed, slot));
        }
        BinaryMix {
            seed: cfg.seed,
            slots,
            identities,
        }
    }

    /// The network seed this mix was derived from.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The number of slots (== `cfg.nodes`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the mix has no slots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The per-slot Go/Rust assignment.
    #[must_use]
    pub fn slots(&self) -> &[Binary] {
        &self.slots
    }

    /// The deterministic identity of slot `i`, or `None` if out of range.
    #[must_use]
    pub fn try_node_identity(&self, i: usize) -> Option<&NodeIdentity> {
        self.identities.get(i)
    }

    /// The deterministic identity of slot `i`.
    ///
    /// # Panics
    /// Panics if `i` is out of range; callers iterate `0..len()`.
    #[must_use]
    pub fn node_identity(&self, i: usize) -> &NodeIdentity {
        self.identities
            .get(i)
            .unwrap_or_else(|| panic!("slot {i} out of range (len {})", self.identities.len()))
    }
}

/// Derive a deterministic per-slot identity from `(seed, slot)`.
///
/// Uses a splitmix64 step over `seed ^ rotate(slot)` so identities are
/// reproducible from the seed, distinct per slot, and seed-sensitive — without
/// pulling in an RNG crate. Ports are laid out deterministically above the base.
fn derive_identity(seed: u64, slot: u32) -> NodeIdentity {
    let mixed =
        splitmix64(seed ^ (u64::from(slot).rotate_left(32)).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let node_seed = format!("{mixed:016x}");
    // A stable, recognizable node-id string for the offline gate; the live
    // operator replaces this with the real cert-derived NodeID-<cb58>.
    let node_id = format!("NodeID-seed-{node_seed}");
    let staking_port = STAKING_PORT_BASE.wrapping_add(slot as u16);
    let http_port = HTTP_PORT_BASE.wrapping_add(slot as u16);
    NodeIdentity {
        slot,
        node_seed,
        node_id,
        staking_port,
        http_port,
    }
}

/// A single splitmix64 finalization step (deterministic, no external RNG dep).
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Errors raised while bringing up or tearing down a mixed network.
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    /// `$AVALANCHEGO_PATH` is required for the Go slots but was not set.
    #[error("AVALANCHEGO_PATH unset — required for Go slots")]
    GoBinaryMissing,
    /// The Rust `avalanchers` binary could not be located.
    #[error("avalanchers binary not found: {0}")]
    RustBinaryMissing(String),
    /// A node process failed to spawn.
    #[error("spawning slot {slot} ({binary:?}): {source}")]
    Spawn {
        /// The slot that failed to spawn.
        slot: u32,
        /// Which implementation the slot runs.
        binary: Binary,
        /// The underlying spawn error.
        source: std::io::Error,
    },
    /// A timeout elapsed waiting for the network to converge.
    #[error("timed out waiting for the network: {0}")]
    Timeout(String),
    /// The local staker cert/key source could not be resolved.
    #[error("local staker cert source: {0}")]
    CertSource(String),
}

/// A single running node in the mixed network.
pub struct Node {
    /// The slot's deterministic identity.
    pub identity: NodeIdentity,
    /// Which implementation this node runs.
    pub binary: Binary,
    /// The node's HTTP API base, e.g. `http://127.0.0.1:9750`.
    pub api_base: String,
    /// Path to this node's stderr/stdout log file (for the version-log scan).
    pub log_path: PathBuf,
    /// The spawned child process.
    child: Child,
}

/// A running mixed Go+Rust network.
///
/// Constructed by [`Network::start`] (live arm only). Owns the child processes;
/// dropping it — or calling [`Network::shutdown`] — kills them.
pub struct Network {
    nodes: Vec<Node>,
    /// The working directory holding per-node data + log files.
    work_dir: PathBuf,
}

impl Network {
    /// Bring up the mixed network: spawn each slot with its implementation's
    /// binary, identical genesis/config, and the slot's seed-derived identity.
    ///
    /// This is the **live** path — it is non-`cfg`-gated (so it always compiles)
    /// but is only invoked by the `#[cfg(feature = "live")]` + `#[ignore]`d
    /// `mixed_network_bringup_smoke` test. It is *not* exercised in CI / this
    /// sandbox (tmpnet multi-node bring-up is heavy).
    ///
    /// ## What the live operator must complete
    /// The genesis allocation, the real TLS/staking-key derivation from
    /// [`NodeIdentity::node_seed`] (so the i-th Go and i-th Rust node share a
    /// node ID), and the bootstrap-IP wiring are sketched here as a single shared
    /// `work_dir` + per-slot `--http-port`/`--staking-port` + `--network-id` flags.
    /// A nightly operator extends `spawn_node` with the full genesis + cert paths
    /// (see the M9.15 handoff). The structure — deterministic identities, the
    /// Go/Rust binary selection, child ownership/teardown, and the per-node API
    /// base — is real and tested by the offline arm.
    ///
    /// # Errors
    /// Returns [`NetworkError`] if a required binary is missing or a node fails
    /// to spawn.
    pub async fn start(mix: BinaryMix, cfg: &NetworkConfig) -> Result<Network, NetworkError> {
        let go_path =
            std::env::var("AVALANCHEGO_PATH").map_err(|_| NetworkError::GoBinaryMissing)?;
        let rust_path = locate_rust_binary()?;

        let work_dir = std::env::temp_dir().join(format!("mixed-net-{}", cfg.seed));
        // Best-effort: create the shared working dir.
        let _ = std::fs::create_dir_all(&work_dir);

        let mut nodes = Vec::with_capacity(mix.len());
        for (i, &binary) in mix.slots().iter().enumerate() {
            let identity = mix.node_identity(i).clone();
            let bin = match binary {
                Binary::Go => go_path.clone(),
                Binary::Rust => rust_path.clone(),
            };
            let node = spawn_node(&bin, binary, identity, &work_dir)?;
            nodes.push(node);
        }

        Ok(Network { nodes, work_dir })
    }

    /// The running nodes (per-node identity, binary, API base, log path).
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Wait until every node reports connected peers (handshakes complete /
    /// PeerLists exchanged), polling `info.peers` over each node's API.
    ///
    /// # Errors
    /// Returns [`NetworkError::Timeout`] if not all nodes connect within
    /// `within`.
    pub async fn await_all_connected(
        &self,
        within: std::time::Duration,
    ) -> Result<(), NetworkError> {
        let deadline = tokio::time::Instant::now()
            .checked_add(within)
            .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
        let want = self.nodes.len().saturating_sub(1);
        loop {
            let mut all_ok = true;
            for node in &self.nodes {
                let peers = crate::observation::Observation::collect(&node.api_base)
                    .await
                    .ok()
                    .map(|o| o.fields.len())
                    .unwrap_or(0);
                if peers < want {
                    all_ok = false;
                    break;
                }
            }
            if all_ok {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(NetworkError::Timeout(format!(
                    "only some of {} nodes connected",
                    self.nodes.len()
                )));
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Scan a Go node's log file for the Rust peer's reported version string
    /// (specs/26 §9(4)); returns whether any Go node logged `version`.
    ///
    /// # Errors
    /// Returns [`NetworkError`] only on a non-recoverable filesystem error;
    /// a missing/empty log is reported as "not logged" (`Ok(false)`).
    pub async fn go_node_logged_peer_version(&self, version: &str) -> Result<bool, NetworkError> {
        for node in &self.nodes {
            if node.binary != Binary::Go {
                continue;
            }
            if let Ok(contents) = std::fs::read_to_string(&node.log_path)
                && contents.contains(version)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Kill every child and drop the network.
    pub async fn shutdown(mut self) {
        for node in &mut self.nodes {
            let _ = node.child.start_kill();
        }
        for node in &mut self.nodes {
            let _ = node.child.wait().await;
        }
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        // Best-effort kill on drop so a panicking test never leaks node
        // processes (`shutdown` is the graceful path).
        for node in &mut self.nodes {
            let _ = node.child.start_kill();
        }
    }
}

/// Spawn one node with its implementation's binary and the slot's identity.
fn spawn_node(
    bin: &str,
    binary: Binary,
    identity: NodeIdentity,
    work_dir: &std::path::Path,
) -> Result<Node, NetworkError> {
    let data_dir = work_dir.join(format!("slot-{}", identity.slot));
    let _ = std::fs::create_dir_all(&data_dir);
    let log_path = data_dir.join("node.log");

    // Open the log file for the child's stdout+stderr (so the version-log scan
    // has a real file to read in the live arm).
    let log = std::fs::File::create(&log_path).map_err(|source| NetworkError::Spawn {
        slot: identity.slot,
        binary,
        source,
    })?;
    let log_err = log.try_clone().map_err(|source| NetworkError::Spawn {
        slot: identity.slot,
        binary,
        source,
    })?;

    let mut cmd = Command::new(bin);
    cmd.arg(format!("--http-port={}", identity.http_port))
        .arg(format!("--staking-port={}", identity.staking_port))
        .arg(format!("--data-dir={}", data_dir.display()))
        // A single-network local run; the live operator supplies the full
        // genesis + bootstrap-IP set keyed off the deterministic identities.
        .arg("--network-id=local")
        .arg(format!("--staking-tls-cert-seed={}", identity.node_seed))
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let child = cmd.spawn().map_err(|source| NetworkError::Spawn {
        slot: identity.slot,
        binary,
        source,
    })?;

    let api_base = format!("http://127.0.0.1:{}", identity.http_port);
    Ok(Node {
        identity,
        binary,
        api_base,
        log_path,
        child,
    })
}

/// Locate the built Rust `avalanchers` binary.
///
/// Honors `$AVALANCHERS_PATH`; otherwise falls back to the conventional Cargo
/// target locations relative to this crate.
fn locate_rust_binary() -> Result<String, NetworkError> {
    if let Ok(path) = std::env::var("AVALANCHERS_PATH") {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(NetworkError::RustBinaryMissing(path));
    }
    for candidate in [
        "target/release/avalanchers",
        "target/debug/avalanchers",
        "../../target/release/avalanchers",
        "../../target/debug/avalanchers",
    ] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_owned());
        }
    }
    Err(NetworkError::RustBinaryMissing(
        "set $AVALANCHERS_PATH or build `avalanchers`".to_owned(),
    ))
}

/// Resolve the configured Go `avalanchego` binary path, or [`NetworkError::GoBinaryMissing`].
fn resolve_go_binary(configured: Option<String>) -> Result<String, NetworkError> {
    configured.ok_or(NetworkError::GoBinaryMissing)
}

/// Spawn one node with an explicit role/cert/bootstrap launch spec.
fn spawn_role_node(
    bin: &str,
    binary: Binary,
    slot: u32,
    launch: &crate::livenet::NodeLaunch,
) -> Result<Node, NetworkError> {
    let _ = std::fs::create_dir_all(&launch.data_dir);
    let log_path = launch.data_dir.join("node.log");
    let log = std::fs::File::create(&log_path).map_err(|source| NetworkError::Spawn {
        slot,
        binary,
        source,
    })?;
    let log_err = log.try_clone().map_err(|source| NetworkError::Spawn {
        slot,
        binary,
        source,
    })?;
    let mut cmd = Command::new(bin);
    cmd.args(crate::livenet::node_args(launch))
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|source| NetworkError::Spawn {
        slot,
        binary,
        source,
    })?;
    let identity = NodeIdentity {
        slot,
        node_seed: String::new(),
        node_id: String::new(),
        staking_port: launch.staking_port,
        http_port: launch.http_port,
    };
    Ok(Node {
        identity,
        binary,
        api_base: format!("http://127.0.0.1:{}", launch.http_port),
        log_path,
        child,
    })
}

/// Tail the last `n` lines of a node log into a string (for timeout diagnostics).
fn log_tail(path: &std::path::Path, n: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let lines: Vec<&str> = s.lines().collect();
            let start = lines.len().saturating_sub(n);
            lines.get(start..).unwrap_or(&[]).join("\n")
        }
        Err(_) => String::new(),
    }
}

impl Network {
    /// Boot a live Go-beacon + Rust-follower mixed net (M9.15).
    ///
    /// The Go node is the sole staked validator (beacon); the Rust node
    /// bootstraps from it and follows. Returns a [`Network`] whose
    /// `nodes()[0]` is the Go beacon and `nodes()[1]` is the Rust follower.
    ///
    /// Requires `$AVALANCHEGO_PATH` (the Go binary) and a built `avalanchers`
    /// binary (located via `$AVALANCHERS_PATH` or the conventional target path).
    /// Also requires the local staker cert/key pairs under
    /// `$AVALANCHEGO_SRC/staking/local/` (default `~/avalanchego`).
    ///
    /// # Errors
    /// - [`NetworkError::GoBinaryMissing`] if `$AVALANCHEGO_PATH` is unset.
    /// - [`NetworkError::Timeout`] if the oracle binary pre-gate fails, a
    ///   node-ID scrape times out, or the Rust node does not bootstrap P/X/C
    ///   within the allowed window.
    /// - [`NetworkError::CertSource`] if the local staker certs are missing.
    /// - [`NetworkError::Spawn`] if a node process fails to start.
    /// - [`NetworkError::RustBinaryMissing`] if the `avalanchers` binary is
    ///   not found.
    pub async fn boot_mixed(seed: u64) -> Result<Network, NetworkError> {
        let go_path = resolve_go_binary(std::env::var("AVALANCHEGO_PATH").ok())?;
        let rust_path = locate_rust_binary()?;

        // Pre-gate: binary commit must match the ~/avalanchego checkout (rpcchainvm=45).
        // Resolve the script absolutely — cargo runs tests with CWD at the package dir,
        // not the workspace root, so a relative path would silently miss.
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/check_oracle_binary.sh");
        let status = std::process::Command::new(&script).status().map_err(|e| {
            NetworkError::Timeout(format!(
                "check_oracle_binary.sh not runnable ({}): {e}",
                script.display()
            ))
        })?;
        if !status.success() {
            return Err(NetworkError::Timeout(
                "check_oracle_binary.sh failed — rebuild ~/avalanchego (stale binary)".to_owned(),
            ));
        }

        let work_dir = std::env::temp_dir().join(format!("mixed-net-{seed}"));
        let _ = std::fs::create_dir_all(&work_dir);
        let ports = crate::livenet::free_ports(4)
            .map_err(|e| NetworkError::Timeout(format!("free_ports: {e}")))?;
        // Extract the four ports by position — free_ports(4) always returns exactly 4.
        let go_http = ports
            .first()
            .copied()
            .ok_or_else(|| NetworkError::Timeout("free_ports returned < 1 port".to_owned()))?;
        let go_staking = ports
            .get(1)
            .copied()
            .ok_or_else(|| NetworkError::Timeout("free_ports returned < 2 ports".to_owned()))?;
        let rust_http = ports
            .get(2)
            .copied()
            .ok_or_else(|| NetworkError::Timeout("free_ports returned < 3 ports".to_owned()))?;
        let rust_staking = ports
            .get(3)
            .copied()
            .ok_or_else(|| NetworkError::Timeout("free_ports returned < 4 ports".to_owned()))?;
        let go_staker = crate::livenet::local_staker(1)?;
        let rust_staker = crate::livenet::local_staker(2)?;

        // 1. Go beacon (no bootstrap peers — it is the genesis validator).
        let go_launch = crate::livenet::NodeLaunch {
            http_port: go_http,
            staking_port: go_staking,
            data_dir: work_dir.join("go"),
            cert_file: go_staker.cert,
            key_file: go_staker.key,
            bootstrap: None,
        };
        let go_node = spawn_role_node(&go_path, Binary::Go, 0, &go_launch)?;

        // 2. Scrape the Go node-ID (it must answer info.getNodeID before we wire bootstrap).
        let go_id = {
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(60))
                .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
            loop {
                if let Ok(id) = crate::livenet::scrape_node_id(&go_node.api_base).await {
                    break id;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(NetworkError::Timeout(format!(
                        "Go beacon never answered info.getNodeID:\n{}",
                        log_tail(&go_node.log_path, 40)
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        };

        // 3. Rust follower, bootstrapped from the Go beacon.
        let rust_launch = crate::livenet::NodeLaunch {
            http_port: rust_http,
            staking_port: rust_staking,
            data_dir: work_dir.join("rust"),
            cert_file: rust_staker.cert,
            key_file: rust_staker.key,
            bootstrap: Some(crate::livenet::Bootstrap {
                ip: format!("127.0.0.1:{go_staking}"),
                id: go_id,
            }),
        };
        let rust_node = spawn_role_node(&rust_path, Binary::Rust, 1, &rust_launch)?;

        let net = Network {
            nodes: vec![go_node, rust_node],
            work_dir,
        };

        // 4. Wait for the Rust follower to bootstrap P/X/C from Go.
        // nodes[1] is the Rust follower — we just pushed it at index 1 above.
        let rust_node_ref = net
            .nodes
            .get(1)
            .ok_or_else(|| NetworkError::Timeout("rust follower missing".to_owned()))?;
        let rust_api = rust_node_ref.api_base.clone();
        let rust_log = rust_node_ref.log_path.clone();
        crate::livenet::await_bootstrapped(
            &rust_api,
            &["P", "X", "C"],
            std::time::Duration::from_secs(180),
        )
        .await
        .map_err(|e| {
            NetworkError::Timeout(format!("{e}\nrust log:\n{}", log_tail(&rust_log, 60)))
        })?;

        Ok(net)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_go_binary_missing_is_go_binary_missing() {
        assert!(
            matches!(resolve_go_binary(None), Err(NetworkError::GoBinaryMissing)),
            "no configured Go binary must yield GoBinaryMissing"
        );
        assert!(
            resolve_go_binary(Some("/bin/x".to_owned())).is_ok(),
            "a configured path resolves"
        );
    }
}
