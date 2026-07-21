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

/// Send SIGTERM to a child by pid (best-effort; lets the process flush logs
/// before the SIGKILL backstop). Test-harness only.
///
/// Uses the system `kill` binary to avoid `unsafe` in this crate
/// (`#![forbid(unsafe_code)]`). Errors are intentionally swallowed — this is a
/// best-effort graceful-flush attempt before the SIGKILL backstop fires.
fn sigterm(child: &Child) {
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
    }
}

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

    /// The Go beacon (staker1) — the first node booted. `None` if empty.
    #[must_use]
    pub fn go_beacon(&self) -> Option<&Node> {
        self.nodes.first()
    }

    /// The Rust follower — the last node booted. `None` if empty.
    #[must_use]
    pub fn rust_follower(&self) -> Option<&Node> {
        self.nodes.last()
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
            // Per-node diagnosis of the most recent poll, so the timeout error
            // names the node and the failing call instead of a bare count.
            let mut lagging: Vec<String> = Vec::new();
            for node in &self.nodes {
                let status = match crate::observation::Observation::collect(&node.api_base).await {
                    Ok(o) if o.fields.len() >= want => continue,
                    Ok(o) => format!("{} fields {:?}", o.fields.len(), o.fields),
                    Err(e) => format!("collect error: {e}"),
                };
                lagging.push(format!("{} ({:?}): {status}", node.api_base, node.binary));
            }
            if lagging.is_empty() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(NetworkError::Timeout(format!(
                    "only some of {} nodes connected; lagging: [{}]",
                    self.nodes.len(),
                    lagging.join("; ")
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
    ///
    /// Teardown sequence: SIGTERM all nodes → sleep 3 s (lets tracing_appender
    /// WorkerGuard flush) → SIGKILL backstop → wait.
    pub async fn shutdown(mut self) {
        // SIGTERM all nodes first so they can flush their log sinks.
        for node in &mut self.nodes {
            sigterm(&node.child);
        }
        // Give all nodes a moment to flush and exit cleanly.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        // SIGKILL backstop for any nodes still alive.
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
        // Graceful teardown: SIGTERM all nodes so they can flush their log
        // sinks (tracing_appender WorkerGuard), sleep 3 s, then SIGKILL any
        // still alive. This ensures the Rust follower's diagnostic logs survive
        // a panicking test. `shutdown` is the preferred async path; Drop is the
        // sync backstop.
        for node in &mut self.nodes {
            sigterm(&node.child);
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
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

/// Among `candidates`, the existing path with the newest modification time;
/// earlier candidates win mtime ties (so the caller's preference order is the
/// tie-breaker). `None` when no candidate exists.
///
/// Guards the live arms against silently running a STALE binary: `test-live`
/// rebuilds `target/release/avalanchers`, but direct `cargo nextest`
/// invocations of the live tests do not — a fixed release-first preference
/// once picked a release binary built a day before the ava-logging chain-slot
/// Interest fixes, reproducing an already-fixed "zero native tracing events"
/// defect in every captured node log (M9.15, 2026-07-14).
fn newest_existing(candidates: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::time::SystemTime, &std::path::PathBuf)> = None;
    for candidate in candidates {
        let Ok(meta) = std::fs::metadata(candidate) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        // Strictly-newer only: on a tie the earlier (preferred) candidate stays.
        if best.is_none_or(|(best_mtime, _)| mtime > best_mtime) {
            best = Some((mtime, candidate));
        }
    }
    best.map(|(_, path)| path.clone())
}

/// Locate the built Rust `avalanchers` binary.
///
/// Honors `$AVALANCHERS_PATH` (explicit override, taken verbatim); otherwise
/// picks the NEWEST (by mtime) of the conventional Cargo target locations
/// relative to this crate — never a stale sibling — and prints the choice so
/// captured live-run output pins which binary actually ran.
fn locate_rust_binary() -> Result<String, NetworkError> {
    if let Ok(path) = std::env::var("AVALANCHERS_PATH") {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(NetworkError::RustBinaryMissing(path));
    }
    let candidates: Vec<std::path::PathBuf> = [
        "target/release/avalanchers",
        "target/debug/avalanchers",
        "../../target/release/avalanchers",
        "../../target/debug/avalanchers",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect();
    if let Some(chosen) = newest_existing(&candidates) {
        let age = std::fs::metadata(&chosen)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| std::time::SystemTime::now().duration_since(mtime).ok());
        eprintln!(
            "locate_rust_binary: {} (built {} ago)",
            chosen.display(),
            age.map_or_else(|| "<unknown>".to_owned(), |d| format!("{}s", d.as_secs())),
        );
        let path = chosen.to_string_lossy().into_owned();
        prewarm_binary(&path);
        return Ok(path);
    }
    Err(NetworkError::RustBinaryMissing(
        "set $AVALANCHERS_PATH or build `avalanchers`".to_owned(),
    ))
}

/// Eat macOS's first-exec system scan of a freshly linked binary.
///
/// The first exec after every relink stalls tens of seconds with zero CPU
/// (Gatekeeper/XProtect + unified-logging registration), which under a
/// six-node boot exceeds the bootstrap window and yields an empty node log
/// (observed live 2026-07-15: 37s solo, >180s under load). One synchronous
/// `--version` here makes every subsequent spawn instant. Best-effort: a
/// failure just means the spawn path pays the stall instead.
fn prewarm_binary(path: &str) {
    let start = std::time::Instant::now();
    let status = std::process::Command::new(path)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    eprintln!(
        "prewarm_binary: {path} ({}s, {})",
        start.elapsed().as_secs(),
        status.map_or_else(|e| format!("spawn error: {e}"), |s| s.to_string()),
    );
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
///
/// `path` is the captured stdout/stderr file (`<data_dir>/node.log`), which holds
/// the early pre-logger boot errors. Once a node initializes its file logger it
/// writes to `<data_dir>/logs/main.log` instead, so we tail that too — otherwise
/// a node that fails *after* logger init folds an empty tail into the error.
fn log_tail(path: &std::path::Path, n: usize) -> String {
    let mut out = tail_file(path, n);
    if let Some(data_dir) = path.parent() {
        let main_log = data_dir.join("logs").join("main.log");
        let main_tail = tail_file(&main_log, n);
        if !main_tail.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("--- logs/main.log ---\n");
            out.push_str(&main_tail);
        }
    }
    out
}

/// Tail the last `n` lines of a single file, or `""` if it cannot be read.
fn tail_file(path: &std::path::Path, n: usize) -> String {
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

        // 12 ports up front: 5 Go validators × (http, staking) + 1 Rust × (http, staking).
        let ports = crate::livenet::free_ports(12)
            .map_err(|e| NetworkError::Timeout(format!("free_ports: {e}")))?;

        // Build the 5 Go validator slots. Validator i uses stakerI's RSA
        // cert/key (Go loads RSA natively) and the i-th well-known local NodeID,
        // so the full bootstrap mesh can be wired before any node is spawned.
        let mut go_validators: Vec<crate::livenet::GoValidator> = Vec::with_capacity(5);
        let mut go_launches: Vec<crate::livenet::NodeLaunch> = Vec::with_capacity(5);
        for i in 0..5usize {
            let port_http = i
                .checked_mul(2)
                .ok_or_else(|| NetworkError::Timeout("port index overflow".to_owned()))?;
            let port_staking = port_http
                .checked_add(1)
                .ok_or_else(|| NetworkError::Timeout("port index overflow".to_owned()))?;
            let http = *ports
                .get(port_http)
                .ok_or_else(|| NetworkError::Timeout("missing go http port".to_owned()))?;
            let staking = *ports
                .get(port_staking)
                .ok_or_else(|| NetworkError::Timeout("missing go staking port".to_owned()))?;
            // local_staker is 1-indexed (staker1..staker5).
            let i1 = i
                .checked_add(1)
                .ok_or_else(|| NetworkError::Timeout("staker index overflow".to_owned()))?;
            let idx = u8::try_from(i1)
                .map_err(|_| NetworkError::Timeout("staker index overflow".to_owned()))?;
            let staker = crate::livenet::local_staker(idx)?;
            let node_id = crate::livenet::LOCAL_VALIDATOR_NODE_IDS
                .get(i)
                .ok_or_else(|| NetworkError::Timeout("missing local validator id".to_owned()))?;
            go_validators.push(crate::livenet::GoValidator {
                ip: format!("127.0.0.1:{staking}"),
                id: (*node_id).to_owned(),
            });
            go_launches.push(crate::livenet::NodeLaunch {
                http_port: http,
                staking_port: staking,
                data_dir: work_dir.join(format!("go{i1}")),
                cert_file: staker.cert,
                key_file: staker.key,
                bootstrap: Vec::new(), // filled below once the full set is known
                signer_key_file: Some(crate::livenet::local_signer_key(idx)?),
                extra_args: Vec::new(),
            });
        }
        // Each Go node bootstraps from the other four (full mesh ⇒ quorum).
        for (i, launch) in go_launches.iter_mut().enumerate() {
            launch.bootstrap = crate::livenet::mesh_peers(&go_validators, i);
        }

        // Spawn all 5 Go validators.
        let mut nodes: Vec<Node> = Vec::with_capacity(6);
        for (i, launch) in go_launches.iter().enumerate() {
            let slot = u32::try_from(i)
                .map_err(|_| NetworkError::Timeout("go slot overflow".to_owned()))?;
            nodes.push(spawn_role_node(&go_path, Binary::Go, slot, launch)?);
        }

        // The beacon (staker1) is node 0; everything keys off its API.
        let beacon_api = nodes
            .first()
            .ok_or_else(|| NetworkError::Timeout("go beacon missing".to_owned()))?
            .api_base
            .clone();
        let beacon_log = nodes
            .first()
            .ok_or_else(|| NetworkError::Timeout("go beacon missing".to_owned()))?
            .log_path
            .clone();

        // Sanity: the scraped beacon NodeID must match the vendored table head
        // (catches a wrong cert↔id mapping early).
        {
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(60))
                .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
            loop {
                if let Ok(id) = crate::livenet::scrape_node_id(&beacon_api).await {
                    if id != crate::livenet::LOCAL_VALIDATOR_NODE_IDS[0] {
                        return Err(NetworkError::Timeout(format!(
                            "beacon NodeID {id} != vendored staker1 {}",
                            crate::livenet::LOCAL_VALIDATOR_NODE_IDS[0]
                        )));
                    }
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(NetworkError::Timeout(format!(
                        "Go beacon never answered info.getNodeID:\n{}",
                        log_tail(&beacon_log, 40)
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }

        // STAGE 1: the 5-validator Go cluster must reach quorum and bootstrap
        // P/X/C before it can serve a frontier to the follower.
        crate::livenet::await_bootstrapped(
            &beacon_api,
            &["P", "X", "C"],
            std::time::Duration::from_secs(240),
        )
        .await
        .map_err(|e| {
            NetworkError::Timeout(format!(
                "Go cluster did not bootstrap (quorum not reached?):\n{e}\ngo beacon log:\n{}",
                log_tail(&beacon_log, 60)
            ))
        })?;

        // Rust follower: a fresh ECDSA cert (avalanchers rejects the RSA local
        // stakers — M9.15 gap), 0 weight, bootstrapping from ALL 5 Go validators.
        let rust_http = *ports
            .get(10)
            .ok_or_else(|| NetworkError::Timeout("missing rust http port".to_owned()))?;
        let rust_staking = *ports
            .get(11)
            .ok_or_else(|| NetworkError::Timeout("missing rust staking port".to_owned()))?;
        let rust_staker = crate::livenet::generate_staker(&work_dir, "rust-staker")?;
        let rust_launch = crate::livenet::NodeLaunch {
            http_port: rust_http,
            staking_port: rust_staking,
            data_dir: work_dir.join("rust"),
            cert_file: rust_staker.cert,
            key_file: rust_staker.key,
            bootstrap: crate::livenet::mesh_peers(&go_validators, usize::MAX),
            signer_key_file: None,
            extra_args: Vec::new(),
        };
        nodes.push(spawn_role_node(&rust_path, Binary::Rust, 5, &rust_launch)?);

        let net = Network { nodes, work_dir };

        // STAGE 2: the Rust follower bootstraps P/X/C from the Go cluster.
        let rust_node_ref = net
            .rust_follower()
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

    /// Boot a live 4-Go + 1-Rust **validator** net (M9.15 Task 8).
    ///
    /// Unlike [`Network::boot_mixed`] (5 Go validators + a 0-weight Rust
    /// follower), here the Rust node is a full **staked validator** — staker5,
    /// equal weight with the four Go stakers. With equal stake the Rust node
    /// proposes ~20% of blocks; the 4-Go quorum finalizes even if it misbehaves,
    /// so a bad Rust block is *observable* (a fork / stalled tip) rather than
    /// wedging the net. `nodes()[0..4]` are the Go validators (stakers 1-4);
    /// `nodes().last()` is the Rust validator (staker5).
    ///
    /// Deltas from [`Network::boot_mixed`]:
    /// - 4 Go validators = stakers 1..=4 (was 1..=5), full-mesh bootstrap.
    /// - Rust node = staker5: RSA cert/key via [`crate::livenet::local_staker`]`(5)`
    ///   (Task 7 made the RSA local stakers loadable by `avalanchers`) and its
    ///   genesis BLS key via [`crate::livenet::local_signer_key`]`(5)` passed as
    ///   `signer_key_file` (was `None`) so its proof-of-possession matches the
    ///   genesis-registered PoP; it bootstraps from the four Go validators.
    ///   NodeID asserted == `LOCAL_VALIDATOR_NODE_IDS[4]`.
    /// - All five slots are genesis validators in one full mesh, spawned up front
    ///   so equal-stake quorum forms with the Rust node participating (not added
    ///   after the Go cluster is already up, as the 0-weight follower is).
    ///
    /// # Errors
    /// Same shape as [`Network::boot_mixed`]: `GoBinaryMissing`, `Timeout`
    /// (oracle pre-gate / node-ID scrape / bootstrap), `CertSource`, `Spawn`,
    /// `RustBinaryMissing`.
    pub async fn boot_mixed_rust_validator(seed: u64) -> Result<Network, NetworkError> {
        let go_path = resolve_go_binary(std::env::var("AVALANCHEGO_PATH").ok())?;
        let rust_path = locate_rust_binary()?;

        // Pre-gate: binary commit must match the ~/avalanchego checkout (rpcchainvm=45).
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

        let work_dir = std::env::temp_dir().join(format!("rust-validator-net-{seed}"));
        let _ = std::fs::create_dir_all(&work_dir);

        // 10 ports: 5 validators × (http, staking).
        let ports = crate::livenet::free_ports(10)
            .map_err(|e| NetworkError::Timeout(format!("free_ports: {e}")))?;

        // Build the 5 validator slots. Slot i uses stakerI's RSA cert/key + BLS
        // signer key and the i-th well-known local NodeID; slots 0-3 run Go,
        // slot 4 (staker5) runs Rust. All five join one full mesh, wired before
        // any node spawns.
        let mut validators: Vec<crate::livenet::GoValidator> = Vec::with_capacity(5);
        let mut launches: Vec<crate::livenet::NodeLaunch> = Vec::with_capacity(5);
        for i in 0..5usize {
            let port_http = i
                .checked_mul(2)
                .ok_or_else(|| NetworkError::Timeout("port index overflow".to_owned()))?;
            let port_staking = port_http
                .checked_add(1)
                .ok_or_else(|| NetworkError::Timeout("port index overflow".to_owned()))?;
            let http = *ports
                .get(port_http)
                .ok_or_else(|| NetworkError::Timeout("missing http port".to_owned()))?;
            let staking = *ports
                .get(port_staking)
                .ok_or_else(|| NetworkError::Timeout("missing staking port".to_owned()))?;
            // local_staker / local_signer_key are 1-indexed (staker1..staker5).
            let i1 = i
                .checked_add(1)
                .ok_or_else(|| NetworkError::Timeout("staker index overflow".to_owned()))?;
            let idx = u8::try_from(i1)
                .map_err(|_| NetworkError::Timeout("staker index overflow".to_owned()))?;
            let staker = crate::livenet::local_staker(idx)?;
            let node_id = crate::livenet::LOCAL_VALIDATOR_NODE_IDS
                .get(i)
                .ok_or_else(|| NetworkError::Timeout("missing local validator id".to_owned()))?;
            validators.push(crate::livenet::GoValidator {
                ip: format!("127.0.0.1:{staking}"),
                id: (*node_id).to_owned(),
            });
            launches.push(crate::livenet::NodeLaunch {
                http_port: http,
                staking_port: staking,
                data_dir: work_dir.join(format!("staker{i1}")),
                cert_file: staker.cert,
                key_file: staker.key,
                bootstrap: Vec::new(), // filled below once the full set is known
                // Every slot is a genesis validator: it MUST present its
                // genesis BLS signer key so its PoP matches genesis, else peers
                // reject the signed-IP BLS signature. This is the delta that
                // makes the Rust node (slot 4) a real staker vs the 0-weight
                // follower in `boot_mixed`.
                signer_key_file: Some(crate::livenet::local_signer_key(idx)?),
                // Go slots (0-3) get `--index-enabled=true` so the T16 live
                // tx-gossip / `mixed_network_rust_proposes` detection can query
                // a Go node's `/ext/index/C/block` API
                // (`livenet::proposer_of_accepted_container`) for the verified
                // proposer of an accepted C-chain block. The Rust slot (4) does
                // not need it — index queries only ever target a Go node.
                extra_args: if i == 4 {
                    Vec::new()
                } else {
                    vec!["--index-enabled=true".to_owned()]
                },
            });
        }
        // Full mesh: each validator bootstraps from the other four (incl. the
        // Rust node), so equal-stake quorum can form.
        for (i, launch) in launches.iter_mut().enumerate() {
            launch.bootstrap = crate::livenet::mesh_peers(&validators, i);
        }

        // Spawn all five up front: slots 0-3 Go, slot 4 (staker5) Rust.
        let mut nodes: Vec<Node> = Vec::with_capacity(5);
        for (i, launch) in launches.iter().enumerate() {
            let slot =
                u32::try_from(i).map_err(|_| NetworkError::Timeout("slot overflow".to_owned()))?;
            let (bin, kind) = if i == 4 {
                (rust_path.as_str(), Binary::Rust)
            } else {
                (go_path.as_str(), Binary::Go)
            };
            nodes.push(spawn_role_node(bin, kind, slot, launch)?);
        }

        // staker1 (node 0) is the reference Go validator; everything keys off it.
        let beacon_api = nodes
            .first()
            .ok_or_else(|| NetworkError::Timeout("go staker1 missing".to_owned()))?
            .api_base
            .clone();
        let beacon_log = nodes
            .first()
            .ok_or_else(|| NetworkError::Timeout("go staker1 missing".to_owned()))?
            .log_path
            .clone();

        // Sanity: the scraped staker1 NodeID must match the vendored table head.
        {
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(60))
                .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
            loop {
                if let Ok(id) = crate::livenet::scrape_node_id(&beacon_api).await {
                    if id != crate::livenet::LOCAL_VALIDATOR_NODE_IDS[0] {
                        return Err(NetworkError::Timeout(format!(
                            "staker1 NodeID {id} != vendored staker1 {}",
                            crate::livenet::LOCAL_VALIDATOR_NODE_IDS[0]
                        )));
                    }
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(NetworkError::Timeout(format!(
                        "Go staker1 never answered info.getNodeID:\n{}",
                        log_tail(&beacon_log, 40)
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }

        let net = Network { nodes, work_dir };

        // The Rust node (staker5, last slot) must present the genesis staker5
        // identity: assert its scraped NodeID == LOCAL_VALIDATOR_NODE_IDS[4].
        // A mismatch means the RSA cert did not load as staker5 (Task 7 gate).
        let rust_node = net
            .nodes()
            .last()
            .ok_or_else(|| NetworkError::Timeout("rust validator missing".to_owned()))?;
        let rust_api = rust_node.api_base.clone();
        let rust_log = rust_node.log_path.clone();
        {
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(60))
                .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
            loop {
                if let Ok(id) = crate::livenet::scrape_node_id(&rust_api).await {
                    if id != crate::livenet::LOCAL_VALIDATOR_NODE_IDS[4] {
                        return Err(NetworkError::Timeout(format!(
                            "rust staker5 NodeID {id} != vendored staker5 {} \
                             (RSA cert did not load as staker5?)",
                            crate::livenet::LOCAL_VALIDATOR_NODE_IDS[4]
                        )));
                    }
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(NetworkError::Timeout(format!(
                        "Rust staker5 never answered info.getNodeID:\n{}",
                        log_tail(&rust_log, 60)
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }

        // All five validators (Rust included — it is a validator now and must
        // reach NormalOp) must bootstrap P/X/C. Poll each node's own API so the
        // error names the lagging node.
        for node in net.nodes() {
            let kind = node.binary;
            let api = node.api_base.clone();
            let log = node.log_path.clone();
            crate::livenet::await_bootstrapped(
                &api,
                &["P", "X", "C"],
                std::time::Duration::from_secs(240),
            )
            .await
            .map_err(|e| {
                NetworkError::Timeout(format!(
                    "{kind:?} validator {api} did not bootstrap (quorum not reached?):\n{e}\n\
                     log:\n{}",
                    log_tail(&log, 60)
                ))
            })?;
        }

        Ok(net)
    }

    /// Bisection probe for M9.15 rung-3: one Go validator (self-bootstraps) +
    /// one Rust follower whose sole bootstrap beacon is that Go node. The
    /// follower's connectivity gate threshold collapses to
    /// `required_conns = (3*1 + 3) / 4 = 1`, so a single beacon connection fires
    /// the gate — exercising the engine/frontier path against real Go in
    /// isolation from the 5-validator quorum-connectivity problem.
    pub async fn boot_single_go_beacon(seed: u64) -> Result<Network, NetworkError> {
        let go_path = resolve_go_binary(std::env::var("AVALANCHEGO_PATH").ok())?;
        let rust_path = locate_rust_binary()?;

        // Pre-gate: the Go binary commit must match the ~/avalanchego checkout.
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

        let work_dir = std::env::temp_dir().join(format!("single-beacon-{seed}"));
        let _ = std::fs::create_dir_all(&work_dir);

        // 4 ports: 1 Go validator (http, staking) + 1 Rust follower (http, staking).
        let ports = crate::livenet::free_ports(4)
            .map_err(|e| NetworkError::Timeout(format!("free_ports: {e}")))?;
        let go_http = *ports
            .first()
            .ok_or_else(|| NetworkError::Timeout("go http".to_owned()))?;
        let go_staking = *ports
            .get(1)
            .ok_or_else(|| NetworkError::Timeout("go staking".to_owned()))?;
        let rust_http = *ports
            .get(2)
            .ok_or_else(|| NetworkError::Timeout("rust http".to_owned()))?;
        let rust_staking = *ports
            .get(3)
            .ok_or_else(|| NetworkError::Timeout("rust staking".to_owned()))?;

        // The single Go validator uses staker1's RSA cert/key + BLS signer key and
        // the first well-known local NodeID, and bootstraps from itself (a 1-node
        // cluster self-bootstraps trivially).
        let staker = crate::livenet::local_staker(1)?;
        let go_node_id = crate::livenet::LOCAL_VALIDATOR_NODE_IDS
            .first()
            .ok_or_else(|| NetworkError::Timeout("missing local validator id".to_owned()))?;
        let go_validator = crate::livenet::GoValidator {
            ip: format!("127.0.0.1:{go_staking}"),
            id: (*go_node_id).to_owned(),
        };
        let go_launch = crate::livenet::NodeLaunch {
            http_port: go_http,
            staking_port: go_staking,
            data_dir: work_dir.join("go1"),
            cert_file: staker.cert,
            key_file: staker.key,
            bootstrap: Vec::new(), // a lone genesis validator self-bootstraps
            signer_key_file: Some(crate::livenet::local_signer_key(1)?),
            extra_args: vec!["--sybil-protection-enabled=false".to_owned()],
        };
        let mut nodes: Vec<Node> = Vec::with_capacity(2);
        nodes.push(spawn_role_node(&go_path, Binary::Go, 0, &go_launch)?);

        let beacon_api = nodes
            .first()
            .ok_or_else(|| NetworkError::Timeout("go beacon missing".to_owned()))?
            .api_base
            .clone();
        let beacon_log = nodes
            .first()
            .ok_or_else(|| NetworkError::Timeout("go beacon missing".to_owned()))?
            .log_path
            .clone();

        // STAGE 1: the lone Go validator must bootstrap P/X/C before serving a frontier.
        crate::livenet::await_bootstrapped(
            &beacon_api,
            &["P", "X", "C"],
            std::time::Duration::from_secs(240),
        )
        .await
        .map_err(|e| {
            NetworkError::Timeout(format!(
                "lone Go validator did not bootstrap:\n{e}\ngo log:\n{}",
                log_tail(&beacon_log, 60)
            ))
        })?;

        // The Rust follower: fresh ECDSA cert, non-validating, bootstraps from the one Go node.
        let rust_staker = crate::livenet::generate_staker(&work_dir, "rust-staker")?;
        let rust_launch = crate::livenet::NodeLaunch {
            http_port: rust_http,
            staking_port: rust_staking,
            data_dir: work_dir.join("rust"),
            cert_file: rust_staker.cert,
            key_file: rust_staker.key,
            bootstrap: vec![crate::livenet::Bootstrap {
                id: go_validator.id.clone(),
                ip: go_validator.ip.clone(),
            }],
            signer_key_file: None,
            extra_args: Vec::new(),
        };
        nodes.push(spawn_role_node(&rust_path, Binary::Rust, 1, &rust_launch)?);

        let net = Network { nodes, work_dir };

        // STAGE 2: the Rust follower bootstraps P/X/C from the single Go beacon.
        let rust_node_ref = net
            .rust_follower()
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
            NetworkError::Timeout(format!("{e}\nrust log:\n{}", log_tail(&rust_log, 80)))
        })?;

        Ok(net)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors_on_empty_network_are_none() {
        let net = Network {
            nodes: Vec::new(),
            work_dir: std::path::PathBuf::from("/tmp/empty-net"),
        };
        assert!(net.go_beacon().is_none(), "empty net has no beacon");
        assert!(net.rust_follower().is_none(), "empty net has no follower");
    }

    /// Regression (M9.15): `locate_rust_binary` used to return the FIRST
    /// existing conventional candidate (`target/release` before
    /// `target/debug`), so a stale release binary silently rode every live
    /// run — a 2026-07-14 mixed_network run executed a release binary built
    /// one day BEFORE the chain-slot Interest fixes (f7e2f43/32ff8e8) and
    /// produced the "only rustls log-bridge lines, zero native tracing"
    /// symptom the fixes had already cured. The picker must choose the
    /// NEWEST (by mtime) existing candidate, preferring earlier candidates
    /// on a tie.
    #[test]
    fn newest_existing_picks_freshest_candidate() {
        let dir = std::env::temp_dir().join(format!(
            "ava-newest-existing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let release = dir.join("release-avalanchers");
        let debug = dir.join("debug-avalanchers");
        let missing = dir.join("missing-avalanchers");
        std::fs::write(&release, b"old").expect("write release");
        std::fs::write(&debug, b"new").expect("write debug");

        // release is one hour OLDER than debug.
        let now = std::time::SystemTime::now();
        let hour = std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .append(true)
            .open(&release)
            .expect("open release")
            .set_modified(now - hour)
            .expect("age release");
        std::fs::File::options()
            .append(true)
            .open(&debug)
            .expect("open debug")
            .set_modified(now)
            .expect("touch debug");

        // Stale-preferred order (release first) must still yield the newer debug.
        let picked = newest_existing(&[release.clone(), debug.clone(), missing.clone()])
            .expect("one candidate exists");
        assert_eq!(picked, debug, "the newest existing candidate wins");

        // Tie on mtime → the earlier (preferred) candidate wins.
        std::fs::File::options()
            .append(true)
            .open(&release)
            .expect("reopen release")
            .set_modified(now)
            .expect("tie release");
        let picked = newest_existing(&[release.clone(), debug, missing]).expect("tie pick");
        assert_eq!(picked, release, "ties keep the preference order");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newest_existing_none_when_nothing_exists() {
        assert!(
            newest_existing(&[std::path::PathBuf::from(
                "/nonexistent/ava-test/avalanchers"
            )])
            .is_none(),
            "no existing candidate must yield None"
        );
    }

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
