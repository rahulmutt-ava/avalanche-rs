# Bootstrap frontier/accepted failure accounting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Snowman bootstrapper tolerate beacons that never reply (Go-parity `GetAcceptedFrontierFailed`/`GetAcceptedFailed` handling) so a single lost/absent frontier reply cannot hang bootstrap, then un-gate the `follower_bootstraps_through_real_beacon_gate` end-to-end test.

**Architecture:** The bootstrapper's frontier-discovery and frontier-agreement phases currently complete only when *every* beacon replies. We add per-phase "responded-or-failed" accounting: a failure records an empty opinion that counts toward phase completion but contributes no frontier/accepted id — exactly Go's `minority/majority.RecordOpinion(node, nil)`. We wire the `*Failed` ops (already synthesized on request timeout by the router) into the bootstrapper via the engine adapter, and fix a frozen-clock in the test-boot path that otherwise disables the request-timeout backstop that synthesizes those ops.

**Tech Stack:** Rust, `cargo-nextest`, `tokio`, the `ava-engine` consensus crate, `ava-network`, `avalanchers` binary integration tests.

## Global Constraints

- License header on every `.rs` file (already present in every file this plan touches):
  `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` / `// See the file LICENSE for licensing terms.`
- `ava-engine` is **library** code: no `unwrap()`/`expect()`/`dbg!`/`todo!` (clippy denies). Test files are exempt.
- 4-space indent, LF endings, import grouping std → external → crate.
- `arithmetic_side_effects` is warn (not deny) outside `ava-saevm*`; use `wrapping_add` for request-id bumps (matches existing code).
- Run a single test with `cargo nextest run -p <crate> -E 'test(<name>)'`. Full gate: `./scripts/run_task.sh lint-all` and `./scripts/run_task.sh test-unit`.
- **Deviation from the design spec, confirmed against the code:** the existing `accepted_frontier`/`accepted` methods **do not guard on `request_id`** (their parameter is `_req`). To stay consistent, the new `*_failed` handlers also ignore `request_id` and guard on **phase + beacon membership** only. (No stale-request-id test — it would contradict the current design.)

---

### Task 1: Frontier-phase failure accounting

Add a "responded" set for the frontier phase, refactor `accepted_frontier` to use it, and add `get_accepted_frontier_failed`. A failed beacon counts toward phase completion but contributes no frontier id.

**Files:**
- Modify: `crates/ava-engine/src/snowman/bootstrap/mod.rs`
- Test: `crates/ava-engine/tests/bootstrap.rs`

**Interfaces:**
- Consumes: existing `Bootstrapper::{start, accepted_frontier, phase}`, `Config`, `Phase`, `RecordingSender`, `Sent` (test support).
- Produces: `Bootstrapper::get_accepted_frontier_failed(&mut self, node: NodeId, _req: u32) -> Result<()>` (async); new private `maybe_begin_frontier_agreement(&mut self)`; new field `frontier_responded: BTreeSet<NodeId>`.

- [ ] **Step 1: Write the failing test**

Add to `crates/ava-engine/tests/bootstrap.rs` (the imports at the top of that file — `BTreeMap`, `Mutex`, `CancellationToken`, `Bootstrapper`, `Config`, `Phase`, `Id`, `NodeId`, `TestVm`, `init_test_vm`, `RecordingSender`, `Sent`, `consensus_ctx`, `RecordingAcceptor` — already cover this test):

