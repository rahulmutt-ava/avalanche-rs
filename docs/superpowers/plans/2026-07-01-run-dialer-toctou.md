# `run_dialer` Concurrent Self-Dial TOCTOU Guard — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the scan dialer from launching duplicate concurrent outbound dials to the same peer while a slow/stalling TLS upgrade is in flight.

**Architecture:** Add a `dialing: Mutex<HashSet<NodeId>>` in-flight guard set to `NetworkImpl`. Factor the dialer tick body into a synchronous, unit-testable `select_dial_targets(now)` helper that skips nodes already in `connected`/`connecting`/`dialing` and marks selected nodes. `handle_dial` clears the mark on every exit path via a `Drop` guard. This is the scan-loop equivalent of Go's one-dial-goroutine-per-tracked-IP model.

**Tech Stack:** Rust, `parking_lot::Mutex`, tokio tasks, `cargo-nextest`. Single crate: `ava-network`.

## Global Constraints

- License header on every `.rs` file: `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` / `// See the file LICENSE for licensing terms.` (no new files created here, so no new headers needed).
- 4-space indent, LF endings, final newline. Import grouping std → external → crate.
- Library code: no `unwrap()`/`expect()`/`dbg!`/`todo!` (clippy denies). Test code may use them (the existing `mod tests` already `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]`).
- `peers_lock` / `tracked_ips` / `dialing` are leaf locks: never held across an `.await`. `parking_lot::Mutex` is synchronous.
- No change to `TrackedIp` backoff/jitter, no restructure into per-IP tasks, no change to `admit_peer` dedup.
- Verify with `cargo nextest run -p ava-network` + `-p ava-node`, and `./scripts/run_task.sh lint`.

---

## File Structure

Only one production file and its in-module test block change:

- Modify: `crates/ava-network/src/network/net_impl.rs`
  - Add `dialing` field to `struct NetworkImpl` (near line 58–59, with `connecting`/`connected`).
  - Initialize `dialing` in the constructor (near line 130–132).
  - Add `fn select_dial_targets(&self, now: Instant) -> Vec<(NodeId, SocketAddr)>`.
  - Rewrite the `run_dialer` tick body (lines 331–349) to call the helper.
  - Change `fn handle_dial` signature to take `node_id: NodeId` (line 264) and add the `DialGuard` at the top of the spawned task.
  - Add `struct DialGuard` + `impl Drop`.
  - Add tests to the existing `mod tests` block (ends at line 546).

---

### Task 1: `dialing` guard set + `select_dial_targets` helper (the TOCTOU fix)

**Files:**
- Modify: `crates/ava-network/src/network/net_impl.rs`
- Test: same file, `mod tests` block

**Interfaces:**
- Consumes: existing `NetworkImpl` fields `connected: Arc<PeerSet>`, `connecting: Arc<PeerSet>`, `tracked_ips: Mutex<HashMap<NodeId, TrackedIp>>`; `PeerSet::contains(&NodeId) -> bool`; `TrackedIp::{should_dial, record_attempt, addr}`; `fn manually_track(&self, NodeId, SocketAddr)` (private, callable from in-module tests).
- Produces: `fn select_dial_targets(&self, now: Instant) -> Vec<(NodeId, SocketAddr)>` and field `dialing: Mutex<std::collections::HashSet<NodeId>>` — both used by Task 2.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block (after `admit_peer_dedups_same_node_id`, before the closing `}` at line 546):

```rust
    /// A node with an in-flight dial (marked in `dialing`) is NOT re-selected
    /// by the scan dialer even after its backoff window elapses — the guard
    /// that stops duplicate concurrent dials during a slow/stalling upgrade.
    /// Regression guard for the M9.15 run_dialer TOCTOU.
    #[tokio::test]
    async fn select_dial_targets_skips_in_flight_dials() {
        let tn = TestNetwork::start().await;
        let net = tn.network();

        let node = NodeId::from_slice(&[7u8; 20]).expect("node id");
        let addr: SocketAddr = "127.0.0.1:19651".parse().expect("addr");
        net.manually_track(node, addr);

        let t0 = Instant::now();

        // First scan: fresh tracked IP is dial-ready → selected and marked.
        let first = net.select_dial_targets(t0);
        assert_eq!(first, vec![(node, addr)], "fresh tracked ip is dialed once");
        assert!(net.dialing.lock().contains(&node), "selected node is marked in-flight");

        // Backoff window (1-2s) has elapsed at t0+3s, so should_dial passes —
        // but the in-flight guard must still hold the node out. On current
        // code (no guard) this returns the node again: the RED assertion.
        let second = net.select_dial_targets(t0 + Duration::from_secs(3));
        assert!(second.is_empty(), "in-flight dial is not re-launched");

        // Dial completes (task cleared the mark): the node is dial-ready again.
        net.dialing.lock().remove(&node);
        let third = net.select_dial_targets(t0 + Duration::from_secs(6));
        assert_eq!(third, vec![(node, addr)], "re-dial once the in-flight guard clears");

        net.start_close();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-network -E 'test(select_dial_targets_skips_in_flight_dials)'`
