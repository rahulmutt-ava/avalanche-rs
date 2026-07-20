# Builder min-delay pacing (ACP-226) — design

**Date:** 2026-07-19
**Status:** implemented
**Follow-up from:** semantic-verify family merge `b5157d5` (final-review ★ triage item)

## Problem

A Rust-built C-Chain block whose timestamp lands less than the ACP-226 minimum
delay after its parent (2000 ms at `INITIAL_DELAY_EXCESS`) dies locally at its
own `verify_time` with `Error::MinDelayNotMet`. The block is dropped and the
per-chain forwarder's 2 s re-arm eventually retries, so this is consensus-safe
but a liveness papercut: proposals land up to 2 s late and every early attempt
burns a full VM-mutex build cycle.

Root cause: coreth paces the **build trigger** — `waitForEvent`
(`plugin/evm/block_builder.go:140`) sleeps until
`minNextBlockTime(parent) = parentTime + parent.MinDelayExcess.Delay()` before
returning `PendingTxs` to the engine, and the miner then stamps `now`
(`customheader.GetNextTimestamp`, `time.go:33`, whose comment explicitly defers
enforcement to `VerifyTime` because the caller *waited*). Our port of that
trigger seam — `EvmPendingWorkWaiter::wait()` (`crates/ava-evm/src/vm.rs`) plus
the per-chain forwarder in `crates/ava-chains/src/create_chain.rs` — resolves
the instant a mempool has work, with no time awareness.

## Scope

Port **only** the min-delay (`minNextBlockTime`) wait. Coreth's second pacing
arm — `lastBuildTime`/`lastBuildParentHash` retry tracking with a 100 ms
`RetryDelay` — stays unported; the forwarder's existing 2 s re-arm keeps
covering the retry-same-parent role. Documented as a deliberate deviation.

## Approach (chosen: A — pace in the waiter)

Considered:

- **A. Pace inside `EvmPendingWorkWaiter::wait()`** — coreth-parity seam (the
  waiter is our port of the Go `NotificationForwarder`'s `WaitForEvent` poll
  target). Trait, forwarder, and `build_block` unchanged; no VM lock held while
  sleeping (the M7.18 lock-parking hazard this seam exists to avoid). **Chosen.**
- **B. Decline inside `build_block`** ("too early" error, 2 s re-arm retries) —
  tiny diff but blocks still land up to 2 s late and error noise remains. Rejected.
- **C. Clamp the header timestamp forward** — coreth explicitly avoids this
  (`GetNextTimestamp` comment); future-dated headers flirt with peers'
  far-future `VerifyTime` bound. Rejected.

## Design

One production file changes (`crates/ava-evm/src/vm.rs`) plus one pure helper
in `crates/ava-evm/src/feerules/mod.rs`.

### New pure helper

```text
feerules::min_next_block_time_ms(parent: &AvaHeader) -> Option<u64>
```

