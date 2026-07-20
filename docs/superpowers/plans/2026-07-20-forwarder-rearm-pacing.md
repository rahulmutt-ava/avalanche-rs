# Forwarder Re-arm Pacing (ACP-226 Residual) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route every NotificationForwarder signal — first and re-arm alike — through the ACP-226-paced `PendingWorkWaiter::wait()`, so consecutive C-Chain builds under sustained load respect the minimum block delay (full coreth NotificationForwarder parity).

**Architecture:** Collapse the forwarder's two loops (paced outer park + unpaced 2 s re-arm) in `crates/ava-chains/src/create_chain.rs` step 7b into ONE loop — paced `wait()` → send `PendingTxs` → 2 s retry-floor sleep — and extract it from the inline spawned closure into a private free function `forward_pending_work(waiter, vm_tx, token)` so the behavior gets direct unit tests with a gated mock waiter. The floor sleep is load-bearing (anti-busy-spin after a windower "not my slot" send). `has_pending()` loses its only forwarder caller but stays on the trait (SAE txpool + ava-evm tests use it).

**Tech Stack:** Rust workspace (`crates/ava-chains`), tokio (`test-util` already in dev-deps: paused time + auto-advance), `async_trait`, `tokio_util::sync::CancellationToken`, cargo-nextest. Reference Go: `~/avalanchego/snow/engine/common/notifier.go` (re-enters `WaitForEvent` before every signal).

**Spec:** `docs/superpowers/specs/2026-07-20-forwarder-rearm-pacing-design.md` (approved).

## Global Constraints

- License header on every touched `.rs` file (already present in the one file this plan modifies).
- No `unwrap()`/`expect()` in library code; `#[cfg(test)]` code may use `expect`.
- All cargo invocations through the Nix dev shell: `./scripts/nix_run.sh cargo ...`.
- Assertion messages on every `assert!`/`assert_eq!` naming the behavior under test.
- Never hold a lock across an `.await` in the forwarder (it holds none today; keep it that way).
- `PendingWorkWaiter::has_pending` MUST remain on the trait unchanged — SAE's txpool (`crates/ava-saevm/cchain/src/txpool.rs`) and the ava-evm waiter tests still call it.
- Scope guards: do NOT port coreth's 100 ms `RetryDelay` arm (the 2 s floor keeps the established cadence); do NOT touch `EvmPendingWorkWaiter`, `build_block`, the `PendingWorkWaiter` trait, or the plugin-path `EvmVm::wait_for_event`.
- Send backpressure semantics unchanged: no cancellation select around `vm_tx.send(..)`.

---

### Task 1: Extract `forward_pending_work` and pace the re-arm

**Files:**
- Modify: `crates/ava-chains/src/create_chain.rs` — the step-7b comment + spawned closure (~lines 768-807), a new private free function + const above `pub fn create_snowman_chain`'s helpers (place directly after the `SnowmanChain` impl block the closure currently sits inside `create_snowman_chain`; exact anchor below), and a new `#[cfg(test)] mod tests` at the end of the file.

**Interfaces:**
- Consumes: `ava_vm::vm::{PendingWorkWaiter, VmEvent}` (already imported at line 61), `tokio::sync::mpsc`, `tokio_util::sync::CancellationToken`, `async_trait::async_trait` (all already in scope in this file).
- Produces: `async fn forward_pending_work(waiter: Arc<dyn PendingWorkWaiter>, vm_tx: mpsc::Sender<VmEvent>, token: CancellationToken)` and `const FORWARDER_RETRY_FLOOR: Duration` — both private to `create_chain.rs`; Task 2 has no code dependency on them.

- [ ] **Step 1: Write the failing tests**

