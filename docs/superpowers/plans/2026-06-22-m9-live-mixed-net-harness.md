# M9.15 Live Mixed Go+Rust Network Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the shared live two-binary bring-up + RPC-driver + observation substrate and use it to land the `differential::mixed_network` arm for real — a Go beacon + Rust follower form a network over the wire, a driven tx finalizes, and both nodes report the same tip.

**Architecture:** Extend the existing `tests/differential/src/network.rs` `Network`/`spawn_node` to launch the Go `avalanchego` binary as a sole-validator beacon and `avalanchers` as a non-validating follower, both on `--network-id=local` (byte-identical embedded genesis) with the well-known local staker certs and a real bootstrap topology. A small RPC driver issues one funded **C-chain** value transfer to the Go node; a settle loop polls both nodes until their C-chain height matches and stabilizes; the existing `Observation::collect().normalized()` asserts no fork / same tip. Go is the only staked validator, so "no fork" means the Rust node faithfully follows Go's finalized blocks (the symmetric mutual-validation topology is a documented follow-up).

**Tech Stack:** Rust, `tokio` (process + net + time), the workspace's hand-rolled JSON-RPC-over-`TcpStream` client (no HTTP-client crate — `00` §4 rule), `ava-wallet` (build→sign→issue) with an `ava-crypto` hand-rolled fallback, `cargo-nextest`. Live binaries: `~/avalanchego/build/avalanchego` (rpcchainvm=45) + `target/release/avalanchers`.

**Spec:** `docs/superpowers/specs/2026-06-22-m9-live-mixed-net-harness-design.md`

## Global Constraints