Returns `header_time_ms(parent) + DelayExcess(parent.min_delay_excess).delay()`,
or `None` when `parent.min_delay_excess` is `None` (pre-Granite parent —
coreth's `minNextBlockTime` nil-arm: nothing to wait for). Both constituents
(`header_time_ms`, `DelayExcess::delay`) already exist in `feerules`.

### Waiter changes

`EvmPendingWorkWaiter` grows two handles, cloned at `pending_work_waiter()`
construction from what `EvmVm` already owns:

- `shared: Arc<Shared>` — the `verified` map (parent-header lookup) and the
  injected clock (`Shared.clock`, the seam the semantic-verify branch created).
- `preferred: Arc<ArcSwap<Id>>` — the `EvmVm.preferred` field is today a bare
  `ArcSwap<Id>`; wrap it in `Arc` (existing `self.preferred.…` call sites are
  unchanged via auto-deref).

`wait()` keeps its exact notify-race-safe subscribe-then-check loop and adds a
pacing tail. Full loop per iteration:

1. Subscribe to both pools' notifies, check `pending()`; if empty, park on the
   notifies and loop (unchanged).
2. Work exists. Look up `*preferred.load_full()` in `shared.verified`. Missing
   entry, or `min_next_block_time_ms(parent) == None` → **return** (fail-open,
   identical to today's behavior; `verify_time` remains the safety backstop).
3. Round the target up to the next whole second:
   `target_secs = min_next_ms.div_ceil(1000)`. Rationale: `build_block` stamps
   whole seconds (`timestamp_ms = secs * 1000`), and a Go-built parent can carry
   a mid-second ms timestamp — flooring would still fail `MinDelayNotMet`.
4. `remaining = target_secs.saturating_sub(shared.clock.lock().unix())`. If
   zero → **return**. Else `tokio::time::sleep(Duration::from_secs(remaining))`
   and **loop back to step 1** (re-subscribe, re-check pending, recompute — the
   preference may have moved or the work may have drained during the sleep).

Delay is computed from the **injected** clock; the sleep itself is tokio time —
the same split the forwarder's 2 s re-arm already uses, so no new
determinism-gate surface.

### What does not change

- `PendingWorkWaiter` trait and the generic forwarder loop in `ava-chains`
  (P/X/SAE still return `None` and are untouched).
- `build_block`'s timestamp derivation (`clock.unix().max(parent.time + 1)`).
- The forwarder's 2 s re-arm (still the retry/recovery path).

## Edge cases & failure posture

Fail-open throughout: every abnormal path skips the wait and resolves
immediately, collapsing to today's behavior with `verify_time` as backstop.

- **Preference moves mid-sleep** — the post-sleep loop re-check recomputes
  against the new preferred. Each iteration either returns or sleeps a strictly
  positive remainder → no busy-spin.
- **Work drained mid-sleep** (concurrent build packed it) — the re-check parks
  the waiter again instead of firing a spurious `PendingTxs`.
- **Long delays / shutdown** — the forwarder already `select!`s `wait()`
  against the chain `CancellationToken`; a sleeping waiter is dropped cleanly.
- **Parent stamped ahead of our clock** (peer skew) — `saturating_sub` yields a
  bounded wait until our clock clears it; building earlier would only fail
  verify anyway.

## Testing

Offline:

1. **Helper unit tests** (`feerules`): pre-Granite parent → `None`; Granite
   parent → `parent_ms + delay`; a mid-second parent ms value proves the
   ceil-to-whole-second rounding at the call site.
2. **Waiter pacing tests** (`ava-evm`, paused tokio time + `MockClock`): clock
   already past target → `wait()` resolves immediately; clock behind →
   `wait()` still pending before the target, resolves after advancing both
   clocks past it.
3. **End-to-end self-verify regression test** (`ava-evm`): Granite parent,
   admit a tx, drive the paced path — the built block passes its own `verify`
   (no `MinDelayNotMet`).

Live (acceptance bar):

4. `./scripts/check_oracle_binary.sh` → prewarm the freshly-linked release
   binary (`--version` once) → re-run `mixed_network` (follower, ~38 s) and
   `mixed_network_rust_proposes` (~29 s), both GREEN. `rust_proposes` is the
   exact papercut scenario (Rust builds shortly after a Go parent).

Standard gates: scoped `cargo nextest -p ava-evm -p ava-chains`, clippy
`--all-targets -D warnings`, fmt, lint-determinism.

## Deliberate deviations from coreth (documented)

- No `RetryDelay`(100 ms)/`lastBuildTime` retry arm — the 2 s forwarder re-arm
  covers it (coarser but out of scope).
- Rust stamps whole-second timestamps (coreth stamps real `UnixMilli`); the
  wait rounds up to the next whole second so the stamp still clears the delay.
  Whole-second `TimeMilliseconds` values are a valid subset under `VerifyTime`.

## AS-BUILT notes

> **AS-BUILT note (2026-07-20):** pacing lives in `EvmPendingWorkWaiter::wait()` only. `EvmVm::wait_for_event` (the rpcchainvm guest path, `ava-vm-rpc/src/guest/mod.rs:234`) remains unpaced — deliberate asymmetry, latent today because no production path serves the Rust C-Chain EVM as a plugin; revisit if a plugin arm ships. The `ava-chains` forwarder's 2 s re-arm was subsequently paced through `wait()` on 2026-07-20 (`forward_pending_work`; see `2026-07-20-forwarder-rearm-pacing-design.md`).
