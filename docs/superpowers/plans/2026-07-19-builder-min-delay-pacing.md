# Builder Min-Delay Pacing (ACP-226) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pace the C-Chain build trigger so a Rust-built block never fires before the ACP-226 minimum delay after its parent — closing the `MinDelayNotMet` liveness papercut flagged in the semantic-verify final review.

**Architecture:** Port coreth's `waitForEvent` min-delay wait (`plugin/evm/block_builder.go:140-214`) into our port of that exact seam: `EvmPendingWorkWaiter::wait()` (`crates/ava-evm/src/vm.rs`). After buildable work appears, the waiter sleeps until `parent_time_ms + parent.MinDelayExcess.delay()`, rounded up to the next whole second (our builder stamps whole-second timestamps), computed from the injected `Shared.clock`. Fail-open everywhere: any abnormal path resolves immediately and `verify_time` remains the safety backstop. The `PendingWorkWaiter` trait, the generic forwarder in `ava-chains`, and `build_block` are untouched.

**Tech Stack:** Rust workspace (`crates/ava-evm`), tokio, `arc_swap`, `ava_utils::clock::{Clock, MockClock}`, cargo-nextest. Reference Go: `~/avalanchego/graft/coreth/plugin/evm/block_builder.go` and `customheader/time.go`.

**Spec:** `docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md` (approved).

## Global Constraints

- License header on every touched `.rs` file (already present in all files this plan modifies).
- No `unwrap()`/`expect()` in library code (`crates/ava-evm/src/**`); tests may use `expect`.
- Arithmetic in library code: use `saturating_*`/`div_ceil` — never bare `+`/`-` on header-time quantities.
- All cargo invocations through the Nix dev shell: `./scripts/nix_run.sh cargo ...`.
- Assertion messages on every `assert!`/`assert_eq!` naming the behavior under test (repo testing convention).
- Never hold a lock across an `.await` in the waiter (the M7.18 lock-parking hazard this seam exists to avoid).
- Scope guard: coreth's `RetryDelay`(100 ms)/`lastBuildTime` retry arm is deliberately NOT ported (the forwarder's 2 s re-arm covers it) — do not add it.

---

### Task 1: `feerules::min_next_block_time_ms` helper

**Files:**
- Modify: `crates/ava-evm/src/feerules/mod.rs` (helper next to `header_time_ms` at ~line 418; tests in the existing `mod tests` that defines `hdr(..)` at ~line 1040)

**Interfaces:**
- Consumes: existing `header_time_ms(h: &AvaHeader) -> u64` (`feerules/mod.rs:418`), `DelayExcess(pub u64)` with `delay(self) -> u64` (milliseconds), `INITIAL_DELAY_EXCESS: DelayExcess = DelayExcess(7_970_124)` (delay = 2000 ms) — all already imported in `feerules/mod.rs` (line 25).
- Produces: `pub(crate) fn min_next_block_time_ms(parent: &AvaHeader) -> Option<u64>` — Task 2's waiter calls this as `crate::feerules::min_next_block_time_ms(...)`.

- [ ] **Step 1: Write the failing tests**

In `crates/ava-evm/src/feerules/mod.rs`, inside the existing tests module that defines the `hdr(number, time, time_ms, extra)` header helper (~line 1040), add:

```rust
// ── min_next_block_time_ms: coreth minNextBlockTime (block_builder.go:202) ─

#[test]
fn min_next_block_time_ms_pre_granite_parent_is_none() {
    // No MinDelayExcess on the parent => the ACP-226 rule does not apply to
    // the child; there is nothing to wait for (coreth's nil-arm returns the
    // zero time).
    let parent = hdr(0, 1_607_144_400, None, vec![]);
    assert_eq!(
        min_next_block_time_ms(&parent),
        None,
        "pre-Granite parent (no MinDelayExcess) => no minimum next block time"
    );
}

#[test]
fn min_next_block_time_ms_adds_parent_delay() {
    // Local genesis shape: whole-second ms timestamp, initial delay excess
    // (delay() == 2000 ms).
    let mut parent = hdr(0, 1_607_144_400, Some(1_607_144_400_000), vec![]);
    parent.min_delay_excess = Some(INITIAL_DELAY_EXCESS.0);
    assert_eq!(
        min_next_block_time_ms(&parent),
        Some(1_607_144_402_000),
        "min next block time = parent_ms + 2000ms initial delay"
    );
}

#[test]
fn min_next_block_time_ms_mid_second_parent_and_seconds_fallback() {
    // A Go-built parent can carry a mid-second ms timestamp — the sum is
    // mid-second too (the CALLER rounds up to whole seconds).
    let mut parent = hdr(0, 1_607_144_400, Some(1_607_144_400_277), vec![]);
    parent.min_delay_excess = Some(INITIAL_DELAY_EXCESS.0);
    assert_eq!(
        min_next_block_time_ms(&parent),
        Some(1_607_144_402_277),
        "mid-second parent ms is preserved in the target"
    );

    // No TimeMilliseconds => header_time_ms falls back to Time * 1000.
    let mut secs_only = hdr(0, 1_607_144_400, None, vec![]);
    secs_only.min_delay_excess = Some(INITIAL_DELAY_EXCESS.0);
    assert_eq!(
        min_next_block_time_ms(&secs_only),
        Some(1_607_144_402_000),
        "seconds-only parent uses the Time*1000 fallback"
    );
}
```