Expected: FAIL — first with a compile error (`no field dialing` / `no method select_dial_targets`). That compile failure IS the red state; proceed to implement.

- [ ] **Step 3: Add the `dialing` field**

In `struct NetworkImpl`, immediately after the `connected: Arc<PeerSet>,` line (line 59):

```rust
    /// Node-ids with an in-flight outbound dial (spawned by `handle_dial`, not
    /// yet admitted or failed). The scan dialer skips these so a slow/stalling
    /// TLS upgrade does not accumulate duplicate concurrent dials to the same
    /// peer — Go runs one dial goroutine per tracked IP; this guard set is the
    /// scan-loop equivalent. Cleared by `DialGuard` on every `handle_dial` exit.
    dialing: Mutex<std::collections::HashSet<NodeId>>,
```

In the constructor, immediately after `connecting: Arc::new(PeerSet::new()),` (line 130) — keep it grouped with the other peer sets, but note `connected`'s init line also lives nearby; place it right after the `peers_lock: Mutex::new(()),` line (line 132) is also fine. Use:

```rust
            dialing: Mutex::new(std::collections::HashSet::new()),
```

- [ ] **Step 4: Add the `select_dial_targets` helper**

Insert this method just above `async fn run_dialer` (line 324):

```rust
    /// Scan the tracked-IP table for peers to (re)dial at `now`. Skips any node
    /// already `connected`, `connecting`, or with an in-flight dial (`dialing`),
    /// applies the reconnect backoff, records the attempt, and marks each
    /// selected node as in-flight before returning it. Factored out of the
    /// dialer loop so the in-flight guard is deterministically unit-testable.
    fn select_dial_targets(&self, now: Instant) -> Vec<(NodeId, SocketAddr)> {
        let mut tracked = self.tracked_ips.lock();
        let mut dialing = self.dialing.lock();
        let mut out = Vec::new();
        for (n, t) in tracked.iter_mut() {
            if self.connected.contains(n) || self.connecting.contains(n) || dialing.contains(n) {
                continue;
            }
            if !t.should_dial(now) {
                continue;
            }
            t.record_attempt(now);
            dialing.insert(*n);
            out.push((*n, t.addr));
        }
        out
    }
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -p ava-network -E 'test(select_dial_targets_skips_in_flight_dials)'`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ava-network/src/network/net_impl.rs
git commit -m "fix(M9.15): in-flight dialing guard + select_dial_targets helper (run_dialer TOCTOU)"
```

---

### Task 2: Wire the guard into `run_dialer` + clear it in `handle_dial`

**Files:**
- Modify: `crates/ava-network/src/network/net_impl.rs`
- Test: same file, `mod tests` block

**Interfaces:**
- Consumes: `select_dial_targets` and the `dialing` field from Task 1.
- Produces: `fn handle_dial(self: &Arc<Self>, node_id: NodeId, addr: SocketAddr)` (new signature) and `struct DialGuard`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    /// The `DialGuard` clears a node's in-flight mark when the dial task exits
    /// (dropped), re-opening it for a future scan. Guards the clear-path that
    /// keeps a peer from being permanently locked out after a dial completes.
    #[tokio::test]
    async fn dial_guard_clears_in_flight_mark_on_drop() {
        let tn = TestNetwork::start().await;
        let net = tn.network();

        let node = NodeId::from_slice(&[8u8; 20]).expect("node id");
        net.dialing.lock().insert(node);
        assert!(net.dialing.lock().contains(&node), "precondition: node marked in-flight");

        {
            let _guard = DialGuard { net: Arc::clone(net), node };
            assert!(net.dialing.lock().contains(&node), "guard alive: mark held");
        }
        assert!(!net.dialing.lock().contains(&node), "guard dropped: mark cleared");

        net.start_close();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-network -E 'test(dial_guard_clears_in_flight_mark_on_drop)'`