```rust
/// A beacon that never answers the frontier query must not hang discovery: its
/// `GetAcceptedFrontierFailed` completes the phase on the beacons that did reply.
#[tokio::test]
async fn frontier_advances_when_a_beacon_fails() {
    let token = CancellationToken::new();
    let vm: TestVm = init_test_vm(&token).await.expect("vm");
    let tip = vm.last_accepted(&token).await.expect("genesis");

    let acceptor = Arc::new(RecordingAcceptor::default());
    let ctx = consensus_ctx(acceptor.clone());
    let sender = RecordingSender::new();

    let a = NodeId::from([10u8; 20]);
    let b = NodeId::from([11u8; 20]);
    let c = NodeId::from([12u8; 20]);
    let mut beacons = BTreeMap::new();
    beacons.insert(a, 1u64);
    beacons.insert(b, 1u64);
    beacons.insert(c, 1u64);

    let cfg = Config {
        subnet_id: Id::EMPTY,
        ctx: ctx.clone(),
        vm: Arc::new(Mutex::new(vm)),
        sender: sender.clone(),
        beacons,
        token: token.clone(),
    };
    let mut boot = Bootstrapper::new(cfg);

    boot.start(0).await.expect("start");
    let _ = sender.drain();
    assert_eq!(boot.phase(), Phase::DiscoveringFrontier, "start enters DiscoveringFrontier");

    // Two of three beacons reply.
    boot.accepted_frontier(a, 1, tip).await.expect("af a");
    boot.accepted_frontier(b, 1, tip).await.expect("af b");
    assert_eq!(
        boot.phase(),
        Phase::DiscoveringFrontier,
        "still awaiting the third beacon"
    );

    // The third beacon's query failed (timeout / never connected).
    boot.get_accepted_frontier_failed(c, 1).await.expect("aff c");

    // The failure completes the phase; agreement begins with the two replies.
    assert_eq!(
        boot.phase(),
        Phase::AgreeingFrontier,
        "a failed beacon completes the frontier phase"
    );
    let sent = sender.drain();
    assert!(
        sent.iter().any(|s| matches!(s, Sent::GetAccepted { .. })),
        "expected GetAccepted after failure completes frontier, got {sent:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-engine -E 'test(frontier_advances_when_a_beacon_fails)'`
Expected: FAIL — compile error `no method named `get_accepted_frontier_failed` found`.

- [ ] **Step 3: Add the `frontier_responded` field**

In `crates/ava-engine/src/snowman/bootstrap/mod.rs`, in `struct Bootstrapper` (after the `frontier_replies` field, ~line 111):

```rust
    /// Beacons that have responded to the frontier query (reply **or** failure).
    /// Phase completion is keyed on this set, not `frontier_replies`, so a
    /// failed/absent beacon (empty opinion) still advances discovery.
    frontier_responded: BTreeSet<NodeId>,
```

In `Bootstrapper::new` (the struct literal, after `frontier_replies: BTreeMap::new(),`):

```rust
            frontier_responded: BTreeSet::new(),
```

- [ ] **Step 4: Refactor `accepted_frontier` + add the failure handler + helper**

Replace the body of `accepted_frontier` (currently ~lines 219-233) and add the two new methods immediately after it:

```rust
    pub async fn accepted_frontier(
        &mut self,
        node: NodeId,
        _req: u32,
        container_id: Id,
    ) -> Result<()> {
        if self.phase != Phase::DiscoveringFrontier || !self.cfg.beacons.contains_key(&node) {
            return Ok(());
        }
        if !self.frontier_responded.insert(node) {
            return Ok(()); // duplicate response
        }
        self.frontier_replies.insert(node, container_id);
        self.maybe_begin_frontier_agreement();
        Ok(())
    }

    /// `GetAcceptedFrontierFailed` — the beacon did not answer the frontier
    /// query (request timed out / never connected). Records an *empty opinion*
    /// (Go `minority.RecordOpinion(node, nil)`): it counts toward phase
    /// completion but contributes no frontier id, so a slow/absent beacon
    /// cannot stall discovery.
    ///
    /// # Errors
    /// Propagates a VM/acceptor error from the agreement that may follow.
    pub async fn get_accepted_frontier_failed(&mut self, node: NodeId, _req: u32) -> Result<()> {
        if self.phase != Phase::DiscoveringFrontier || !self.cfg.beacons.contains_key(&node) {
            return Ok(());
        }
        if !self.frontier_responded.insert(node) {
            return Ok(()); // duplicate response
        }
        self.maybe_begin_frontier_agreement();
        Ok(())
    }

    /// Begin frontier agreement once every beacon has responded (reply or
    /// failure).
    fn maybe_begin_frontier_agreement(&mut self) {
        if self.frontier_responded.len() == self.cfg.beacons.len() {
            self.begin_frontier_agreement();
        }
    }
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -p ava-engine -E 'test(frontier_advances_when_a_beacon_fails)'`
Expected: PASS.