At the very end of `crates/ava-chains/src/create_chain.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use ava_vm::vm::{PendingWorkWaiter, VmEvent};
    use tokio::sync::{Semaphore, mpsc};
    use tokio_util::sync::CancellationToken;

    use super::forward_pending_work;

    /// Narrow local mock: `wait()` resolves once per permit the test releases.
    /// Stands in for the ACP-226-paced EVM waiter — "gated" == pacing (or an
    /// empty pool) is holding the forwarder back.
    struct GatedWaiter(Semaphore);

    impl GatedWaiter {
        fn new() -> Arc<Self> {
            Arc::new(Self(Semaphore::new(0)))
        }

        fn release(&self, n: usize) {
            self.0.add_permits(n);
        }
    }

    #[async_trait]
    impl PendingWorkWaiter for GatedWaiter {
        fn has_pending(&self) -> bool {
            // Unused by the forwarder (pacing subsumed the re-arm guard);
            // required by the trait.
            self.0.available_permits() > 0
        }

        async fn wait(&self) {
            self.0
                .acquire()
                .await
                .expect("test semaphore is never closed")
                .forget();
        }
    }

    // All tests run under paused tokio time: timers auto-advance whenever the
    // runtime is idle, so the 60 s `timeout(..)` guards and the 2 s retry
    // floor elapse instantly and deterministically. (Safe here, unlike the
    // ava-evm pending_work_waiter integration tests, because the gate is a
    // semaphore the test controls — there is no MockClock for auto-advance to
    // race against.)

    #[tokio::test(start_paused = true)]
    async fn no_send_before_wait_releases_then_one_per_release() {
        let waiter = GatedWaiter::new();
        let (vm_tx, mut rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        // The sustained-load regression: while wait() is gated (pacing not yet
        // elapsed), NO signal may reach the engine — the old inner re-arm loop
        // sent unpaced here.
        assert!(
            tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .is_err(),
            "no PendingTxs before wait() releases (paced re-arm)"
        );

        waiter.release(1);
        let evt = tokio::time::timeout(Duration::from_secs(60), rx.recv())
            .await
            .expect("released wait() must produce a signal")
            .expect("engine channel stays open");
        assert!(
            matches!(evt, VmEvent::PendingTxs),
            "forwarder signals PendingTxs"
        );

        // One release => exactly one send: the floor elapses (auto-advance)
        // and wait() re-parks on the drained semaphore.
        assert!(
            tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .is_err(),
            "exactly one PendingTxs per wait() release"
        );

        token.cancel();
        task.await.expect("forwarder exits cleanly on cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn consecutive_sends_spaced_by_retry_floor() {
        let waiter = GatedWaiter::new();
        let (vm_tx, mut rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();
        // Two immediate releases: wait() resolves instantly twice, so ONLY the
        // floor separates the sends — without it they land in the same instant
        // (the busy-spin the floor exists to prevent).
        waiter.release(2);
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        rx.recv().await.expect("first signal");
        let first = tokio::time::Instant::now();
        rx.recv().await.expect("second signal");
        let elapsed = tokio::time::Instant::now().duration_since(first);
        assert!(
            elapsed >= Duration::from_secs(2),
            "consecutive sends must be spaced by the 2s retry floor, got {elapsed:?}"
        );

        token.cancel();
        task.await.expect("forwarder exits cleanly on cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_terminates_while_parked_in_wait() {
        let waiter = GatedWaiter::new();
        let (vm_tx, _rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        token.cancel();
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("cancel while parked in wait() must terminate the forwarder")
            .expect("forwarder must not panic");
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_terminates_during_retry_floor() {
        let waiter = GatedWaiter::new();
        let (vm_tx, mut rx) = mpsc::channel::<VmEvent>(8);
        let token = CancellationToken::new();
        waiter.release(1);
        let task = tokio::spawn(forward_pending_work(
            Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
            vm_tx,
            token.clone(),
        ));

        rx.recv().await.expect("first signal");
        // The forwarder is now at (or entering) the floor sleep. Time is
        // paused and never advanced, so the cancelled branch is the only arm
        // of its select that can become ready.
        token.cancel();
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("cancel during the retry floor must terminate the forwarder")
            .expect("forwarder must not panic");
    }

    #[tokio::test(start_paused = true)]
    async fn closed_channel_terminates_forwarder() {
        let waiter = GatedWaiter::new();
        let (vm_tx, rx) = mpsc::channel::<VmEvent>(8);
        drop(rx);
        waiter.release(1);

        tokio::time::timeout(
            Duration::from_secs(60),
            forward_pending_work(
                Arc::clone(&waiter) as Arc<dyn PendingWorkWaiter>,
                vm_tx,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("a closed engine channel must terminate the forwarder");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-chains -E 'test(forward) or test(retry_floor) or test(closed_channel) or test(cancel_terminates)'`
Expected: FAIL to compile — `cannot find function forward_pending_work in module super`.

- [ ] **Step 3: Implement — the paced loop as a free function; shrink the spawn site**

**(a)** In `crates/ava-chains/src/create_chain.rs`, find the step-7b block (currently ~lines 768-807):

