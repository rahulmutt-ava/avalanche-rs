# M9.15 Networked Peer Handshake → Bootstrap-to-Finished Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `avalanchers` complete a real networked peer handshake and bootstrap a chain from a beacon to `Phase::Finished`, proven between two Rust nodes over localhost TLS, with the live Go-interop failure diagnosed in parallel.

**Architecture:** The consensus engine is already complete (`bootstrap/mod.rs` requester, `getter.rs` responder, `engine_adapter.rs` dispatch, `ChainRouter::handle_inbound` routing). The missing work is *glue*: (1) wire the `Getter` into the per-state engine adapters so a beacon answers `Get*`; (2) decode inbound p2p bytes into engine `InboundOp`s in the node-assembly `RouterBridge` (the inverse of the existing `OutboundSender`); (3) gate bootstrap start on beacon connectivity + forward peer lifecycle; (4) apply the defined-but-unused reconnect backoff. Then a two-node localhost-TLS test drives bootstrap end-to-end. Track 2 adds app-level handshake logging and a live-Go diagnosis.

**Tech Stack:** Rust (Cargo workspace), `tokio`, `rustls` (TLS 1.3), `ava-message` (p2p codec), `ava-engine`/`ava-network`/`ava-node`/`avalanchers` crates, `cargo-nextest`, `mockall`.

## Global Constraints

- License header on every `.rs`: `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` / `// See the file LICENSE for licensing terms.`
- 4-space indent, LF, final newline. Imports grouped std → external → crate (`StdExternalCrate`).
- **No `unwrap()`/`expect()`/`dbg!`/`todo!` in library code** (clippy denies). Use `?` + per-crate `thiserror` `Error`.
- `#![forbid(unsafe_code)]` everywhere except audited FFI.
- No floating-point or `HashMap` iteration order in consensus/codec paths.
- No wall-clock reads in new schedulable logic — make backoff timing a pure function of an injected `Instant`/clock so it is deterministically testable.
- Live arms gated `#[cfg(feature = "live")] #[ignore]`; CI-runnable arms always on.
- Verify each crate with `./scripts/run_task.sh` tasks; end every worktree wave with a full-workspace `cargo nextest` + `cargo fmt --check` in the main tree (stale-binary worktree gotcha).
- Commit after every green step.

---

## File Structure

| File | Responsibility | Task |
|------|----------------|------|
| `crates/ava-network/src/network/tracked_ip.rs` | add pure backoff-schedule fields/methods | 1 |
| `crates/ava-network/src/network/net_impl.rs` | apply backoff in the dial scan | 1 |
| `crates/ava-engine/src/networking/engine_adapter.rs` | dispatch `Get*` to a shared `Getter` in both adapters | 2 |
| `crates/ava-node/src/init/inbound_decode.rs` (new) | decode `ava_message` inbound → engine `InboundOp` + chain id | 3 |
| `crates/ava-node/src/init/networking.rs` | `RouterBridge::handle_inbound` → decode → `engine_router.handle_inbound`; lifecycle forwarding | 3,4 |
| `crates/avalanchers/src/wiring/chains.rs` | gate bootstrap `start` on `on_sufficiently_connected` | 4 |
| `crates/avalanchers/tests/networked_bootstrap.rs` (new) | two-node localhost-TLS bootstrap-to-Finished | 5 |
| `crates/ava-network/src/peer/handshake.rs`, `upgrader.rs` | app-level `tracing` at each handshake rung | 6 |
| `tests/differential/src/livenet.rs`, `tests/differential/tests/mixed_network.rs` | live capture + fix-or-record | 7 |

---

## Track 1 — Two-Rust-node networked bootstrap (CI spine)

### Task 1: Reconnect backoff in the dialer (G4)