- [ ] **Step 6: Run the existing bootstrap tests (no regression)**

Run: `cargo nextest run -p ava-engine -E 'test(bootstrap)'`
Expected: PASS — `bootstrap_fetches_and_executes_range`, `halt_aborts_bootstrap`, and the new test all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ava-engine/src/snowman/bootstrap/mod.rs crates/ava-engine/tests/bootstrap.rs
git commit -m "fix(ava-engine): frontier-phase failure accounting in the bootstrapper (Go parity)"
```

---

### Task 2: Accepted-phase failure accounting

Add `get_accepted_failed` for the frontier-agreement phase. The accepted phase already uses `accepted_replies: BTreeSet<NodeId>` as its responded set (reused here), tallying weight only on real replies.

**Files:**
- Modify: `crates/ava-engine/src/snowman/bootstrap/mod.rs`
- Test: `crates/ava-engine/tests/bootstrap.rs`

**Interfaces:**
- Consumes: `Bootstrapper::{start, accepted_frontier, accepted, phase}`, existing `accepted_replies`, `begin_fetching`.
- Produces: `Bootstrapper::get_accepted_failed(&mut self, node: NodeId, _req: u32) -> Result<()>` (async).

- [ ] **Step 1: Write the failing test**

Add to `crates/ava-engine/tests/bootstrap.rs`:

```rust
/// A beacon that never answers the frontier-agreement query must not hang the
/// accepted phase: its `GetAcceptedFailed` completes the phase on the beacons
/// that did reply, and fetching begins for the agreed tip.
///
/// Uses **three** beacons so the two that accept the tip carry weight 2, which
/// exceeds the `> total/2` threshold (`total = 3`, threshold `= 1`); with only
/// two beacons a single accepter (weight 1) would not exceed threshold and the
/// node would treat itself as caught up instead of fetching.
#[tokio::test]
async fn accepted_advances_when_a_beacon_fails() {
    let token = CancellationToken::new();
    let vm: TestVm = init_test_vm(&token).await.expect("vm");
    let genesis = vm.last_accepted(&token).await.expect("genesis");
    let (_chain_bytes, ids) = build_chain(genesis);
    let tip = *ids.last().expect("tip");

    let acceptor = Arc::new(RecordingAcceptor::default());
    let ctx = consensus_ctx(acceptor.clone());
    let sender = RecordingSender::new();

    let a = NodeId::from([10u8; 20]);
    let b = NodeId::from([11u8; 20]);
    let c = NodeId::from([12u8; 20]);
    let mut beacons = BTreeMap::new();
    beacons.insert(a, 1u64);
    beacons.insert(b, 1u64);
    beacons.insert(c, 1u64);

    let cfg = Config {
        subnet_id: Id::EMPTY,
        ctx: ctx.clone(),
        vm: Arc::new(Mutex::new(vm)),
        sender: sender.clone(),
        beacons,
        token: token.clone(),
    };
    let mut boot = Bootstrapper::new(cfg);

    boot.start(0).await.expect("start");
    // All three report the tip -> frontier agreement begins.
    boot.accepted_frontier(a, 1, tip).await.expect("af a");
    boot.accepted_frontier(b, 1, tip).await.expect("af b");
    boot.accepted_frontier(c, 1, tip).await.expect("af c");
    assert_eq!(boot.phase(), Phase::AgreeingFrontier, "all frontier replies in");
    let _ = sender.drain();

    // Two beacons accept the tip (weight 2 > threshold 1); the third fails.
    boot.accepted(a, 2, &[tip]).await.expect("acc a");
    boot.accepted(b, 2, &[tip]).await.expect("acc b");
    boot.get_accepted_failed(c, 2).await.expect("accf c");

    let sent = sender.drain();
    assert!(
        sent.iter().any(|s| matches!(s, Sent::GetAncestors { id, .. } if *id == tip)),
        "expected GetAncestors for the agreed tip after failure completes accepted, got {sent:?}"
    );
    assert_eq!(boot.phase(), Phase::Fetching, "accepted phase completed -> fetching");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-engine -E 'test(accepted_advances_when_a_beacon_fails)'`
Expected: FAIL — compile error `no method named `get_accepted_failed` found`.

- [ ] **Step 3: Add `get_accepted_failed`**

In `crates/ava-engine/src/snowman/bootstrap/mod.rs`, immediately after the `accepted` method (~line 268):

```rust
    /// `GetAcceptedFailed` — the beacon did not answer the frontier-agreement
    /// query. Records an *empty opinion* (Go `majority.RecordOpinion(node, nil)`):
    /// it counts toward phase completion but contributes no accepted weight.
    ///
    /// # Errors
    /// Propagates a VM error from the fetch that may follow.
    pub async fn get_accepted_failed(&mut self, node: NodeId, _req: u32) -> Result<()> {
        if self.phase != Phase::AgreeingFrontier || !self.cfg.beacons.contains_key(&node) {
            return Ok(());
        }
        if !self.accepted_replies.insert(node) {
            return Ok(()); // duplicate response
        }
        if self.accepted_replies.len() == self.cfg.beacons.len() {
            self.begin_fetching().await?;
        }
        Ok(())
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p ava-engine -E 'test(accepted_advances_when_a_beacon_fails)'`
Expected: PASS.

- [ ] **Step 5: Run the bootstrap suite**

Run: `cargo nextest run -p ava-engine -E 'test(bootstrap)'`
Expected: PASS (all bootstrap tests).

- [ ] **Step 6: Commit**

```bash
git add crates/ava-engine/src/snowman/bootstrap/mod.rs crates/ava-engine/tests/bootstrap.rs
git commit -m "fix(ava-engine): accepted-phase failure accounting in the bootstrapper (Go parity)"
```

---

### Task 3: Restart frontier discovery when every beacon fails

If all beacons fail the frontier query, there is no frontier information at all — proceeding would falsely declare the node caught up at genesis. Mirror Go ("no blocks accepted → restart bootstrap") by re-broadcasting `GetAcceptedFrontier`.

**Files:**
- Modify: `crates/ava-engine/src/snowman/bootstrap/mod.rs`
- Test: `crates/ava-engine/tests/bootstrap.rs`

**Interfaces:**
- Consumes: `maybe_begin_frontier_agreement`, `frontier_responded`, `frontier_replies`, `request_id`, `cfg.sender.send_get_accepted_frontier`.
- Produces: new private `restart_frontier_discovery(&mut self)`; modified `maybe_begin_frontier_agreement`.

- [ ] **Step 1: Write the failing test**

Add to `crates/ava-engine/tests/bootstrap.rs`:

```rust
/// When every beacon fails the frontier query, the bootstrapper must NOT declare
/// itself caught up — it restarts discovery by re-broadcasting GetAcceptedFrontier.
#[tokio::test]
async fn all_beacons_failing_restarts_frontier_discovery() {
    let token = CancellationToken::new();
    let vm: TestVm = init_test_vm(&token).await.expect("vm");

    let acceptor = Arc::new(RecordingAcceptor::default());
    let ctx = consensus_ctx(acceptor.clone());
    let sender = RecordingSender::new();

    let a = NodeId::from([10u8; 20]);
    let b = NodeId::from([11u8; 20]);
    let mut beacons = BTreeMap::new();
    beacons.insert(a, 1u64);
    beacons.insert(b, 1u64);

    let cfg = Config {
        subnet_id: Id::EMPTY,
        ctx: ctx.clone(),
        vm: Arc::new(Mutex::new(vm)),
        sender: sender.clone(),
        beacons,
        token: token.clone(),
    };
    let mut boot = Bootstrapper::new(cfg);

    boot.start(0).await.expect("start"); // first GetAcceptedFrontier
    let _ = sender.drain();

    // Both beacons fail their frontier query.
    boot.get_accepted_frontier_failed(a, 1).await.expect("aff a");
    boot.get_accepted_frontier_failed(b, 1).await.expect("aff b");

    // No agreement: still discovering, and a fresh GetAcceptedFrontier was re-sent.
    assert_eq!(
        boot.phase(),
        Phase::DiscoveringFrontier,
        "all-failed must restart, not advance/finish"
    );
    let sent = sender.drain();
    assert!(
        sent.iter().any(|s| matches!(s, Sent::GetAcceptedFrontier { .. })),
        "expected a re-broadcast GetAcceptedFrontier, got {sent:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-engine -E 'test(all_beacons_failing_restarts_frontier_discovery)'`
Expected: FAIL — after both failures the phase leaves `DiscoveringFrontier` (advances to agreement/finish) and no re-broadcast occurs.

- [ ] **Step 3: Add restart-on-empty to the completion helper**

In `crates/ava-engine/src/snowman/bootstrap/mod.rs`, replace `maybe_begin_frontier_agreement` (added in Task 1) and add `restart_frontier_discovery`:

```rust
    /// Once every beacon has responded (reply or failure): begin agreement if
    /// any frontier was reported, otherwise restart discovery (all beacons
    /// failed — no frontier information at all; Go "no blocks accepted →
    /// restart bootstrap").
    fn maybe_begin_frontier_agreement(&mut self) {
        if self.frontier_responded.len() != self.cfg.beacons.len() {
            return;
        }
        if self.frontier_replies.is_empty() {
            self.restart_frontier_discovery();
            return;
        }
        self.begin_frontier_agreement();
    }

    /// Re-broadcast `GetAcceptedFrontier` under a fresh request id after every
    /// beacon failed, clearing the responded set so the new round can complete.
    fn restart_frontier_discovery(&mut self) {
        self.frontier_responded.clear();
        self.frontier_replies.clear();
        self.request_id = self.request_id.wrapping_add(1);
        let beacons: std::collections::HashSet<NodeId> =
            self.cfg.beacons.keys().copied().collect();
        self.cfg
            .sender
            .send_get_accepted_frontier(&beacons, self.request_id);
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p ava-engine -E 'test(all_beacons_failing_restarts_frontier_discovery)'`
Expected: PASS.

- [ ] **Step 5: Run the bootstrap suite**

Run: `cargo nextest run -p ava-engine -E 'test(bootstrap)'`
Expected: PASS (all bootstrap tests, including Tasks 1–2).

- [ ] **Step 6: Commit**

```bash
git add crates/ava-engine/src/snowman/bootstrap/mod.rs crates/ava-engine/tests/bootstrap.rs
git commit -m "fix(ava-engine): restart frontier discovery when all beacons fail (Go parity)"
```

---

### Task 4: Dispatch the `*Failed` ops through the engine adapter

The router already synthesizes `InboundOp::GetAcceptedFrontierFailed`/`GetAcceptedFailed` on request timeout, but the bootstrapper adapter drops them at its `_ => {}` catch-all. Wire them to the new handlers.

**Files:**
- Modify: `crates/ava-engine/src/networking/engine_adapter.rs`
- Test: `crates/ava-engine/tests/engine_adapter.rs`

**Interfaces:**
- Consumes: `BootstrapperEngineAdapter::new`, `ChainEngine::{start, handle}`, `transition_channel`, `Getter::new`, `RecordingSender`, `Sent`, `InboundOp`.
- Produces: two new match arms in `BootstrapperEngineAdapter::handle`.

- [ ] **Step 1: Write the failing test**

Add to `crates/ava-engine/tests/engine_adapter.rs`. It builds a bootstrapper adapter directly, starts it, and delivers one real frontier reply + one `GetAcceptedFrontierFailed`; the phase must advance (a `GetAccepted` is sent) — proving the failed op is dispatched. (This mirrors the setup at `engine_adapter.rs:122-135`; reuse the file's existing imports for `BootstrapperEngineAdapter`, `transition_channel`, `Bootstrapper`, `BootConfig`, `RecordingSender`, `Sent`, `ChainEngine`, etc. Note `Config` is imported there under the alias `BootConfig`.)

```rust
/// The bootstrapper adapter routes a synthesized `GetAcceptedFrontierFailed`
/// (from a request timeout) into the bootstrapper, completing the frontier phase
/// on the beacons that did reply.
#[tokio::test]
async fn adapter_dispatches_get_accepted_frontier_failed() {
    let token = CancellationToken::new();
    let vm: TestVm = init_test_vm(&token).await.expect("vm");
    let tip = vm.last_accepted(&token).await.expect("genesis");

    let acceptor = Arc::new(CapturingAcceptor);
    let ctx = consensus_ctx(acceptor);
    let sender = RecordingSender::new();

    let a = NodeId::from([10u8; 20]);
    let b = NodeId::from([11u8; 20]);
    let mut beacons = BTreeMap::new();
    beacons.insert(a, 1u64);
    beacons.insert(b, 1u64);

    let vm_arc = Arc::new(AsyncMutex::new(vm));
    let boot = Bootstrapper::new(BootConfig {
        subnet_id: Id::EMPTY,
        ctx: ctx.clone(),
        vm: Arc::clone(&vm_arc),
        sender: Arc::clone(&sender),
        beacons,
        token: token.clone(),
    });
    let getter = Arc::new(ava_engine::snowman::Getter::new(
        vm_arc,
        Arc::clone(&sender),
        token.clone(),
    ));
    // Keep the transition receiver alive so `after()`'s transition send never errors.
    let (transition_tx, _transition_rx) = transition_channel(8);
    let mut adapter = BootstrapperEngineAdapter::new(boot, transition_tx, 0, getter);

    adapter.start().await; // sends GetAcceptedFrontier (request id 1)
    let _ = sender.drain();

    // One beacon replies; the other's query times out (synthesized *Failed).
    adapter
        .handle(a, InboundOp::AcceptedFrontier { request_id: 1, container_id: tip })
        .await;
    adapter
        .handle(b, InboundOp::GetAcceptedFrontierFailed { request_id: 1 })
        .await;

    let sent = sender.drain();
    assert!(
        sent.iter().any(|s| matches!(s, Sent::GetAccepted { .. })),
        "the failed op must be dispatched and complete the frontier phase, got {sent:?}"
    );
}
```

If `consensus_ctx`/`CapturingAcceptor`/`AsyncMutex` are not already in scope in this test file, reuse the file's existing context helper (it defines a `ConsensusContext` builder around `CapturingAcceptor` at `engine_adapter.rs:48-52`) and `tokio::sync::Mutex as AsyncMutex` import already present near the top.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-engine -E 'test(adapter_dispatches_get_accepted_frontier_failed)'`
Expected: FAIL — the `GetAcceptedFrontierFailed` op hits the `_ => {}` catch-all, the phase never advances, so no `GetAccepted` is sent (assertion fails).

- [ ] **Step 3: Add the dispatch arms**

In `crates/ava-engine/src/networking/engine_adapter.rs`, in `BootstrapperEngineAdapter::handle`, add these two arms immediately before the `_ => {}` catch-all (~line 188):

```rust
            InboundOp::GetAcceptedFrontierFailed { request_id } => {
                let res = self.boot.get_accepted_frontier_failed(node, request_id).await;
                self.after("bootstrap.get_accepted_frontier_failed", res).await;
            }
            InboundOp::GetAcceptedFailed { request_id } => {
                let res = self.boot.get_accepted_failed(node, request_id).await;
                self.after("bootstrap.get_accepted_failed", res).await;
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p ava-engine -E 'test(adapter_dispatches_get_accepted_frontier_failed)'`
Expected: PASS.

- [ ] **Step 5: Run the engine_adapter + bootstrap suites**

Run: `cargo nextest run -p ava-engine -E 'test(engine_adapter) or test(bootstrap)'`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ava-engine/src/networking/engine_adapter.rs crates/ava-engine/tests/engine_adapter.rs
git commit -m "fix(ava-engine): dispatch GetAcceptedFrontierFailed/GetAcceptedFailed to the bootstrapper"
```

---

### Task 5: Real monotonic clock on the test-boot path + un-gate the e2e

Fix the frozen `MockClock` that disables the request-timeout backstop in `boot_chain_over_network` (the timeout that synthesizes the `*Failed` ops now handled by Tasks 1–4), then remove the `#[ignore]` from the end-to-end regression test and delete the temporary diagnostic.

**Files:**
- Modify: `crates/avalanchers/src/wiring/chains.rs` (~line 1571)
- Modify: `crates/avalanchers/tests/beaconed_bootstrap.rs`

**Interfaces:**
- Consumes: `RealClock` (already imported at `chains.rs:47`), `AdaptiveTimeoutManager::new`, `follower_bootstraps_through_real_beacon_gate`.
- Produces: nothing new — this is the wiring fix + test un-gate.

**Why both halves are one task:** the clock fix alone does not make the e2e reliable (without Tasks 1–4 the synthesized `*Failed` is swallowed), and un-gating alone stays flaky (without the clock the timeout never fires). The reliable e2e is the single deliverable and its own test.

- [ ] **Step 1: Fix the clock in `boot_chain_over_network`**

In `crates/avalanchers/src/wiring/chains.rs`, in `boot_chain_over_network` (~line 1571), replace:

```rust
    // Self-contained router + virtual clock for the single-chain test nodes that
    // call this directly. The production multi-chain path uses the shared
    // `node.chain_router` via `boot_chain_over_network_core`.
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(SystemTime::UNIX_EPOCH));
```

with:

```rust
    // Self-contained router for the single-chain test nodes that call this
    // directly. The production multi-chain path uses the shared
    // `node.chain_router` via `boot_chain_over_network_core`.
    //
    // The `AdaptiveTimeoutManager` MUST be driven by an advancing monotonic
    // clock: `fire_expired` compares `deadline <= clock.monotonic()`, so a
    // frozen `MockClock` (whose `monotonic()` latches on first call) silently
    // disables every request timeout — the bootstrapper would then never see a
    // `*Failed` op for a lost/absent beacon reply and would hang. Use
    // `RealClock`, matching production (`ava-node` `chain_manager.rs`).
    let clock: Arc<dyn Clock> = Arc::new(RealClock);
```

- [ ] **Step 2: Confirm the crate still builds**

Run: `cargo build -p avalanchers`
Expected: builds clean. (If `MockClock`/`SystemTime` become unused in this file, the two remaining in-process paths at chains.rs:406 and :1082 still use them, so the imports stay live. If clippy later flags an unused import, remove only the now-unused symbol.)

- [ ] **Step 3: Remove the temporary diagnostic test**

In `crates/avalanchers/tests/beaconed_bootstrap.rs`, delete the entire `diag_beacon_wedge` test function and its doc comment (added during root-cause investigation; it is a 30s-sleeping probe, not a gate).

- [ ] **Step 4: Un-gate the e2e test**

In `crates/avalanchers/tests/beaconed_bootstrap.rs`, on `follower_bootstraps_through_real_beacon_gate`:
- Delete the `#[ignore = "..."]` attribute line.
- Replace the stale doc paragraph that begins ``/// `#[ignore]` (M9.15): this 6-node real-TLS bring-up is **bimodally flaky**`` … through the `--run-ignored all` sentence with:

```rust
/// This exercises the real `BeaconManager` connectivity gate end-to-end (the
/// coverage `networked_bootstrap.rs` lacks, since it hand-fires the gate). The
/// bootstrapper tolerates a beacon that has not yet handshaked when the gate
/// fires at `required_conns` (< all beacons): the missing frontier reply is
/// recovered via the request-timeout-synthesized `GetAcceptedFrontierFailed`
/// (see `ava-engine` bootstrap failure accounting), so the follower reaches
/// `NormalOp` deterministically.
```

- [ ] **Step 5: Verify the e2e is now deterministic (run it many times)**

Run:
```bash
for i in $(seq 1 30); do
  cargo nextest run -p avalanchers -E 'test(follower_bootstraps_through_real_beacon_gate)' \
    || { echo "WEDGE/FAIL on run $i"; break; }
done; echo "done"
```
Expected: 30 clean passes, no "WEDGE/FAIL". (Each warm run completes in ~1s; the 120s bound is only cold-runtime headroom.)

- [ ] **Step 6: Verify no regression in the sibling network e2e**

Run: `cargo nextest run -p avalanchers -E 'test(networked_bootstrap) or test(boot_over_network)'`
Expected: PASS (these also use `boot_chain_over_network`).

- [ ] **Step 7: Commit**

```bash
git add crates/avalanchers/src/wiring/chains.rs crates/avalanchers/tests/beaconed_bootstrap.rs
git commit -m "fix(M9.15): RealClock for boot_chain_over_network timeouts; un-gate beaconed_bootstrap"
```

---

### Task 6: Full-workspace gates + docs

Run the project's CI gates and record the fix in the plan/PORTING as the repo convention requires.

**Files:**
- Modify: `plan/M9-interop-hardening.md` (append an AS-BUILT note under the M9.15 section)

- [ ] **Step 1: Lint**

Run: `./scripts/run_task.sh lint-all`
Expected: clean (clippy `-D warnings`, rustfmt, license headers).

- [ ] **Step 2: Unit tests for the touched crates**

Run: `cargo nextest run -p ava-engine -p avalanchers -p ava-node`
Expected: all pass.

- [ ] **Step 3: Record the AS-BUILT note**

In `plan/M9-interop-hardening.md`, under the M9.15 section, append a short AS-BUILT callout: root cause (bootstrapper required all-beacon replies + no `*Failed` accounting; frozen `MockClock` disabled the timeout backstop on the test-boot path), the Go-parity fix (empty-opinion failure accounting + `*Failed` dispatch + `RealClock`), the un-gated `follower_bootstraps_through_real_beacon_gate`, and the remaining follow-up (the `ava-network` concurrent self-dial TOCTOU, still latent).

- [ ] **Step 4: Commit**

```bash
git add plan/M9-interop-hardening.md
git commit -m "docs(M9.15): AS-BUILT — bootstrap failure accounting + beaconed_bootstrap un-gated"
```

---

## Follow-ups (out of scope for this plan)

- **`ava-network` concurrent self-dial TOCTOU:** `run_dialer` can dispatch a second `handle_dial` to a node before the first completes its TLS upgrade and enters `connecting` (unlike Go's one-goroutine-per-tracked-IP model). Investigated and ruled out as the cause of this wedge, but a real latent bug. Fix by adding an in-flight-dial guard set or porting Go's per-IP dialer loop.
- **Nightly live `mixed_network` two-binary arm** remains gated by design.