If the module's `use super::{...}` list does not already include `min_next_block_time_ms` and `INITIAL_DELAY_EXCESS`, add them to it.

- [ ] **Step 2: Run tests to verify they fail**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm -E 'test(min_next_block_time_ms)'`
Expected: FAIL to compile — `cannot find function min_next_block_time_ms`.

- [ ] **Step 3: Write the implementation**

In `crates/ava-evm/src/feerules/mod.rs`, directly below `header_time_ms` (~line 424):

```rust
/// coreth `plugin/evm/block_builder.go:202` — `minNextBlockTime`.
///
/// The earliest millisecond timestamp a child of `parent` may carry under the
/// parent's ACP-226 `MinDelayExcess`: `header_time_ms(parent) + delay`. `None`
/// when the parent pre-dates Granite (`min_delay_excess` absent) — the rule
/// does not apply to the block being built, so there is nothing to wait for
/// (coreth's nil-arm). Callers that stamp whole-second timestamps must round
/// the result UP to the next whole second.
#[must_use]
pub(crate) fn min_next_block_time_ms(parent: &AvaHeader) -> Option<u64> {
    let excess = parent.min_delay_excess?;
    Some(header_time_ms(parent).saturating_add(DelayExcess(excess).delay()))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm -E 'test(min_next_block_time_ms)'`
Expected: 3 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ava-evm/src/feerules/mod.rs
git commit -m "feat(ava-evm): feerules::min_next_block_time_ms — coreth minNextBlockTime port (block_builder.go:202)"
```

---

### Task 2: Pace `EvmPendingWorkWaiter::wait()` on the ACP-226 min delay

**Files:**
- Modify: `crates/ava-evm/src/vm.rs` — `EvmVm.preferred` field (~line 363) + its construction (~line 443), `EvmPendingWorkWaiter` struct + `wait()` (~lines 727-766), `pending_work_waiter()` (~line 907)
- Test: `crates/ava-evm/tests/pending_work_waiter.rs` (extend the existing file)

**Interfaces:**
- Consumes: Task 1's `crate::feerules::min_next_block_time_ms(parent: &AvaHeader) -> Option<u64>`; existing `Shared` (private struct in `vm.rs`: `verified: DashMap<Id, ProcessingBlock>`, `clock: parking_lot::Mutex<Arc<dyn Clock>>`); `Clock::unix() -> u64` (seconds); `EvmVm::with_clock(self, Arc<dyn Clock>) -> Self`; `MockClock::{at, set}` (`Clone` shares state via inner `Arc`).
- Produces: no new public API. Behavior change only: `PendingWorkWaiter::wait()` on the EVM waiter now resolves no earlier than the whole second clearing `parent_time_ms + min_delay`.

- [ ] **Step 1: Write the failing test (pacing gate + end-to-end self-verify)**

Append to `crates/ava-evm/tests/pending_work_waiter.rs`. Add to the existing imports at the top of the file:

```rust
use std::time::UNIX_EPOCH;

use ava_utils::clock::MockClock;
use ava_vm::block::ChainVm;
use tokio_util::sync::CancellationToken;
```

(If the compiler reports `verify` is a trait method not in scope, additionally `use ava_vm::block::Block;`.)

Add near the other consts:

```rust
/// The committed local C-Chain genesis timestamp (`local.json` `"timestamp":
/// "0x5FCB13D0"`). The genesis header is Granite-active on `local`, so it
/// carries `min_delay_excess = INITIAL_DELAY_EXCESS` (delay = 2000 ms) — the
/// earliest legal child stamp is GENESIS_TIME_SECS + 2.
const GENESIS_TIME_SECS: u64 = 1_607_144_400;
```

Add the two tests. (Deliberate deviation from the design doc's "paused tokio time" note: `start_paused` auto-advances sleeps whenever the runtime is idle, which would both defeat the "still pending at 300 ms" assertion and hot-loop the pacing tail against a never-advancing `MockClock`. Real short sleeps + a `MockClock` pin are used instead — the test costs ~1 s of real time, in line with this file's existing 5 s timeouts.)

```rust
#[tokio::test]
async fn wait_resolves_immediately_when_min_delay_elapsed() {
    // Clock already at genesis + 2s (== the ACP-226 target for the initial
    // delay excess): pacing must add ZERO wait — guards against over-waiting.
    let (vm, _dir) = build_vm();
    let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(GENESIS_TIME_SECS + 2));
    let vm = vm.with_clock(Arc::new(clock));

    let (tx, _hash) = signed_transfer(0);
    let sender = SenderAccount {
        nonce: 0,
        balance: ewoq_balance(),
    };
    let rules = AdmissionRules {
        chain_id: CHAIN_ID,
        ..Default::default()
    };
    vm.evm_mempool_handle()
        .lock()
        .add_local(tx, &sender, &rules)
        .expect("admit ewoq transfer");

    let waiter = vm
        .pending_work_waiter()
        .expect("EvmVm exposes a PendingWorkWaiter");
    tokio::time::timeout(Duration::from_secs(1), waiter.wait())
        .await
        .expect("min delay already elapsed => wait() resolves without pacing");
}