```rust
    // 7b. Production NotificationForwarder (Go snow/engine/common/notifier.go:31-134,
    //     started at handler start per handler.go:254-255): a VM pending-work
    //     signal becomes an engine `PendingTxs` build trigger. Spawned ONLY for a
    //     VM that hands out a lock-free waiter (`Some` — the EVM VM today; P/X/SAE
    //     return `None`, so their chains spawn no task and see no extra sends). The
    //     task holds NO VM lock: it awaits the Task-1 waiter (which locks only the
    //     mempool `Arc`s) and sends into `vm_tx`, never touching the shared
    //     `Arc<Mutex<dyn Vm>>` verify/get/build hold (the M7.18 lock-parking hazard
    //     this seam exists to avoid).
    if let Some(waiter) = pending_waiter {
        let vm_tx = vm_tx.clone();
        let token = token.clone();
        tokio::spawn(async move {
            loop {
                // Park until the VM gains buildable work, or the chain is torn down.
                tokio::select! {
                    () = waiter.wait() => {}
                    () = token.cancelled() => return,
                }
                // Signal once; spurious signals are harmless (the engine build
                // returns NotFound when there is nothing to build — engine.rs).
                if vm_tx.send(VmEvent::PendingTxs).await.is_err() {
                    return;
                }
                // Re-arm while work remains so a build rejected by the proposervm
                // windower ("not my slot yet") is retried when the next window
                // opens (Go's CheckForEvent re-arm). Bounded, cancellation-aware
                // sleep — never a busy-spin.
                while waiter.has_pending() {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(2)) => {}
                        () = token.cancelled() => return,
                    }
                    if vm_tx.send(VmEvent::PendingTxs).await.is_err() {
                        return;
                    }
                }
            }
        });
    }
```

Replace it with:

```rust
    // 7b. Production NotificationForwarder (Go snow/engine/common/notifier.go:31-134,
    //     started at handler start per handler.go:254-255): a VM pending-work
    //     signal becomes an engine `PendingTxs` build trigger. Spawned ONLY for a
    //     VM that hands out a lock-free waiter (`Some` — the EVM VM today; P/X/SAE
    //     return `None`, so their chains spawn no task and see no extra sends).
    //     See `forward_pending_work` for the loop's invariants.
    if let Some(waiter) = pending_waiter {
        tokio::spawn(forward_pending_work(waiter, vm_tx.clone(), token.clone()));
    }
```

**(b)** Directly AFTER the closing brace of the function containing that spawn site (`create_snowman_chain`, the `Ok(SnowmanChain { ... })` return at ~line 809-821) — i.e. between that function and the end of the file — add:

```rust
/// Retry floor between consecutive forwarder signals. After a send that
/// produces no block (proposervm windower "not my slot yet"), buildable work
/// still exists and the ACP-226 pacing inside [`PendingWorkWaiter::wait`] has
/// already elapsed, so `wait()` returns instantly — without this floor the
/// loop would busy-spin sends. Coreth's analogue is the 100 ms
/// `RetryDelay`/`lastBuildTime` arm inside `waitForEvent`; we keep the
/// established 2 s cadence (design:
/// `docs/superpowers/specs/2026-07-20-forwarder-rearm-pacing-design.md`).
const FORWARDER_RETRY_FLOOR: Duration = Duration::from_secs(2);

/// The production NotificationForwarder loop: parks on the ACP-226-paced
/// [`PendingWorkWaiter::wait`] and turns each release into ONE engine
/// `PendingTxs` signal, so EVERY send — first and re-arm alike — respects the
/// parent's minimum block delay (coreth parity: its NotificationForwarder
/// re-enters `WaitForEvent` before every signal). `wait()` parking on an
/// empty pool doubles as the idle park, so no separate `has_pending()` guard
/// is needed. Holds NO VM lock (M7.18 lock-parking hazard): the waiter locks
/// only mempool `Arc`s. Exits on chain teardown (`token`) or a closed engine
/// channel; a full channel parks the send un-cancellably, as before.
async fn forward_pending_work(
    waiter: Arc<dyn PendingWorkWaiter>,
    vm_tx: mpsc::Sender<VmEvent>,
    token: CancellationToken,
) {
    loop {
        // Park until the VM has buildable work AND the parent's ACP-226
        // minimum delay has cleared, or the chain is torn down.
        tokio::select! {
            () = waiter.wait() => {}
            () = token.cancelled() => return,
        }
        // Signal once; spurious signals are harmless (the engine build
        // returns NotFound when there is nothing to build — engine.rs).
        if vm_tx.send(VmEvent::PendingTxs).await.is_err() {
            return;
        }
        // Anti-busy-spin retry floor — see [`FORWARDER_RETRY_FLOOR`].
        tokio::select! {
            () = tokio::time::sleep(FORWARDER_RETRY_FLOOR) => {}
            () = token.cancelled() => return,
        }
    }
}
```

`Arc`, `Duration`, `mpsc`, `CancellationToken`, `PendingWorkWaiter`, `VmEvent`, and `tokio::select!` are all already imported/in scope in this file — add NO new imports outside the test module.

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-chains -E 'test(forward) or test(retry_floor) or test(closed_channel) or test(cancel_terminates)'`
Expected: 5 PASS.

- [ ] **Step 5: Run the crate suites + hygiene**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-chains -p ava-evm`
Expected: all PASS (the ava-evm waiter integration tests exercise the same waiter this forwarder consumes).