**Files:**
- Modify: `crates/ava-network/src/network/tracked_ip.rs`
- Modify: `crates/ava-network/src/network/net_impl.rs` (`run_dialer`, ~264-283; `handle_dial`, ~212-238; the connected/closed transitions in `watch_peer`, ~146-179)
- Test: `crates/ava-network/src/network/tracked_ip.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: existing `TrackedIp { addr, delay }`, `INITIAL_RECONNECT_DELAY` (1s), `MAX_RECONNECT_DELAY` (60s), `increase_delay()`, `reset_delay()`.
- Produces: `TrackedIp::next_attempt: Instant` field; `TrackedIp::should_dial(&self, now: Instant) -> bool`; `TrackedIp::record_attempt(&mut self, now: Instant)` (sets `next_attempt = now + delay` and increases delay); `TrackedIp::record_success(&mut self, now: Instant)` (resets delay, clears the gate). The dialer calls `should_dial(Instant::now())` before dialing and `record_attempt` after launching a dial; the connect-success path calls `record_success`.

- [ ] **Step 1: Write the failing test** in `tracked_ip.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use super::*;

    fn addr() -> SocketAddr {
        "127.0.0.1:9651".parse().expect("addr")
    }

    #[test]
    fn backoff_gates_redials_and_grows_then_resets() {
        let t0 = Instant::now();
        let mut ip = TrackedIp::new(addr());
        // Fresh: dial immediately.
        assert!(ip.should_dial(t0), "fresh tracked ip should dial");

        // After an attempt, the next dial is gated by the (initial 1s) delay,
        // and the delay doubles for the following attempt.
        ip.record_attempt(t0);
        assert!(!ip.should_dial(t0), "must wait out the backoff window");
        assert!(
            !ip.should_dial(t0 + Duration::from_millis(999)),
            "still inside the 1s window"
        );
        assert!(
            ip.should_dial(t0 + Duration::from_secs(1)),
            "dial once the window elapses"
        );

        // Second failed attempt: window is now 2s.
        let t1 = t0 + Duration::from_secs(1);
        ip.record_attempt(t1);
        assert!(!ip.should_dial(t1 + Duration::from_millis(1999)), "2s window");
        assert!(ip.should_dial(t1 + Duration::from_secs(2)), "after 2s window");

        // Cap at MAX_RECONNECT_DELAY.
        for _ in 0..10 {
            let now = ip.next_attempt;
            ip.record_attempt(now);
        }
        assert_eq!(ip.delay, MAX_RECONNECT_DELAY, "backoff caps at the maximum");

        // A successful connection resets the backoff and re-opens dialing.
        let t2 = ip.next_attempt;
        ip.record_success(t2);
        assert_eq!(ip.delay, INITIAL_RECONNECT_DELAY, "success resets backoff");
        assert!(ip.should_dial(t2), "success re-opens dialing");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-network -E 'test(backoff_gates_redials_and_grows_then_resets)'`
Expected: FAIL — `should_dial`/`record_attempt`/`record_success`/`next_attempt` do not exist.

- [ ] **Step 3: Add the fields/methods** to `TrackedIp`:

```rust
use std::time::Instant;
// ... in the struct:
//     /// The earliest instant the dialer may (re)attempt this IP.
//     pub next_attempt: Instant,

impl TrackedIp {
    // In `new`, initialize `next_attempt: Instant::now()` so a fresh IP dials at once.

    /// Whether the dialer may attempt this IP at `now` (backoff window elapsed).
    #[must_use]
    pub fn should_dial(&self, now: Instant) -> bool {
        now >= self.next_attempt
    }

    /// Record a (failed/pending) dial attempt at `now`: gate the next attempt by
    /// the current delay, then grow the backoff.
    pub fn record_attempt(&mut self, now: Instant) {
        self.next_attempt = now + self.delay;
        self.increase_delay();
    }

    /// Record a successful connection at `now`: reset the backoff and re-open dialing.
    pub fn record_success(&mut self, now: Instant) {
        self.reset_delay();
        self.next_attempt = now;
    }
}
```

(`Instant` is not `const`-friendly, so `new` keeps `pub fn` and sets `next_attempt: Instant::now()`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p ava-network -E 'test(backoff_gates_redials_and_grows_then_resets)'`
Expected: PASS.

- [ ] **Step 5: Apply the gate in `run_dialer`** (`net_impl.rs`): when collecting targets, hold the `tracked_ips` lock long enough to (a) skip entries where `!t.should_dial(Instant::now())`, and (b) call `t.record_attempt(now)` on the entries selected for dial. Keep the existing `connected`/`connecting` skip. On a successful upgrade in the connect path (`watch_peer`/`handle_dial` success), look up the node's `TrackedIp` and call `record_success(Instant::now())`.

```rust
// In run_dialer's tick arm, replace the target collection:
let now = std::time::Instant::now();
let targets: Vec<(NodeId, SocketAddr)> = {
    let mut tracked = self.tracked_ips.lock();
    let mut out = Vec::new();
    for (n, t) in tracked.iter_mut() {
        if self.connected.contains(n) || self.connecting.contains(n) {
            continue;
        }
        if !t.should_dial(now) {
            continue;
        }
        t.record_attempt(now);
        out.push((*n, t.addr));
    }
    out
};
for (node, addr) in targets {
    self.handle_dial(addr);
    let _ = node;
}
```

For `record_success`: in the peer-connected transition (where a peer moves `connecting → connected`), add — under the `tracked_ips` lock — `if let Some(t) = tracked.get_mut(&node_id) { t.record_success(Instant::now()); }`. (Locate the success point in `watch_peer`/`handle_dial`; cite the exact line when implementing.)

- [ ] **Step 6: Verify + commit**

Run: `cargo nextest run -p ava-network && ./scripts/run_task.sh lint`
Expected: all `ava-network` tests pass (no `connecting`-skip regressions), clippy clean.

```bash
git add crates/ava-network/src/network/tracked_ip.rs crates/ava-network/src/network/net_impl.rs
git commit -m "fix(ava-network): apply reconnect backoff in the dialer (M9.15 G4)"
```

---

### Task 2: Wire the `Getter` responder into both engine adapters (G3)

**Files:**
- Modify: `crates/ava-engine/src/networking/engine_adapter.rs`
- Test: `crates/ava-engine/src/networking/engine_adapter.rs` (`#[cfg(test)] mod tests`) or an existing engine test module that has a `TestVm` + recording `Sender`.

**Interfaces:**
- Consumes: `Getter<V, S>` (`getter.rs`) with `get_accepted_frontier(node, req)`, `get_ancestors(node, req, container_id)`, `get_accepted(node, req, &[Id])`, `get(node, req, container_id)`; `InboundOp::{Get, GetAncestors-as-request, GetAcceptedFrontier-as-request, GetAccepted-as-request}`.
- **Note:** `InboundOp` currently models the *failed* variants for these (e.g. `GetAncestorsFailed`) but the *request* variants `GetAncestors { request_id, container_id }`, `GetAcceptedFrontier { request_id }`, `GetAccepted { request_id, container_ids }` are **not present** in `router.rs`. Add them (see Step 1). `Get { request_id, container_id }` already exists.
- Produces: both `BootstrapperEngineAdapter` and `SnowmanEngineAdapter` hold `getter: Arc<Getter<V, S>>` and dispatch `Get`/`GetAncestors`/`GetAcceptedFrontier`/`GetAccepted` request ops to it (instead of dropping at `_ =>`). Update `BootstrapperEngineAdapter::new` / `SnowmanEngineAdapter::new` to take the `Arc<Getter<V, S>>`.

- [ ] **Step 1: Add the request `InboundOp` variants** in `crates/ava-engine/src/networking/router.rs` (after the existing `Get`):

```rust
    /// `GetAcceptedFrontier` request — reply with our last-accepted frontier.
    GetAcceptedFrontier {
        /// Wire request ID.
        request_id: u32,
    },
    /// `GetAccepted` request — reply with the accepted subset of `container_ids`.
    GetAccepted {
        /// Wire request ID.
        request_id: u32,
        /// The queried container ids.
        container_ids: Vec<Id>,
    },
    /// `GetAncestors` request — reply with the block + best-effort ancestry.
    GetAncestors {
        /// Wire request ID.
        request_id: u32,
        /// The requested container id.
        container_id: Id,
    },
```

- [ ] **Step 2: Write the failing test** (in `engine_adapter.rs` tests): build a `Getter` over a `TestVm` seeded with a known last-accepted id + a recording `Sender`; wrap it in a `BootstrapperEngineAdapter`; call `handle(node, InboundOp::GetAcceptedFrontier { request_id: 7 })`; assert the recording sender observed `send_accepted_frontier(node, 7, <the seeded last-accepted id>)`.

```rust
#[tokio::test]
async fn bootstrap_adapter_answers_get_accepted_frontier_via_getter() {
    // Build TestVm (last_accepted = KNOWN_ID), recording Sender, Getter,
    // and a BootstrapperEngineAdapter wrapping a Bootstrapper + the getter.
    // ... (use the crate's existing TestVm + recording-Sender helpers)
    adapter
        .handle(node, InboundOp::GetAcceptedFrontier { request_id: 7 })
        .await;
    let sent = recorder.accepted_frontier_calls();
    assert_eq!(sent, vec![(node, 7, KNOWN_ID)], "getter served frontier");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo nextest run -p ava-engine -E 'test(bootstrap_adapter_answers_get_accepted_frontier_via_getter)'`
Expected: FAIL — adapter drops `GetAcceptedFrontier` (`_ => {}`), recorder empty.

- [ ] **Step 4: Implement** — give both adapters a `getter: Arc<Getter<V, S>>`, and in each `handle()` route the request ops to it before the `_ =>` arm:

```rust
// In BootstrapperEngineAdapter::handle (and identically in SnowmanEngineAdapter::handle):
InboundOp::GetAcceptedFrontier { request_id } => {
    if let Err(err) = self.getter.get_accepted_frontier(node, request_id).await {
        log_engine_error("getter.get_accepted_frontier", &err);
    }
}
InboundOp::GetAncestors { request_id, container_id } => {
    if let Err(err) = self.getter.get_ancestors(node, request_id, container_id).await {
        log_engine_error("getter.get_ancestors", &err);
    }
}
InboundOp::GetAccepted { request_id, container_ids } => {
    if let Err(err) = self.getter.get_accepted(node, request_id, &container_ids).await {
        log_engine_error("getter.get_accepted", &err);
    }
}
InboundOp::Get { request_id, container_id } => {
    if let Err(err) = self.getter.get(node, request_id, container_id).await {
        log_engine_error("getter.get", &err);
    }
}
```

Update both `::new` signatures to accept and store `getter: Arc<Getter<V, S>>`, and update every construction site (chain-boot wiring in `avalanchers`/`ava-chains`) to build the `Getter` (sharing the same `Arc<Mutex<V>>` + `Arc<S>` the engines use) and pass it in.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -p ava-engine -E 'test(bootstrap_adapter_answers_get_accepted_frontier_via_getter)'`
Expected: PASS.

- [ ] **Step 6: Verify + commit**

Run: `cargo nextest run -p ava-engine && cargo build --workspace`
Expected: engine tests pass; workspace builds (all `::new` call-sites updated).

```bash
git add crates/ava-engine/src/networking/engine_adapter.rs crates/ava-engine/src/networking/router.rs
git commit -m "feat(ava-engine): answer Get* via the Getter in both engine adapters (M9.15 G3)"
```

---

### Task 3: Decode inbound p2p → engine `InboundOp` and route it (G1)

**Files:**
- Create: `crates/ava-node/src/init/inbound_decode.rs`
- Modify: `crates/ava-node/src/init/networking.rs` (`RouterBridge::handle_inbound`, lines 80-88) + `crates/ava-node/src/init/mod.rs` (register `mod inbound_decode;`)
- Test: `crates/ava-node/src/init/inbound_decode.rs` tests + a `RouterBridge` routing test in `networking.rs`.

**Interfaces:**
- Consumes: `ava_message::codec::InboundMessage { op: Op, .. }` (the network-layer message handed to `handle_inbound`), `ava_engine::networking::router::{InboundOp, InboundMessage as EngineInboundMessage, Router}`. Inspect `ava_message::Op` + payload accessors to map each consensus op to its `InboundOp` (request_id, chain id, container(s)/id(s)/heights). Mirror the inverse of `ava_engine::networking::sender::OutboundSender` which builds these via `ava_message::MsgBuilder::create_outbound`.
- Produces: `pub fn decode_inbound(node: NodeId, msg: &ava_message::codec::InboundMessage) -> Option<EngineInboundMessage>` returning `None` for ops the engine does not consume (Ping/Pong/Handshake/PeerList/etc., already handled at the peer layer). `RouterBridge::handle_inbound` calls it and forwards to `engine_router.handle_inbound`.

- [ ] **Step 1: Write the failing test** in `inbound_decode.rs`: build an outbound `GetAcceptedFrontier` for a known chain via the same `ava_message` builder the `OutboundSender` uses, parse it back into an `ava_message::codec::InboundMessage`, run `decode_inbound`, and assert the engine op + chain.

```rust
#[test]
fn decodes_get_accepted_frontier() {
    // Build the wire bytes for GetAcceptedFrontier{chain, request_id: 9}
    // using the ava-message builder; parse into codec::InboundMessage `m`.
    let node = NodeId::from([1u8; 20]);
    let got = decode_inbound(node, &m).expect("decode");
    assert_eq!(got.chain, CHAIN);
    assert_eq!(got.node, node);
    assert_eq!(got.op, InboundOp::GetAcceptedFrontier { request_id: 9 });
}

#[test]
fn drops_non_consensus_ops() {
    // A Ping (or PeerList) message decodes to None.
    assert!(decode_inbound(NodeId::from([2u8; 20]), &ping).is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-node -E 'test(decodes_get_accepted_frontier)'`
Expected: FAIL — `decode_inbound` does not exist.

- [ ] **Step 3: Implement `decode_inbound`** — `match` on `msg.op`, extracting fields, returning `Some(EngineInboundMessage { node, chain, op })` for each consensus op (`GetAcceptedFrontier`/`AcceptedFrontier`/`GetAccepted`/`Accepted`/`GetAncestors`/`Ancestors`/`Get`/`Put`/`PushQuery`/`PullQuery`/`Chits`) and `None` otherwise. Convert chain bytes → `Id`, container bytes → `Vec<u8>`, ids → `Id`. (Read `ava_message::Op` + the `OutboundSender` encode to get exact field names; no `unwrap` — map malformed fields to `None`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p ava-node -E 'test(decodes_get_accepted_frontier) + test(drops_non_consensus_ops)'`
Expected: PASS.

- [ ] **Step 5: Wire `RouterBridge::handle_inbound`** to use it:

```rust
async fn handle_inbound(&self, _ctx: &CancellationToken, msg: InboundMessage) {
    let Some(router) = self.engine_router() else {
        tracing::debug!(op = ?msg.op, "no engine router yet; dropping inbound");
        return;
    };
    match crate::init::inbound_decode::decode_inbound(/* sender node id */, &msg) {
        Some(engine_msg) => router.handle_inbound(engine_msg).await,
        None => tracing::trace!(op = ?msg.op, "non-consensus inbound; ignored"),
    }
}
```

(`InboundMessage` may not carry the sender NodeId; if so, thread the sender NodeId into `handle_inbound`'s signature from the peer actor — check `ava_network::router::InboundHandler` and the peer call-site. If that requires a signature change, fold it here and update the trait + all impls in this file.)

- [ ] **Step 6: Write a RouterBridge routing test** (in `networking.rs` tests): a recording `EngineRouter` stub; `set_engine_router`; feed a decoded-able `InboundMessage`; assert the engine router received the converted `EngineInboundMessage`.

- [ ] **Step 7: Verify + commit**

Run: `cargo nextest run -p ava-node && ./scripts/run_task.sh lint`
Expected: PASS, clippy clean.

```bash
git add crates/ava-node/src/init/inbound_decode.rs crates/ava-node/src/init/networking.rs crates/ava-node/src/init/mod.rs
git commit -m "feat(ava-node): decode inbound p2p into engine ops and route them (M9.15 G1)"
```

---

### Task 4: Forward peer lifecycle + gate bootstrap start on beacon connectivity (G2)

**Files:**
- Modify: `crates/ava-node/src/init/networking.rs` (`RouterBridge::connected`/`disconnected`, lines 90-99)
- Modify: `crates/avalanchers/src/wiring/chains.rs` (`boot_chain_over_network` / `boot_chain_with_sender`) — gate the bootstrapper `start` on `on_sufficiently_connected`.
- Test: `crates/avalanchers/tests/` — a unit test that the bootstrapper does not send `GetAcceptedFrontier` until the watch fires.

**Interfaces:**
- Consumes: `Networking::on_sufficiently_connected: watch::Receiver<bool>` (already fired by `BeaconManager` at the `(3·n+3)/4` threshold), the `BootstrapperEngineAdapter::start` hook (called by `ChainHandler` on launch).
- Produces: bootstrap `start` is deferred until `on_sufficiently_connected` observes `true`; `RouterBridge::connected`/`disconnected` log at `info` and (if a peer surface lands on the engine router) forward there. **Decision:** do **not** invent an engine peer-surface; beacon counting already lives in `BeaconManager`. The only behavioral change needed is the start-gate.

- [ ] **Step 1: Write the failing test** — boot a follower chain whose beacon set is non-empty over a recording network, assert the bootstrapper sends **no** `GetAcceptedFrontier` before `on_sufficiently_connected` is set, then set it and assert the frontier request is sent. (Model on `avalanchers/tests/outbound_sender_boot.rs`.)

```rust
#[tokio::test]
async fn bootstrap_waits_for_sufficient_beacons_before_frontier() {
    // boot_chain_over_network with one beacon, recording Network.
    // Before connectivity: no GetAcceptedFrontier recorded.
    assert!(net.recorded_get_accepted_frontier().is_empty(), "no premature frontier");
    // Signal connectivity:
    connected_tx.send(true).expect("signal");
    // Now the bootstrapper starts and broadcasts the frontier request.
    wait_until(|| !net.recorded_get_accepted_frontier().is_empty()).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p avalanchers -E 'test(bootstrap_waits_for_sufficient_beacons_before_frontier)'`
Expected: FAIL — bootstrap `start` fires immediately on handler launch, sending the frontier request with no connectivity gate.

- [ ] **Step 3: Implement the gate** — in the chain-boot path, before the `ChainHandler` activates the bootstrapper (or inside the `BootstrapperEngineAdapter::start` hook via an injected `watch::Receiver<bool>`), `await` `on_sufficiently_connected.wait_for(|&v| v)` (or `changed()` loop) so `start` proceeds only once the threshold is met. Thread the receiver from `Networking` into the boot path. Keep the no-beacon case (threshold already `true`) starting immediately.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p avalanchers -E 'test(bootstrap_waits_for_sufficient_beacons_before_frontier)'`
Expected: PASS.

- [ ] **Step 5: Upgrade lifecycle logging** in `RouterBridge::connected`/`disconnected` from `debug` to `info` with structured fields (these are operationally important and feed Track 2). No engine forwarding (decision above).

- [ ] **Step 6: Verify + commit**

Run: `cargo nextest run -p avalanchers && ./scripts/run_task.sh lint`
Expected: PASS (all pre-existing boot tests still green), clippy clean.

```bash
git add crates/avalanchers/src/wiring/chains.rs crates/ava-node/src/init/networking.rs crates/avalanchers/tests/
git commit -m "feat(avalanchers): gate bootstrap start on beacon connectivity (M9.15 G2)"
```

---

### Task 5: Two-node localhost-TLS bootstrap-to-Finished integration test (G5)

**Files:**
- Create: `crates/avalanchers/tests/networked_bootstrap.rs`
- Test: itself.

**Interfaces:**
- Consumes: `boot_chain_over_network` (Task 4), `NetworkImpl::new_with_metrics`, the `Getter`-wired engine (Task 2), the inbound decoder (Task 3), the backoff dialer (Task 1). Uses a `TestVm` (in `ava-engine`/`ava-vm` testutil) with a synthetic block chain so bootstrap has real blocks to fetch.
- Produces: a CI-runnable test proving B reaches `Phase::Finished` and `B.last_accepted == A.tip`.

- [ ] **Step 1: Write the failing test** — two `NetworkImpl` instances bound to `127.0.0.1:0`, each with a generated ECDSA staking identity. Node A: `TestVm` seeded with N≥2 accepted blocks, engine in `NormalOp` (so its `Getter` answers). Node B: same `TestVm` type at genesis, A configured as its sole beacon (B `manually_track(A.node_id, A.addr)`, A in B's bootstrapper set). Drive both event loops; poll B's bootstrapper phase.

```rust
#[tokio::test]
async fn follower_bootstraps_from_beacon_to_finished() {
    // ... bring up A (beacon, tip at height H) and B (follower).
    // B tracks A and lists A as its only beacon.
    let finished = wait_until_timeout(Duration::from_secs(30), || {
        b.bootstrap_phase() == Phase::Finished
    }).await;
    assert!(finished, "follower reached Phase::Finished");
    assert_eq!(b.last_accepted(), a.tip(), "follower converged on beacon tip");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p avalanchers -E 'test(follower_bootstraps_from_beacon_to_finished)'`
Expected: FAIL initially (handshake/route/getter path exercised end-to-end for the first time; iterate on real failures, not by weakening the assert).

- [ ] **Step 3: Make it pass** — debug the real bring-up via the Task 6 logging: confirm handshake completes (both `connected` fire), B sends `GetAcceptedFrontier`, A answers, B fetches ancestors to `Finished`. Fix glue gaps surfaced here in the owning crate (do not special-case the test).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p avalanchers -E 'test(follower_bootstraps_from_beacon_to_finished)'`
Expected: PASS.

- [ ] **Step 5: Full-workspace verify + commit**

Run (main tree): `./scripts/run_task.sh lint-all && ./scripts/run_task.sh test-unit`
Expected: workspace green, fmt clean.

```bash
git add crates/avalanchers/tests/networked_bootstrap.rs
git commit -m "test(avalanchers): two-node localhost-TLS bootstrap-to-Finished (M9.15 G5)"
```

---

## Track 2 — Live Go diagnosis (parallel, single-track)

### Task 6: App-level handshake logging (D1)

**Files:**
- Modify: `crates/ava-network/src/peer/handshake.rs` (`handle_handshake` ~29-114, each validation branch; `handle_peer_list` ~121-131; `finish_handshake` ~216-231)
- Modify: `crates/ava-network/src/peer/upgrader.rs` (`upgrade` ~97-139, post-TLS NodeID derivation)
- Test: assert via an existing handshake test that logs are emitted (or just confirm compilation + manual `RUST_LOG` capture; logging has no behavioral assertion).

**Interfaces:**
- Consumes: existing handshake validation flow. Produces: structured `tracing` events (target `ava_network::peer::handshake`) at: TLS upgraded (peer NodeID + cert key type), Handshake received (network id, version, claimed ip, #subnets), each rejection branch (reason + offending value), PeerList received, `finish_handshake` (the rung that currently never logs).

- [ ] **Step 1: Add `tracing` calls** at each rung. Each rejection branch logs `tracing::info!(reason = "...", ...)` before closing; success rungs log `tracing::debug!`. Example for the signed-IP branch:

```rust
// before returning the signed-IP rejection:
tracing::info!(
    %node_id,
    claimed_ip = %claimed_addr,
    "handshake rejected: signed-IP signature invalid"
);
```

- [ ] **Step 2: Verify it compiles + lints**

Run: `cargo build -p ava-network && ./scripts/run_task.sh lint`
Expected: clean.

- [ ] **Step 3: Confirm existing handshake tests still pass** (logging is additive)

Run: `cargo nextest run -p ava-network -E 'test(handshake)'`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/ava-network/src/peer/handshake.rs crates/ava-network/src/peer/upgrader.rs
git commit -m "feat(ava-network): app-level handshake rung logging (M9.15 D1)"
```

---

### Task 7: Live capture + fix-or-record (D2/D3) — investigative

**Files:**
- Use: `tests/differential/src/livenet.rs`, `tests/differential/tests/mixed_network.rs` (the merge-ready `m9.15-live-mixed-net` harness), `~/avalanchego/build/avalanchego`.
- Possibly modify: `crates/ava-network/src/peer/{handshake,ip_signer,upgrader}.rs` (the fix, once located).

> **This task is investigative; its outcome is a root-cause + either a fix or an honest recorded finding.** It cannot be pre-written as mechanical steps. The leading hypothesis is a **signed-IP signature** mismatch (a fast post-TLS reject fits the observed ~250ms loop); alternatives are a `Handshake` message wire incompat (ACP fields / client-version string) or a rustls↔Go TLS quirk.

- [ ] **Step 1: Verify the oracle binary matches its checkout.**

Run: `./scripts/check_oracle_binary.sh`
Expected: `OK` (rpcchainvm=45, binary commit == `~/avalanchego` HEAD). On FAIL: `cd ~/avalanchego && PATH="$HOME/.local/share/mise/installs/go/1.25.10/bin:$PATH" ./scripts/build.sh` then re-check.

- [ ] **Step 2: Build the follower + run the live arm with full logging.**

Run: `cargo build -p avalanchers --release && AVALANCHEGO_PATH=~/avalanchego/build/avalanchego RUST_LOG=ava_network=debug cargo test -p ava-differential --features live mixed_network -- --ignored --nocapture`
Expected: capture the exact handshake rung at which the Go beacon rejects/never-registers the follower (now visible via Task 6 logging).

- [ ] **Step 3: Pin the root cause.** Compare the failing rung against Go: if signed-IP, compare the signed-IP byte layout + signature scheme (`ip_signer.rs` vs Go `peer.ipSigner`); if Handshake wire, diff the `Handshake` proto fields the follower sends vs Go's expected set; if TLS, capture the rustls alert.

- [ ] **Step 4: Fix if tractable** — apply the minimal fix in the owning `ava-network` module, add a regression unit test (e.g. signed-IP bytes round-trip vs a Go-recorded vector), and re-run Step 2 to confirm the handshake completes + the follower registers.

- [ ] **Step 5: Record the outcome.** Fold the finding into `plan/M9-interop-hardening.md` (M9.15 callout) and `specs/05-networking-p2p.md` as an AS-BUILT note. If the fix is too large for this session, leave the live arm `#[cfg(feature="live")] #[ignore]` and record the precise root cause + next step. Commit the doc/fix.

```bash
git add -A
git commit -m "differential(M9.15): live handshake root-cause + fix-or-record (D2/D3)"
```

---

## Self-Review

**Spec coverage:** G1→Task 3, G2→Task 4, G3→Task 2, G4→Task 1, G5→Task 5, D1→Task 6, D2/D3→Task 7. Determinism (backoff pure-function) → Task 1 Step 3. CI gating (live `#[ignore]`) → Tasks 5/7. Non-goals (gossip dialing, BLS PoP, IP resolver) — explicitly not tasked. All spec sections covered.

**Placeholder scan:** Task 7 is intentionally investigative (a live root-cause cannot be pre-coded), but every other task carries concrete code + exact run commands. The two "cite the exact line when implementing" notes (Task 1 Step 5 success-point, Task 3 Step 5 sender-NodeId threading) are real lookups, not hand-waves — the surrounding code is specified.

**Type consistency:** `should_dial`/`record_attempt`/`record_success`/`next_attempt` (Task 1) used consistently. `decode_inbound` signature (Task 3) matches its `RouterBridge::handle_inbound` call (Task 3 Step 5). New `InboundOp::{GetAcceptedFrontier,GetAccepted,GetAncestors}` request variants (Task 2 Step 1) are consumed by the adapters (Task 2 Step 4) and produced by the decoder (Task 3 Step 3). `on_sufficiently_connected: watch::Receiver<bool>` (Task 4) matches the field in `Networking`.

**Known integration risk:** Tasks 4 and 5 cross into `avalanchers` chain-boot wiring whose exact `boot_chain_over_network` shape must be read at implementation time; the plan specifies the *behavior* (gate start on the watch) and the test, leaving the precise call-site edit to the implementer per the existing code.