Expected: FAIL — compile error (`cannot find struct DialGuard`).

- [ ] **Step 3: Add `DialGuard`**

Insert just above `fn handle_dial` (line 264):

```rust
/// Removes `node` from the in-flight `dialing` set on drop, covering every exit
/// path of the `handle_dial` task (dial failure, upgrade failure, admit) as well
/// as task cancellation.
struct DialGuard {
    net: Arc<NetworkImpl>,
    node: NodeId,
}

impl Drop for DialGuard {
    fn drop(&mut self) {
        self.net.dialing.lock().remove(&self.node);
    }
}
```

- [ ] **Step 4: Thread `node_id` through `handle_dial` and construct the guard**

Change the signature (line 264) from:

```rust
    fn handle_dial(self: &Arc<Self>, addr: SocketAddr) {
        let this = Arc::clone(self);
        self.tasks.spawn(async move {
```

to:

```rust
    fn handle_dial(self: &Arc<Self>, node_id: NodeId, addr: SocketAddr) {
        let this = Arc::clone(self);
        self.tasks.spawn(async move {
            // Clear the in-flight mark on any exit path (Task-1 `select_dial_targets`
            // set it before spawning us).
            let _dial_guard = DialGuard { net: Arc::clone(&this), node: node_id };
```

Leave the rest of the task body unchanged. (The existing `admit_peer` call inside still runs; the guard only clears the `dialing` mark.)

- [ ] **Step 5: Rewrite the `run_dialer` tick body to use the helper**

Replace the `_ = ticker.tick()` arm body (lines 330–350, the block that builds `targets` inline and the `for (node, addr) in targets { let _ = node; self.handle_dial(addr); }`) with:

```rust
                _ = ticker.tick() => {
                    for (node, addr) in self.select_dial_targets(Instant::now()) {
                        self.handle_dial(node, addr);
                    }
                }
```

- [ ] **Step 6: Run the guard test + the Task-1 test**

Run: `cargo nextest run -p ava-network -E 'test(dial_guard_clears_in_flight_mark_on_drop) + test(select_dial_targets_skips_in_flight_dials)'`
Expected: both PASS.

- [ ] **Step 7: Run the full crate + node dialer tests**

Run: `cargo nextest run -p ava-network -p ava-node`
Expected: all PASS (no regressions in the beacon/dialer e2e tests).

- [ ] **Step 8: Lint**

Run: `./scripts/run_task.sh lint`
Expected: clippy `-D warnings` + rustfmt + license all clean.

- [ ] **Step 9: Commit**

```bash
git add crates/ava-network/src/network/net_impl.rs
git commit -m "fix(M9.15): wire in-flight dialing guard through run_dialer/handle_dial"
```

---

## Self-Review

**1. Spec coverage:**
- Design §1 (data structure & `select_dial_targets`) → Task 1. ✓
- Design §2 (clear-path `DialGuard` + `handle_dial` param) → Task 2. ✓
- Design §3 (deterministic tests: 3-step select test + DialGuard drop test) → Task 1 Step 1 + Task 2 Step 1. ✓
- Verification (nextest -p ava-network + -p ava-node, lint) → Task 2 Steps 7–8. ✓
- Non-goals (no TrackedIp change, no per-IP restructure, no admit_peer change) → respected; run_dialer body only swaps to the helper, backoff untouched. ✓

**2. Placeholder scan:** No TBD/TODO/"handle edge cases"/"similar to". All code shown in full. ✓

**3. Type consistency:** `dialing: Mutex<HashSet<NodeId>>` referenced identically in Tasks 1 & 2. `select_dial_targets(&self, now: Instant) -> Vec<(NodeId, SocketAddr)>` matches its callers. `handle_dial(self, node_id: NodeId, addr: SocketAddr)` matches the `run_dialer` call site. `DialGuard { net, node }` fields consistent between definition (Task 2 Step 3) and construction (Task 2 Step 4, test Task 2 Step 1). ✓