Run: `./scripts/nix_run.sh cargo clippy -p ava-chains --all-targets -- -D warnings`
Expected: clean. (If clippy flags the `as Arc<dyn PendingWorkWaiter>` coercions in tests as unnecessary, drop the casts — `tokio::spawn(forward_pending_work(Arc::clone(&waiter), ...))` may coerce implicitly; keep whichever form compiles clean.)

Run: `./scripts/nix_run.sh cargo fmt --check`
Expected: clean (run `./scripts/nix_run.sh cargo fmt` and include the result if not).

- [ ] **Step 6: Commit**

```bash
git add crates/ava-chains/src/create_chain.rs
git commit -m "feat(ava-chains): pace the forwarder re-arm through PendingWorkWaiter::wait() — every PendingTxs signal now respects the ACP-226 min delay (coreth notifier.go parity); extract forward_pending_work + unit tests"
```

---

### Task 2: Closeout — docs, workspace gates, live gates

**Files:**
- Modify: `plan/M9-interop-hardening.md` (the min-delay AS-BUILT callout's `**Residual (follow-up):**` sentence, ~line 2221)
- Modify: `docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md` (the AS-BUILT note's re-arm clause)
- Modify: `docs/superpowers/specs/2026-07-20-forwarder-rearm-pacing-design.md` (`**Status:** approved` → `**Status:** implemented`)
- No code.

**Interfaces:**
- Consumes: Task 1 merged into the working tree.
- Produces: green whole-workspace + live gates; docs recording closure.

- [ ] **Step 1: Docs**

1. In `plan/M9-interop-hardening.md`, rewrite the min-delay callout's residual sentence (currently "**Residual (follow-up):** pacing covers the first signal after idle ... until the re-arm is routed through the paced `wait()`.") to record closure, keeping the `>` style:

```
> **Residual — CLOSED 2026-07-20:** the forwarder re-arm is now routed through the paced
> `wait()` (`forward_pending_work`, one paced loop + 2 s anti-busy-spin retry floor; coreth
> notifier.go parity), so every `PendingTxs` signal — first and re-arm alike — respects the
> ACP-226 min delay. Design: `docs/superpowers/specs/2026-07-20-forwarder-rearm-pacing-design.md`.
> Still open (latent, documented): plugin-path `EvmVm::wait_for_event` unpaced.
```

2. In `docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md`, update the AS-BUILT note's final clause — replace "The `ava-chains` forwarder's 2 s re-arm is likewise unpaced under sustained load (see the M9 AS-BUILT residual)." with "The `ava-chains` forwarder's 2 s re-arm was subsequently paced through `wait()` on 2026-07-20 (`forward_pending_work`; see `2026-07-20-forwarder-rearm-pacing-design.md`)." Leave the `wait_for_event` asymmetry sentence untouched.

3. In `docs/superpowers/specs/2026-07-20-forwarder-rearm-pacing-design.md`, flip `**Status:** approved` → `**Status:** implemented`.

- [ ] **Step 2: Full workspace gates**

```bash
./scripts/run_task.sh lint-all
./scripts/run_task.sh test-unit
```

Expected: both green. If lint-all fails at `bazel-check-metadata`/`check-clean-branch` with a modified `crates/ava-chains/BUILD.bazel`, that is the gazelle regen for the new in-module tests — inspect that the diff is only generated dep/test additions, commit it as `build(bazel): gazelle regen — ava-chains forwarder tests`, and re-run lint-all.

- [ ] **Step 3: Live gates**

Verify the oracle, prewarm a freshly-relinked release binary, then rerun both live arms:

```bash
./scripts/check_oracle_binary.sh
touch crates/ava-chains/src/lib.rs
./scripts/nix_run.sh cargo build -p avalanchers --release && ./target/release/avalanchers --version
AVALANCHEGO_PATH=$HOME/avalanchego/build/avalanchego ./scripts/nix_run.sh cargo test -p ava-differential --features live --test mixed_network -- --ignored --exact --nocapture mixed_network
AVALANCHEGO_PATH=$HOME/avalanchego/build/avalanchego ./scripts/nix_run.sh cargo test -p ava-differential --features live --test mixed_network -- --ignored --exact --nocapture mixed_network_rust_proposes
```

(Live arms via `cargo test`, NOT nextest — the 120 s slow-timeout kills them. `check_oracle_binary.sh` must print `OK` first.)

Expected: both PASS. A FAIL in `mixed_network_rust_proposes` means the pacing now over-waits (e.g. the floor + pacing interaction starves the engine's patience) — debug against `notifier.go`/`block_builder.go`, do not weaken the wait.

- [ ] **Step 4: Final commit**

```bash
git add plan/M9-interop-hardening.md docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md docs/superpowers/specs/2026-07-20-forwarder-rearm-pacing-design.md
git commit -m "docs: forwarder re-arm pacing AS-BUILT — sustained-load MinDelayNotMet residual closed"
```