#[tokio::test]
async fn wait_gates_on_min_delay_then_built_block_passes_self_verify() {
    // The papercut regression test. Clock pinned at genesis + 1s: one second
    // of the 2000ms ACP-226 minimum delay remains, so wait() must NOT resolve
    // yet (pre-pacing it resolved the instant work existed, and the block
    // built at genesis+1 died at its own verify with MinDelayNotMet).
    let (vm, _dir) = build_vm();
    let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(GENESIS_TIME_SECS + 1));
    let mut vm = vm.with_clock(Arc::new(clock.clone()));

    let (tx, _hash) = signed_transfer(0);
    let sender = SenderAccount {
        nonce: 0,
        balance: ewoq_balance(),
    };
    let rules = AdmissionRules {
        chain_id: CHAIN_ID,
        ..Default::default()
    };
    vm.evm_mempool_handle()
        .lock()
        .add_local(tx, &sender, &rules)
        .expect("admit ewoq transfer");

    let waiter = vm
        .pending_work_waiter()
        .expect("EvmVm exposes a PendingWorkWaiter");
    let w2 = Arc::clone(&waiter);
    let mut parked = tokio::spawn(async move { w2.wait().await });

    // 300ms into a ~1s pacing window: still gated. (Self-healing on slow
    // machines: if the waiter's sleep elapses before the clock is advanced
    // below, it recomputes a positive remainder and sleeps again.)
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !parked.is_finished(),
        "wait() must gate until GENESIS_TIME_SECS + 2 (ACP-226 min delay)"
    );

    // The chain clock reaching the target lets the pacing loop resolve once
    // its pending sleep elapses.
    clock.set(UNIX_EPOCH + Duration::from_secs(GENESIS_TIME_SECS + 2));
    tokio::time::timeout(Duration::from_secs(5), &mut parked)
        .await
        .expect("wait() resolves once the min delay has elapsed")
        .expect("parked wait() task must not panic");

    // The paced build stamps clock.unix() == genesis+2 and passes its OWN
    // verify — no MinDelayNotMet (actual 2000ms == required 2000ms).
    let token = CancellationToken::new();
    let built = vm
        .build_block(&token)
        .await
        .expect("build_block after the paced wait");
    built
        .verify(&token)
        .await
        .expect("self-verify after paced build: MinDelayNotMet papercut is closed");
}
```

- [ ] **Step 2: Run the new tests to verify the gating test fails**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm -E 'test(wait_gates_on_min_delay) or test(wait_resolves_immediately_when_min_delay)'`
Expected: `wait_gates_on_min_delay_then_built_block_passes_self_verify` FAILS at the `!parked.is_finished()` assertion (today's waiter resolves the instant work exists). `wait_resolves_immediately_when_min_delay_elapsed` PASSES both before and after (it guards against over-waiting, not the gap).

- [ ] **Step 3: Implement — wrap `preferred` in `Arc`, extend the waiter, add the pacing tail**

All in `crates/ava-evm/src/vm.rs`.

**(a)** Change the `EvmVm.preferred` field type (~line 363) so the waiter can hold it:

```rust
    /// The currently preferred (leaf) block id (Go `vm.preferred`). Record-only:
    /// Snowman owns fork choice, so `set_preference` does no reorg work (G6).
    /// Behind an `Arc` so the [`EvmPendingWorkWaiter`] can watch the next
    /// build's parent without holding the VM.
    preferred: Arc<ArcSwap<Id>>,
```

and its construction (~line 443):

```rust
            preferred: Arc::new(ArcSwap::from_pointee(tip.0)),
```

The three existing call sites (`*self.preferred.load_full()` at ~581 and ~930, `self.preferred.store(...)` at ~1038) compile unchanged via auto-deref.

**(b)** Extend `EvmPendingWorkWaiter` (~line 727) — replace the struct definition with:

```rust
/// A lock-free [`PendingWorkWaiter`] over `EvmVm`'s two mempools (the atomic
/// X<->C pool and the EVM pool), paced by the parent's ACP-226 minimum block
/// delay (coreth `waitForEvent`, `plugin/evm/block_builder.go:140-214`). Holds
/// only `Arc`s the VM already owns — NEVER the outer `Arc<Mutex<dyn Vm>>` a
/// proposal forwarder would otherwise have to hold to call `wait_for_event`
/// (the M7.18 lock-parking hazard this seam exists to avoid): `verified` is a
/// `DashMap`, the clock read clones an `Arc`, and no lock is held across an
/// `.await`.
struct EvmPendingWorkWaiter {
    atomic: Arc<parking_lot::Mutex<AtomicMempool>>,
    evm: Arc<parking_lot::Mutex<EvmMempool>>,
    /// The shared core — the preferred parent's header (`verified`) and the
    /// injected clock, the two ACP-226 pacing inputs.
    shared: Arc<Shared>,
    /// The preferred (leaf) block id the next build will extend.
    preferred: Arc<ArcSwap<Id>>,
}
```

**(c)** Replace the `wait()` body (~lines 745-765) with the paced loop (the subscribe-before-check race guard is preserved verbatim; the pacing tail is new):

```rust
    async fn wait(&self) {
        // Mirrors `EvmVm::wait_for_event` below: register on BOTH pools'
        // notify BEFORE the emptiness check so a tx admitted between the
        // check and the `select!` is never lost (tokio `Notify` stores one
        // permit — the `.notified()` future created here observes a
        // `notify_one` that fires after this line).
        loop {
            let atomic_notify = self.atomic.lock().subscribe();
            let evm_notify = self.evm.lock().subscribe();
            if !self.pending() {
                tokio::select! {
                    () = atomic_notify.notified() => {}
                    () = evm_notify.notified() => {}
                }
                // Loop back: re-subscribe and re-check. A spurious wake (e.g.
                // the admission that woke us was immediately drained by a
                // concurrent `build_block`) simply re-arms.
                continue;
            }

            // Work exists. coreth waitForEvent (block_builder.go:140-163): do
            // not signal the engine before the ACP-226 minimum delay after
            // the parent has elapsed — a block built earlier dies at its own
            // VerifyTime with MinDelayNotMet. Fail-open: an unresolvable
            // preferred id or a pre-Granite parent means nothing to wait for
            // (verify remains the safety backstop; coreth's nil-arm).
            let preferred = *self.preferred.load_full();
            let Some(min_next_ms) = self
                .shared
                .verified
                .get(&preferred)
                .and_then(|pb| crate::feerules::min_next_block_time_ms(pb.block.header()))
            else {
                return;
            };
            // `build_block` stamps whole seconds (`timestamp_ms = secs *
            // 1000`), so round UP to the next whole second that clears the
            // delay — a Go-built parent can carry a mid-second ms timestamp,
            // and flooring would still fail MinDelayNotMet.
            let target_secs = min_next_ms.div_ceil(1000);
            let now_secs = self.shared.clock.lock().unix();
            let remaining = target_secs.saturating_sub(now_secs);
            if remaining == 0 {
                return;
            }
            // Sleep the remainder, then loop back to the top: the preference
            // may have moved or the work may have drained while we slept.
            // Each iteration either returns or sleeps a strictly positive
            // remainder — no busy-spin. Cancellation-safe: the forwarder
            // `select!`s this future against the chain token, so a sleeping
            // waiter is simply dropped at teardown.
            tokio::time::sleep(Duration::from_secs(remaining)).await;
        }
    }