- License header on every `.rs`: `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` / `// See the file LICENSE for licensing terms.`
- 4-space indent, LF, final newline. Import grouping std → external → crate.
- Errors: `thiserror` per-crate enum + `Result<T>`; `anyhow` only in binary/tests. `ava-differential` already exposes `NetworkError` (extend it, additively).
- **No `unwrap()`/`expect()`/`dbg!`/`todo!` in library code** (clippy denies). `tests/differential/src/*.rs` is *library* code (it's the `ava-differential` lib crate) — hold it to library rules. Test files under `tests/differential/tests/*.rs` may use `expect`/`assert!`.
- Lint bar: `cargo clippy -p ava-differential --all-targets -- -D warnings` must stay clean, **including `--features live`**.
- No floats in any comparison/observation path. Integer math only; `checked_*`/`saturating_*`.
- Offline arms run every CI run; the live arm is `#[cfg(feature = "live")]` + `#[ignore]` and **must early-return cleanly when `$AVALANCHEGO_PATH` is unset**.
- Run tasks via the runner, not raw cargo, for the final gates: `./scripts/run_task.sh lint` / `test-unit`. Per-task TDD loops may call `cargo nextest run -p ava-differential ...` directly.
- The live binaries pre-gate is mandatory: `scripts/check_oracle_binary.sh` must print `OK` (binary commit == `~/avalanchego` HEAD, rpcchainvm=45) before booting a two-binary net.
- Commit messages: scope-prefixed, e.g. `differential(M9.15): ...`.

---

## File Structure

- `tests/differential/src/rpc.rs` — **new.** Crate-level JSON-RPC-over-`TcpStream` helper (`Endpoint`, `call`), extracted from `observation.rs::rpc` so both the observation collector and the live driver share one client.
- `tests/differential/src/observation.rs` — **modify.** Replace the private `mod rpc` with `use crate::rpc`.
- `tests/differential/src/livenet.rs` — **new.** The live-net role/flag model (`Role`, `node_args`), free-port allocation, cert-source resolution, readiness helpers (`scrape_node_id`, `await_bootstrapped`), and the tx driver (`drive_c_transfer`, `await_same_c_height`). Pure/offline-testable pieces live here next to the live orchestration that uses them.
- `tests/differential/src/network.rs` — **modify.** Add `NetworkError::CertSource`; enrich `Timeout` messages with a log tail; add `Network::boot_mixed` (the orchestration) and a role/cert/bootstrap-aware private `spawn_role_node`.
- `tests/differential/src/lib.rs` — **modify.** `pub mod rpc;` + `pub mod livenet;`.
- `tests/differential/tests/mixed_network.rs` — **modify.** Rewrite only the `#[cfg(feature="live")]` `mixed_network()` body to drive the substrate; leave the offline `replay_recorded` proptest arm untouched.
- `tests/differential/Cargo.toml` — **modify.** Add `ava-wallet` + (if the fallback is used) confirm `ava-crypto` dev-deps.

---

## Task 1: Extract a shared JSON-RPC client module

**Files:**
- Create: `tests/differential/src/rpc.rs`
- Modify: `tests/differential/src/observation.rs` (lines ~196-end: the private `mod rpc`)
- Modify: `tests/differential/src/lib.rs` (add `pub mod rpc;`)

**Interfaces:**
- Produces: `crate::rpc::Endpoint` with `pub(crate) fn parse(api_base: &str) -> Result<Endpoint, crate::observation::ObsError>`; `pub(crate) async fn call(endpoint: &Endpoint, path: &str, method: &str, params: &str) -> Result<serde_json::Value, crate::observation::ObsError>`. Same behavior as the current `observation::rpc`.

- [ ] **Step 1: Write the failing test**

Add to a new `tests/differential/src/rpc.rs` (`#[cfg(test)] mod tests`):

```rust
#[test]
fn parses_host_and_port_dropping_path() {
    let ep = Endpoint::parse("http://127.0.0.1:9650/ext/info").expect("parse");
    assert_eq!(ep.host, "127.0.0.1");
    assert_eq!(ep.port, 9650);
}

#[test]
fn rejects_non_http_scheme() {
    assert!(Endpoint::parse("https://x:1").is_err(), "https must be rejected");
    assert!(Endpoint::parse("127.0.0.1:9650").is_err(), "missing scheme rejected");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-differential -E 'test(parses_host_and_port)'`
Expected: FAIL — `rpc` module / `Endpoint` does not exist yet.

- [ ] **Step 3: Write minimal implementation**

Move the entire body of `observation.rs`'s private `mod rpc` into `tests/differential/src/rpc.rs`, changing visibility from `pub(super)` to `pub(crate)` and fixing the `use super::ObsError;` to `use crate::observation::ObsError;`. Add the license header. Then in `observation.rs` delete the inline `mod rpc { ... }` and replace its uses (`rpc::Endpoint::parse`, `rpc::call`) with `crate::rpc::Endpoint::parse` / `crate::rpc::call` (add `use crate::rpc;` at the top, then `rpc::...` keeps working). Add `pub mod rpc;` to `lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-differential` (the new `rpc` tests + every existing observation test pass)
Expected: PASS, no regressions.
Then: `cargo clippy -p ava-differential --all-targets -- -D warnings` → clean.

- [ ] **Step 5: Commit**

```bash
git add tests/differential/src/rpc.rs tests/differential/src/observation.rs tests/differential/src/lib.rs
git commit -m "differential(M9.15): extract shared JSON-RPC client into crate::rpc"
```

---

## Task 2: Local-net role + flag-vector assembly

**Files:**
- Create: `tests/differential/src/livenet.rs`
- Modify: `tests/differential/src/lib.rs` (add `pub mod livenet;`)

**Interfaces:**
- Produces:
  - `pub enum Role { Beacon, Follower }`
  - `pub struct NodeLaunch { pub http_port: u16, pub staking_port: u16, pub data_dir: std::path::PathBuf, pub cert_file: std::path::PathBuf, pub key_file: std::path::PathBuf, pub bootstrap: Option<Bootstrap> }`
  - `pub struct Bootstrap { pub ip: String, pub id: String }` (e.g. `ip = "127.0.0.1:9651"`, `id = "NodeID-..."`)
  - `pub fn node_args(launch: &NodeLaunch) -> Vec<String>` — the exact CLI flag vector (no binary path).

- [ ] **Step 1: Write the failing test**

In `livenet.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn launch(role: Role) -> NodeLaunch {
        NodeLaunch {
            http_port: 9650,
            staking_port: 9651,
            data_dir: PathBuf::from("/tmp/slot0"),
            cert_file: PathBuf::from("/certs/staker1.crt"),
            key_file: PathBuf::from("/certs/staker1.key"),
            bootstrap: match role {
                Role::Beacon => None,
                Role::Follower => Some(Bootstrap {
                    ip: "127.0.0.1:9651".to_owned(),
                    id: "NodeID-abc".to_owned(),
                }),
            },
        }
    }

    #[test]
    fn beacon_args_have_no_bootstrap_flags() {
        let args = node_args(&launch(Role::Beacon));
        assert!(args.iter().any(|a| a == "--network-id=local"), "network-id");
        assert!(args.iter().any(|a| a == "--http-port=9650"), "http-port");
        assert!(args.iter().any(|a| a == "--staking-port=9651"), "staking-port");
        assert!(args.iter().any(|a| a == "--staking-tls-cert-file=/certs/staker1.crt"), "cert");
        assert!(args.iter().any(|a| a == "--staking-tls-key-file=/certs/staker1.key"), "key");
        assert!(!args.iter().any(|a| a.starts_with("--bootstrap-ips")), "no bootstrap-ips on beacon");
        assert!(!args.iter().any(|a| a.starts_with("--bootstrap-ids")), "no bootstrap-ids on beacon");
    }

    #[test]
    fn follower_args_carry_bootstrap_topology() {
        let args = node_args(&launch(Role::Follower));
        assert!(args.iter().any(|a| a == "--bootstrap-ips=127.0.0.1:9651"), "bootstrap-ips");
        assert!(args.iter().any(|a| a == "--bootstrap-ids=NodeID-abc"), "bootstrap-ids");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-differential -E 'test(beacon_args) + test(follower_args)'`
Expected: FAIL — `livenet` / `node_args` undefined.

- [ ] **Step 3: Write minimal implementation**

Add the license header, the types, and:

```rust
/// The role a node plays in the live mixed net.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Sole initial-staker / validator; proposes and finalizes blocks.
    Beacon,
    /// Non-validating node that bootstraps from (and follows) the beacon.
    Follower,
}

/// A bootstrap target (the beacon's staking address + node ID).
#[derive(Debug, Clone)]
pub struct Bootstrap {
    /// `host:staking_port`, e.g. `127.0.0.1:9651`.
    pub ip: String,
    /// The beacon's scraped `NodeID-...` string.
    pub id: String,
}

/// Everything needed to launch one node (binary path supplied separately).
#[derive(Debug, Clone)]
pub struct NodeLaunch {
    pub http_port: u16,
    pub staking_port: u16,
    pub data_dir: std::path::PathBuf,
    pub cert_file: std::path::PathBuf,
    pub key_file: std::path::PathBuf,
    /// `None` for a beacon; `Some` for a follower.
    pub bootstrap: Option<Bootstrap>,
}

/// The exact CLI flag vector for `launch` (mirrors specs/13; both binaries
/// honor these identically).
#[must_use]
pub fn node_args(launch: &NodeLaunch) -> Vec<String> {
    let mut args = vec![
        "--network-id=local".to_owned(),
        format!("--http-port={}", launch.http_port),
        format!("--staking-port={}", launch.staking_port),
        format!("--data-dir={}", launch.data_dir.display()),
        format!("--staking-tls-cert-file={}", launch.cert_file.display()),
        format!("--staking-tls-key-file={}", launch.key_file.display()),
    ];
    if let Some(b) = &launch.bootstrap {
        args.push(format!("--bootstrap-ips={}", b.ip));
        args.push(format!("--bootstrap-ids={}", b.id));
    }
    args
}
```

Add `pub mod livenet;` to `lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-differential -E 'test(beacon_args) + test(follower_args)'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/differential/src/livenet.rs tests/differential/src/lib.rs
git commit -m "differential(M9.15): live-net role + CLI flag-vector assembly"
```

---

## Task 3: Free-port allocation + local staker cert resolution

**Files:**
- Modify: `tests/differential/src/livenet.rs`
- Modify: `tests/differential/src/network.rs` (add `NetworkError::CertSource`)

**Interfaces:**
- Produces:
  - `pub fn free_ports(n: usize) -> std::io::Result<Vec<u16>>` — `n` distinct currently-free localhost TCP ports.
  - `pub struct CertPair { pub cert: std::path::PathBuf, pub key: std::path::PathBuf }`
  - `pub fn local_staker(idx: u8) -> Result<CertPair, NetworkError>` — resolves `$AVALANCHEGO_SRC/staking/local/staker{idx}.{crt,key}` (default `$AVALANCHEGO_SRC = ~/avalanchego`); `idx` is 1 or 2.
- Consumes: `crate::network::NetworkError`.

- [ ] **Step 1: Write the failing test**

In `livenet.rs` tests:

```rust
#[test]
fn free_ports_are_distinct_and_nonzero() {
    let ports = free_ports(4).expect("free_ports");
    assert_eq!(ports.len(), 4, "asked for 4 ports");
    let mut sorted = ports.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "ports are distinct");
    assert!(ports.iter().all(|&p| p != 0), "no zero ports");
}

#[test]
fn local_staker_missing_dir_errors_with_path() {
    // SAFETY: single-threaded test; restore after.
    let prev = std::env::var("AVALANCHEGO_SRC").ok();
    unsafe { std::env::set_var("AVALANCHEGO_SRC", "/nonexistent-xyz") };
    let err = local_staker(1).expect_err("missing cert dir must error");
    assert!(format!("{err}").contains("staker1"), "error names the cert: {err}");
    match prev {
        Some(v) => unsafe { std::env::set_var("AVALANCHEGO_SRC", v) },
        None => unsafe { std::env::remove_var("AVALANCHEGO_SRC") },
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-differential -E 'test(free_ports_are) + test(local_staker_missing)'`
Expected: FAIL — `free_ports` / `local_staker` / `NetworkError::CertSource` undefined.

- [ ] **Step 3: Write minimal implementation**

In `network.rs`, add to `enum NetworkError`:

```rust
    /// The local staker cert/key source could not be resolved.
    #[error("local staker cert source: {0}")]
    CertSource(String),
```

In `livenet.rs`:

```rust
use std::net::TcpListener;

use crate::network::NetworkError;

/// `n` distinct currently-free localhost TCP ports. Binds `:0`, reads the OS
/// assignment, and drops the listener (a brief TOCTOU window the live arm
/// tolerates — nodes bind immediately after).
pub fn free_ports(n: usize) -> std::io::Result<Vec<u16>> {
    let mut held = Vec::with_capacity(n);
    let mut ports = Vec::with_capacity(n);
    for _ in 0..n {
        let l = TcpListener::bind(("127.0.0.1", 0))?;
        ports.push(l.local_addr()?.port());
        held.push(l); // hold all until done so we never hand out a duplicate
    }
    Ok(ports)
}

/// A resolved staker cert/key pair.
#[derive(Debug, Clone)]
pub struct CertPair {
    pub cert: std::path::PathBuf,
    pub key: std::path::PathBuf,
}

/// Resolve the well-known local staker `idx` (1 or 2) from
/// `$AVALANCHEGO_SRC/staking/local/` (default `~/avalanchego`).
pub fn local_staker(idx: u8) -> Result<CertPair, NetworkError> {
    let src = std::env::var("AVALANCHEGO_SRC").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/avalanchego")
    });
    let dir = std::path::Path::new(&src).join("staking").join("local");
    let cert = dir.join(format!("staker{idx}.crt"));
    let key = dir.join(format!("staker{idx}.key"));
    if !cert.exists() || !key.exists() {
        return Err(NetworkError::CertSource(format!(
            "staker{idx} cert/key not found under {} (set $AVALANCHEGO_SRC)",
            dir.display()
        )));
    }
    Ok(CertPair { cert, key })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-differential -E 'test(free_ports_are) + test(local_staker_missing)'`
Expected: PASS.
Then: `cargo clippy -p ava-differential --all-targets -- -D warnings` → clean.

- [ ] **Step 5: Commit**

```bash
git add tests/differential/src/livenet.rs tests/differential/src/network.rs
git commit -m "differential(M9.15): free-port allocation + local staker cert resolution"
```

---

## Task 4: Readiness helpers — node-ID scrape + bootstrapped wait

**Files:**
- Modify: `tests/differential/src/livenet.rs`

**Interfaces:**
- Produces:
  - `pub fn parse_node_id(v: &serde_json::Value) -> Option<String>` — pulls `nodeID` from an `info.getNodeID` result.
  - `pub fn parse_bootstrapped(v: &serde_json::Value) -> Option<bool>` — pulls `isBootstrapped` from an `info.isBootstrapped` result.
  - `pub async fn scrape_node_id(api_base: &str) -> Result<String, crate::observation::ObsError>`
  - `pub async fn await_bootstrapped(api_base: &str, chains: &[&str], within: std::time::Duration) -> Result<(), NetworkError>` — polls `info.isBootstrapped` for each chain alias (`"P"`,`"X"`,`"C"`) until all true.
- Consumes: `crate::rpc::call`, `crate::network::NetworkError`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parse_node_id_extracts_field() {
    let v = serde_json::json!({ "nodeID": "NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg" });
    assert_eq!(
        parse_node_id(&v).as_deref(),
        Some("NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg")
    );
    assert_eq!(parse_node_id(&serde_json::json!({})), None);
}

#[test]
fn parse_bootstrapped_extracts_bool() {
    assert_eq!(parse_bootstrapped(&serde_json::json!({ "isBootstrapped": true })), Some(true));
    assert_eq!(parse_bootstrapped(&serde_json::json!({ "isBootstrapped": false })), Some(false));
    assert_eq!(parse_bootstrapped(&serde_json::json!({})), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-differential -E 'test(parse_node_id) + test(parse_bootstrapped)'`
Expected: FAIL — functions undefined.

- [ ] **Step 3: Write minimal implementation**

```rust
use crate::rpc;

/// Pull `nodeID` from an `info.getNodeID` result.
#[must_use]
pub fn parse_node_id(v: &serde_json::Value) -> Option<String> {
    v.get("nodeID").and_then(|n| n.as_str()).map(str::to_owned)
}

/// Pull `isBootstrapped` from an `info.isBootstrapped` result.
#[must_use]
pub fn parse_bootstrapped(v: &serde_json::Value) -> Option<bool> {
    v.get("isBootstrapped").and_then(serde_json::Value::as_bool)
}

/// Query `info.getNodeID` over the node's API.
pub async fn scrape_node_id(api_base: &str) -> Result<String, crate::observation::ObsError> {
    let ep = rpc::Endpoint::parse(api_base)?;
    let res = rpc::call(&ep, "/ext/info", "info.getNodeID", "{}").await?;
    parse_node_id(&res)
        .ok_or_else(|| crate::observation::ObsError::Rpc("info.getNodeID: missing nodeID".to_owned()))
}

/// Poll `info.isBootstrapped` for every chain alias until all report true or
/// `within` elapses.
pub async fn await_bootstrapped(
    api_base: &str,
    chains: &[&str],
    within: std::time::Duration,
) -> Result<(), NetworkError> {
    let ep = rpc::Endpoint::parse(api_base)
        .map_err(|e| NetworkError::Timeout(format!("bad api_base {api_base}: {e}")))?;
    let deadline = std::time::Instant::now()
        .checked_add(within)
        .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
    loop {
        let mut all = true;
        for chain in chains {
            let params = format!(r#"{{"chain":"{chain}"}}"#);
            let ready = rpc::call(&ep, "/ext/info", "info.isBootstrapped", &params)
                .await
                .ok()
                .and_then(|v| parse_bootstrapped(&v))
                .unwrap_or(false);
            if !ready {
                all = false;
                break;
            }
        }
        if all {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(NetworkError::Timeout(format!(
                "node {api_base} did not bootstrap {chains:?} within {within:?}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
```

> Note: `ObsError::Rpc(String)` already exists (see `observation.rs`); reuse it. If its variant name differs, match the existing one.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-differential -E 'test(parse_node_id) + test(parse_bootstrapped)'`
Expected: PASS.
Then: `cargo clippy -p ava-differential --all-targets -- -D warnings` → clean.

- [ ] **Step 5: Commit**

```bash
git add tests/differential/src/livenet.rs
git commit -m "differential(M9.15): node-id scrape + bootstrapped-wait readiness helpers"
```

---

## Task 5: `Network::boot_mixed` — the bring-up orchestration

**Files:**
- Modify: `tests/differential/src/network.rs`

**Interfaces:**
- Consumes: `crate::livenet::{Role, NodeLaunch, Bootstrap, CertPair, free_ports, local_staker, scrape_node_id, await_bootstrapped}`.
- Produces: `pub async fn Network::boot_mixed(seed: u64) -> Result<Network, NetworkError>` — pre-gates the oracle binary, launches the Go beacon, scrapes its node-ID, launches the Rust follower bootstrapped from it, waits for connectivity + Rust bootstrapped. Returns a `Network` whose `nodes()[0]` is the Go beacon and `nodes()[1]` is the Rust follower.

- [ ] **Step 1: Write the failing test** (offline-safe guard behavior)

Add to `network.rs` `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn boot_mixed_errors_cleanly_without_go_binary() {
    // SAFETY: single-threaded test; remove the var so the Go-binary guard fires.
    let prev = std::env::var("AVALANCHEGO_PATH").ok();
    unsafe { std::env::remove_var("AVALANCHEGO_PATH") };
    let err = Network::boot_mixed(0xC0FFEE).await.expect_err("must require a Go binary");
    assert!(matches!(err, NetworkError::GoBinaryMissing), "got {err:?}");
    if let Some(v) = prev {
        unsafe { std::env::set_var("AVALANCHEGO_PATH", v) };
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-differential -E 'test(boot_mixed_errors_cleanly)'`
Expected: FAIL — `boot_mixed` undefined.

- [ ] **Step 3: Write minimal implementation**

Add a role/cert/bootstrap-aware spawner and the orchestration. Place the oracle pre-gate behind the Go-binary check so the offline test reaches `GoBinaryMissing` first.

```rust
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
        Ok(s) => s.lines().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"),
        Err(_) => String::new(),
    }
}

impl Network {
    /// Boot a live Go-beacon + Rust-follower mixed net (M9.15). Go is the sole
    /// staked validator; the Rust node bootstraps from and follows it.
    ///
    /// # Errors
    /// [`NetworkError`] if `$AVALANCHEGO_PATH` is unset, the oracle binary is
    /// stale, certs are missing, a node fails to spawn, or readiness times out.
    pub async fn boot_mixed(seed: u64) -> Result<Network, NetworkError> {
        let go_path =
            std::env::var("AVALANCHEGO_PATH").map_err(|_| NetworkError::GoBinaryMissing)?;
        let rust_path = locate_rust_binary()?;

        // Pre-gate: binary commit must match the ~/avalanchego checkout (rpcchainvm=45).
        let status = std::process::Command::new("scripts/check_oracle_binary.sh")
            .status();
        if let Ok(s) = status {
            if !s.success() {
                return Err(NetworkError::Timeout(
                    "check_oracle_binary.sh failed — rebuild ~/avalanchego (stale binary)".to_owned(),
                ));
            }
        }

        let work_dir = std::env::temp_dir().join(format!("mixed-net-{seed}"));
        let _ = std::fs::create_dir_all(&work_dir);
        let ports = crate::livenet::free_ports(4).map_err(|e| NetworkError::Timeout(format!("free_ports: {e}")))?;
        let go_staker = crate::livenet::local_staker(1)?;
        let rust_staker = crate::livenet::local_staker(2)?;

        // 1. Go beacon.
        let go_launch = crate::livenet::NodeLaunch {
            http_port: ports[0],
            staking_port: ports[1],
            data_dir: work_dir.join("go"),
            cert_file: go_staker.cert,
            key_file: go_staker.key,
            bootstrap: None,
        };
        let go_node = spawn_role_node(&go_path, Binary::Go, 0, &go_launch)?;

        // 2. Scrape the Go node-ID (it must answer info.getNodeID before we wire bootstrap).
        let go_id = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
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
            http_port: ports[2],
            staking_port: ports[3],
            data_dir: work_dir.join("rust"),
            cert_file: rust_staker.cert,
            key_file: rust_staker.key,
            bootstrap: Some(crate::livenet::Bootstrap {
                ip: format!("127.0.0.1:{}", ports[1]),
                id: go_id,
            }),
        };
        let rust_node = spawn_role_node(&rust_path, Binary::Rust, 1, &rust_launch)?;

        let net = Network {
            nodes: vec![go_node, rust_node],
            work_dir,
        };

        // 4. Wait for the Rust follower to bootstrap P/X/C from Go.
        crate::livenet::await_bootstrapped(
            &net.nodes[1].api_base,
            &["P", "X", "C"],
            std::time::Duration::from_secs(180),
        )
        .await
        .map_err(|e| NetworkError::Timeout(format!(
            "{e}\nrust log:\n{}",
            log_tail(&net.nodes[1].log_path, 60)
        )))?;

        Ok(net)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p ava-differential -E 'test(boot_mixed_errors_cleanly)'`
Expected: PASS (the Go-binary guard fires before any spawn).
Then: `cargo clippy -p ava-differential --all-targets -- -D warnings` → clean (also `--features live`).

- [ ] **Step 5: Commit**

```bash
git add tests/differential/src/network.rs
git commit -m "differential(M9.15): Network::boot_mixed — Go-beacon + Rust-follower bring-up"
```

---

## Task 6: The tx driver — issue a C-chain transfer + settle on equal height

**Files:**
- Modify: `tests/differential/src/livenet.rs`
- Modify: `tests/differential/Cargo.toml` (add `ava-wallet` dev/normal dep as needed)

**Interfaces:**
- Produces:
  - `pub async fn drive_c_transfer(go_api: &str) -> Result<(), NetworkError>` — build+sign+issue one C-chain value transfer from the local prefunded key to itself (a no-op-value self-send is fine; the point is to produce a finalized block) against the Go node's `/ext/bc/C/rpc`, then poll the receipt until mined.
  - `pub async fn await_same_c_height(a_api: &str, b_api: &str, min: u64, within: std::time::Duration) -> Result<u64, NetworkError>` — poll `eth_blockNumber` on both nodes until both report the same height `>= min`, stable across two consecutive polls; returns that height.

> **Vehicle decision (do this first, ~10 min):** read `crates/ava-wallet/src/c/` + `crates/ava-wallet/src/client.rs` to learn the C-wallet build→sign→issue API. The local network's prefunded "ewoq" key is well-known: address `0x8db97C7cEcE249c2b98bDC0226Cc4C2A57BF52FC`, private key `0x56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027`.
> - **Primary:** use `ava_wallet::c` to build+sign a minimal C-chain tx and issue it via the wallet's client to `go_api`.
> - **Fallback (if the wallet API doesn't fit cleanly in one task):** hand-roll a legacy Ethereum tx — RLP-encode `[nonce, gasPrice, gas, to, value=0, data=[]]`, keccac256, sign with `ava_crypto`'s secp256k1 (recoverable), RLP-encode the signed tx, and POST `eth_sendRawTransaction` via `crate::rpc::call`. Local chainId is `43112` (0xa868) → EIP-155 `v = 2*chainId + 35 + recId`.
> Record in the commit message which vehicle was used.

- [ ] **Step 1: Write the failing test** (pure settle-comparison logic)

Factor the height comparison into a pure helper so it is offline-testable:

```rust
/// Decide whether two polled heights mean "settled at the same tip".
#[must_use]
pub fn settled(a: Option<u64>, b: Option<u64>, min: u64) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x == y && x >= min)
}

#[cfg(test)]
mod settle_tests {
    use super::settled;
    #[test]
    fn settled_requires_equal_and_min() {
        assert!(settled(Some(3), Some(3), 1), "equal and >= min");
        assert!(!settled(Some(2), Some(3), 1), "unequal heights not settled");
        assert!(!settled(Some(1), Some(1), 2), "below min not settled");
        assert!(!settled(None, Some(1), 1), "missing height not settled");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-differential -E 'test(settled_requires_equal)'`
Expected: FAIL — `settled` undefined.

- [ ] **Step 3: Write minimal implementation**

Implement `settled` (above), then `await_same_c_height` and `drive_c_transfer`:

```rust
/// Parse an `eth_blockNumber` hex-quantity result (`"0x1a"`) into a height.
#[must_use]
pub fn parse_eth_block_number(v: &serde_json::Value) -> Option<u64> {
    let s = v.as_str()?.strip_prefix("0x")?;
    u64::from_str_radix(s, 16).ok()
}

/// Poll both nodes' C-chain `eth_blockNumber` until equal, `>= min`, and stable.
pub async fn await_same_c_height(
    a_api: &str,
    b_api: &str,
    min: u64,
    within: std::time::Duration,
) -> Result<u64, NetworkError> {
    let ea = rpc::Endpoint::parse(a_api).map_err(|e| NetworkError::Timeout(format!("{e}")))?;
    let eb = rpc::Endpoint::parse(b_api).map_err(|e| NetworkError::Timeout(format!("{e}")))?;
    let deadline = std::time::Instant::now()
        .checked_add(within)
        .ok_or_else(|| NetworkError::Timeout("deadline overflow".to_owned()))?;
    let mut last_stable: Option<u64> = None;
    loop {
        let ha = rpc::call(&ea, "/ext/bc/C/rpc", "eth_blockNumber", "[]").await.ok().and_then(|v| parse_eth_block_number(&v));
        let hb = rpc::call(&eb, "/ext/bc/C/rpc", "eth_blockNumber", "[]").await.ok().and_then(|v| parse_eth_block_number(&v));
        if settled(ha, hb, min) {
            let h = ha.unwrap_or(0);
            if last_stable == Some(h) {
                return Ok(h);
            }
            last_stable = Some(h);
        } else {
            last_stable = None;
        }
        if std::time::Instant::now() >= deadline {
            return Err(NetworkError::Timeout(format!(
                "C-chain heights never settled >= {min} (a={ha:?} b={hb:?})"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Build+sign+issue one C-chain tx to `go_api` and wait for it to be mined.
/// (Vehicle per the decision note above.)
pub async fn drive_c_transfer(go_api: &str) -> Result<(), NetworkError> {
    // ... build+sign+issue (ava-wallet primary / hand-rolled fallback) ...
    // POST eth_sendRawTransaction via crate::rpc::call, capture tx hash,
    // then poll eth_getTransactionReceipt until non-null (or timeout).
    todo!("implement per the vehicle decision; replace before commit")
}
```

> The `todo!()` is a **placeholder marker for the executor only** — it MUST be replaced with the real build+sign+issue before Step 4 (the project bans `todo!` in lib code; clippy will reject it). The surrounding `settled` / `await_same_c_height` / `parse_eth_block_number` are complete and the offline gate for this task.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-differential -E 'test(settled_requires_equal)'`
Expected: PASS.
Then build the full live arm to ensure `drive_c_transfer` compiles with no `todo!`:
`cargo clippy -p ava-differential --all-targets --features live -- -D warnings` → clean.

- [ ] **Step 5: Commit**

```bash
git add tests/differential/src/livenet.rs tests/differential/Cargo.toml
git commit -m "differential(M9.15): C-chain tx driver + same-height settle (<vehicle used>)"
```

---

## Task 7: Wire the live `mixed_network` test to the substrate

**Files:**
- Modify: `tests/differential/tests/mixed_network.rs` (only the `#[cfg(feature="live")]` `mixed_network()` fn body, lines ~135-209)

**Interfaces:**
- Consumes: `ava_differential::network::Network::{boot_mixed, nodes, shutdown}`, `ava_differential::livenet::{drive_c_transfer, await_same_c_height}`, `ava_differential::observation::Observation`.

- [ ] **Step 1: Replace the live-arm body** (keep the offline `replay_recorded` proptest arm above it untouched)

```rust
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a live mixed Go+Rust net ($AVALANCHEGO_PATH + avalanchers) — nightly only"]
async fn mixed_network() {
    use std::time::Duration;

    use ava_differential::livenet::{await_same_c_height, drive_c_transfer};
    use ava_differential::network::Network;
    use ava_differential::observation::Observation;

    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network");
        return;
    }

    // 1. Boot Go beacon + Rust follower; waits for the Rust node to bootstrap P/X/C from Go.
    let net = Network::boot_mixed(0x5EED).await.expect("mixed Go+Rust net boots + bootstraps");
    net.await_all_connected(Duration::from_secs(60))
        .await
        .expect("Go and Rust complete the TLS handshake / exchange PeerLists");

    let go_api = net.nodes()[0].api_base.clone();
    let rust_api = net.nodes()[1].api_base.clone();

    // 2. Record the pre-tx C height, drive one tx on the Go validator, settle.
    let before = await_same_c_height(&go_api, &rust_api, 0, Duration::from_secs(30))
        .await
        .expect("nodes agree on a starting C height");
    drive_c_transfer(&go_api).await.expect("issue + mine one C-chain tx on the Go validator");
    let after = await_same_c_height(&go_api, &rust_api, before + 1, Duration::from_secs(60))
        .await
        .expect("both nodes advance to the same C height after the tx");
    assert!(after > before, "tx must advance the C-chain tip: {before} -> {after}");

    // 3. No fork / same tip: full normalized observation must match across impls.
    let go_obs = Observation::collect(&go_api).await.expect("collect Go observation").normalized();
    let rust_obs = Observation::collect(&rust_api).await.expect("collect Rust observation").normalized();
    assert_eq!(go_obs, rust_obs, "Go and Rust diverged — fork across the mixed net");

    net.shutdown().await;
}
```

- [ ] **Step 2: Verify it compiles under both feature sets**

Run: `cargo clippy -p ava-differential --all-targets -- -D warnings` (offline arm still compiles)
Run: `cargo clippy -p ava-differential --all-targets --features live -- -D warnings`
Expected: both clean.

- [ ] **Step 3: Verify offline arms still pass + live arm skips cleanly**

Run: `cargo nextest run -p ava-differential` (offline `replay_recorded` arm green; live arm not run)
Run: `cargo nextest run -p ava-differential --features live --run-ignored all -E 'test(mixed_network)'` **with `AVALANCHEGO_PATH` unset**
Expected: the live `mixed_network` prints the skip line and passes (early return).

- [ ] **Step 4: Commit**

```bash
git add tests/differential/tests/mixed_network.rs
git commit -m "differential(M9.15): wire live mixed_network arm to the boot_mixed substrate"
```

---

## Task 8: Run the live arm for real this session (the proof)

**Files:** none (verification + any gap fixes discovered).

This is the user's bar: actually boot the two-binary net against `~/avalanchego` and show the live `mixed_network` arm green, built up rung by rung.

- [ ] **Step 1: Build + pre-gate**

```bash
cargo build --release -p avalanchers
cd ~/avalanchego && ./scripts/build.sh && cd -   # only if check fails
./scripts/check_oracle_binary.sh                 # must print OK (commit match + rpcchainvm=45)
```

- [ ] **Step 2: Rung 1 — handshake + bootstrap.** Temporarily run with the assert/tx portions commented (or via a `RUST_LOG` env + the `boot_mixed` + `await_all_connected` calls only) to confirm: the Go beacon answers `info.getNodeID`, the Rust follower connects (`info.peers` lists Go), and `info.isBootstrapped` flips true for P/X/C. If it stalls, read `target/.../mixed-net-*/rust/node.log` and fix the gap (e.g. genesis mismatch, cert/node-id, bootstrap flag wiring) via a TDD loop with a lower rung green underneath. Record any real gap honestly in the plan + spec.

```bash
AVALANCHEGO_PATH=~/avalanchego/build/avalanchego \
  cargo nextest run -p ava-differential --features live --run-ignored all -E 'test(mixed_network)' --no-capture
```

- [ ] **Step 3: Rung 2 — tip at rest.** With bootstrap green, confirm `await_same_c_height(.., 0, ..)` and the pre-tx `Observation` equality hold (both nodes agree at the quiescent tip) before any tx.

- [ ] **Step 4: Rung 3 — driven tx.** Confirm `drive_c_transfer` issues + mines on the Go validator and both nodes advance to the same height, and the final `assert_eq!(go_obs, rust_obs)` passes. If the Rust node does not follow Go's block, that is the real consensus-convergence gap the spec anticipated — stop, capture logs, and fix via TDD (do not weaken the assert).

- [ ] **Step 5: Capture the green result + commit any fixes.**

```bash
# Re-run the full live arm end to end and capture the PASS line.
AVALANCHEGO_PATH=~/avalanchego/build/avalanchego \
  cargo nextest run -p ava-differential --features live --run-ignored all -E 'test(mixed_network)'
git add -A && git commit -m "differential(M9.15): live mixed_network green end-to-end (proof + gap fixes)"
```

- [ ] **Step 6: Final repo gates**

```bash
./scripts/run_task.sh lint          # clippy + rustfmt + license + TOML, workspace-wide
./scripts/run_task.sh test-unit     # offline workspace tests stay green
```

Then update the M9 plan banner (`plan/M9-interop-hardening.md`) and the memory frontier note to reflect M9.15 live `mixed_network` GREEN (or the honest rung reached + remaining gap).

---

## Self-Review

**Spec coverage:**
- Topology (Go beacon staker1 / Rust follower staker2, `--network-id=local`, scraped bootstrap) → Tasks 2, 3, 5. ✅
- Genesis parity via `--network-id=local` → Task 2 (no genesis file passed). ✅
- Cert source from `staking/local/` → Task 3. ✅
- Bring-up sequence (pre-gate → Go → scrape → Rust → connected → bootstrapped) → Task 5. ✅
- Driver (funded tx, settle, poll-to-Committed) → Task 6; spec's P-chain-primary / C-chain-fallback resolved to C-chain as the concrete vehicle (spec authorized it; most directly observable via `eth_blockNumber`). ✅
- Observation no-fork assert → Task 7 (reuses existing `Observation`). ✅
- Error handling / log-tail-on-timeout / kill_on_drop → Tasks 3 (`CertSource`), 5 (`log_tail`, enriched `Timeout`); `kill_on_drop` + `Drop`/`shutdown` already exist. ✅
- Offline arms unchanged + new pure unit tests → Tasks 1-4, 6, 7. ✅
- Live arm gating + early return → Task 7. ✅
- Real this-session proof, rung by rung → Task 8. ✅
- Out-of-scope (Approach C; upgrade/load reuse; nightly var) → recorded in spec, not implemented. ✅

**Placeholder scan:** One intentional `todo!()` in Task 6 Step 3, explicitly flagged as an executor marker that MUST be replaced before Step 4 (clippy bans `todo!`). All other steps carry complete code. No "TBD"/"handle edge cases"/"similar to Task N".

**Type consistency:** `NodeLaunch`/`Bootstrap`/`Role`/`CertPair` defined in Task 2-3 and consumed verbatim in Task 5. `rpc::{Endpoint,call}` from Task 1 used in Tasks 4, 6. `NetworkError::{GoBinaryMissing, CertSource, Timeout, Spawn}` consistent across Tasks 3, 5. `Observation`/`Network::{nodes,shutdown,await_all_connected,boot_mixed}` match existing signatures + Task 5's addition. `settled`/`parse_eth_block_number`/`await_same_c_height`/`drive_c_transfer` from Task 6 used in Task 7.

---

## As-built (2026-06-22)

**Tasks 1–7 (substrate): DONE.** All implemented TDD-first, reviewed clean, committed
on branch `m9.15-live-mixed-net`. Two authorized deviations from the brief, both
forced by the crate's `#![forbid(unsafe_code)]` (edition-2024 `std::env::set_var`/
`remove_var` are `unsafe fn`): the `local_staker` and `boot_mixed` guard tests use
private path/option-injected helpers (`local_staker_in`, `resolve_go_binary`)
instead of env mutation — hermetic, same intent, public APIs unchanged. Task 6
vehicle = reth/alloy `TxLegacy` + `ava-crypto` secp256k1 (mirrors
`ava-evm/tests/evm_factory.rs::sign_legacy`); `ava-crypto` moved dev→normal dep.

**Task 8 (live proof): drove the REAL two-binary net rung by rung; reached the
documented hard gap and recorded it honestly rather than faking the assert.**

Prereqs cleared: rebuilt `avalanchers` release; rebuilt the Go oracle (was stale —
`~/avalanchego` had been pulled to `d295aca`; pre-gate now `OK`, rpcchainvm=45).
Toolchain gotcha: the on-PATH `go` was nix 1.26.3 while `GOROOT` pointed at mise
1.25.10 → `build.sh` failed "version mismatch"; fixed by building with mise's
go 1.25.10 on PATH.

Rungs reached:
- **Go beacon: healthy.** Boots, initializes P/X/C, serves `info.*` (node
  `NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg`, staker1, a genesis validator).
- **Rust follower: boots** (after two harness gap-fixes below) — creates
  `process.json`, generates its BLS signer, starts its API server.
- **Peer handshake: does NOT complete.** The follower repeatedly dials the beacon
  and starts a TLS 1.3 mutual-auth handshake (ClientHello → CertificateRequest →
  "Attempting client auth") but loops reconnecting ~every 250 ms; Go never logs
  the peer. So bootstrap never starts → `boot_mixed`'s `await_bootstrapped` times
  out at 180s → `mixed_network` FAILS (no fake green).

Harness gap-fixes committed (`d0ed34c`), correct and reviewed-worthy regardless of
the blocker — they advance the harness to the exact edge of the gap:
1. **ECDSA follower cert** (`livenet::generate_staker`): `avalanchers`'
   `Identity::from_pem` (`ava-network/identity.rs`) only loads ECDSA-P256 PKCS#8
   keys and rejects the **RSA** local staker keys that Go reads fine
   (`stakingKeyType: RSA`). The follower is a non-validator, so it gets a fresh
   ECDSA cert; the Go beacon keeps RSA `staker1`.
2. **`--db-type=memdb`** (both nodes): the `avalanchers` release build ships
   without the optional `rocksdb` feature the default on-disk `leveldb` requires
   (the official `cargo build -p avalanchers --release` enables no features).
3. **`log_tail` reads `logs/main.log`**: avalanchego logs there after logger init,
   not stdout, so the timeout diagnostic was empty.

**BLOCKER (out of scope for this *harness* plan):** the live arm cannot go green
until `avalanchers` completes a real peer handshake + networked bootstrap against
a live Go beacon. Its live chain-readiness to date was wired in-process /
beaconless (`drive_startup_chains`); the standalone networked-bootstrap-from-a-
remote-peer path is not yet operational. This is avalanchego-side production work
(peer-layer + bootstrap engine), not a `differential`-harness task.

**Follow-ups recorded:**
- avalanchers cannot load RSA staking keys (only ECDSA-P256) — a real drop-in gap
  if the standard local staker certs must be usable directly.
- avalanchers release build has no on-disk DB backend (no `rocksdb` feature).
- avalanchers peer handshake against a live Go node does not complete — the
  primary blocker for the live mixed-net arm; needs production work + better
  application-level (non-`rustls`) handshake/peer diagnostics.
- The live `mixed_network` arm + `boot_mixed` substrate are correct and stay
  wired (`#[cfg(feature="live")] #[ignore]`, nightly-gated); they will pass once
  the peer/bootstrap gap closes. CI is unaffected (offline arms green: 38/38).