```

Note the DashMap guard from `verified.get(..)` is confined to the `and_then` expression — it is dropped before the `.await` (never hold a lock across an await).

**(d)** Extend `pending_work_waiter()` (~line 907):

```rust
    fn pending_work_waiter(&self) -> Option<Arc<dyn PendingWorkWaiter>> {
        Some(Arc::new(EvmPendingWorkWaiter {
            atomic: Arc::clone(&self.txpool),
            evm: Arc::clone(&self.evm_mempool),
            shared: Arc::clone(&self.shared),
            preferred: Arc::clone(&self.preferred),
        }))
    }
```

- [ ] **Step 4: Run the full waiter + touched-surface tests**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm --test pending_work_waiter`
Expected: 5 PASS (3 pre-existing + 2 new). The pre-existing three still pass because `build_vm()` without `with_clock` keeps the `RealClock`, and "now" is years past `genesis + 2s` — pacing adds zero wait there.

Then the crate + the forwarder's crate:

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm -p ava-chains`
Expected: all PASS (the forwarder test in `ava-chains` drives a real `EvmVm` waiter; same RealClock reasoning).

- [ ] **Step 5: Commit**

```bash
git add crates/ava-evm/src/vm.rs crates/ava-evm/tests/pending_work_waiter.rs
git commit -m "feat(ava-evm): pace EvmPendingWorkWaiter on ACP-226 min delay — coreth waitForEvent parity (block_builder.go:140-214); closes the MinDelayNotMet build papercut"
```

---

### Task 3: Closeout — docs, workspace gates, live gates

**Files:**
- Modify: `plan/M9-interop-hardening.md` (short AS-BUILT callout under the semantic-verify AS-BUILT section: the ★ builder min-delay pacing follow-up from the final-review triage is now closed)
- Modify: `docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md` (flip `**Status:** approved` → `**Status:** implemented`)
- No code.

**Interfaces:**
- Consumes: Tasks 1-2 merged into the working tree.
- Produces: green whole-workspace + live gates; docs recording what landed.

- [ ] **Step 1: Docs**

In `plan/M9-interop-hardening.md`, immediately after the semantic-verify AS-BUILT block (the section ending with the 16-mutant corpus note, ~line 2210), add a short callout in the established `>` style:

- what landed: `feerules::min_next_block_time_ms` (coreth `minNextBlockTime`, `block_builder.go:202`) + the paced `EvmPendingWorkWaiter::wait()` (coreth `waitForEvent` parity), whole-second round-up rationale;
- the deliberate deviation: coreth's 100 ms `RetryDelay` retry arm unported — the forwarder's 2 s re-arm covers the retry-same-parent role;
- pointer to the design doc `docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md`.

Flip the design doc's status line to `implemented`.

- [ ] **Step 2: Full workspace gates**

```bash
./scripts/run_task.sh lint-all
./scripts/run_task.sh test-unit
```

Expected: both green. Fix anything surfaced (fmt drift, cross-crate breakage from the `preferred` Arc-wrap) before proceeding.

- [ ] **Step 3: Live gates**

Verify the oracle, prewarm a freshly-relinked release binary (macOS first-exec ~40 s stall — the stale-binary gotcha: `touch crates/ava-evm/src/lib.rs` first so the relink actually happens), then rerun both live arms:

```bash
./scripts/check_oracle_binary.sh
touch crates/ava-evm/src/lib.rs
./scripts/nix_run.sh cargo build -p avalanchers --release && ./target/release/avalanchers --version
cargo test -p ava-differential --features live --test mixed_network -- --ignored --exact --nocapture mixed_network
cargo test -p ava-differential --features live --test mixed_network -- --ignored --exact --nocapture mixed_network_rust_proposes
```

(Per the M9.15 gotcha notes: run live arms via `cargo test`, NOT nextest — the 120 s slow-timeout kills them. `$AVALANCHEGO_PATH` must point at the oracle binary; `check_oracle_binary.sh` must print `OK` first.)

Expected: both PASS. `mixed_network_rust_proposes` is the exact papercut scenario (Rust proposes shortly after a Go parent) — with pacing, the first build attempt should land instead of dying at `MinDelayNotMet` and retrying. A FAIL here is a pacing bug (e.g. over-waiting past the engine's patience or a wrong target computation) — debug against `block_builder.go`, do not weaken the wait.

- [ ] **Step 4: Final commit**

```bash
git add plan/M9-interop-hardening.md docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md
git commit -m "docs: builder min-delay pacing AS-BUILT — ACP-226 waitForEvent parity landed, MinDelayNotMet papercut closed"
```
