# Bootstrap frontier/accepted failure accounting + un-gate `beaconed_bootstrap`

**Date:** 2026-07-01
**Milestone:** M9.15 (interop hardening — follow-up)
**Status:** Approved design; ready for implementation plan

## Summary

The Snowman bootstrapper's frontier-discovery and frontier-agreement phases
require replies from **every** queried beacon and have **no failure/timeout
accounting**. Any beacon that is not connected when the one-shot query is
broadcast — or whose reply is lost — hangs bootstrap forever. This is a real
production liveness bug; it was surfaced by the bimodally-flaky (`#[ignore]`d)
end-to-end test `follower_bootstraps_through_real_beacon_gate`
(`crates/avalanchers/tests/beaconed_bootstrap.rs`).

The fix ports Go's failure handling (`GetAcceptedFrontierFailed` /
`GetAcceptedFailed` record an *empty opinion* that still counts toward phase
completion) and fixes a secondary frozen-clock bug that disables the
request-timeout backstop on the test-boot path. Then the e2e test is un-gated.

## Root cause (confirmed)

Investigation used a diagnostic that polled both-sides `peer_info` + the beacon
gate every 200ms during the wedge, plus a refuting experiment. Findings:

1. **Trigger.** The beacon-connectivity gate fires at
   `required_conns = (3·n + 3) / 4` (`crates/ava-node/src/init/networking.rs`),
   which for `n = 5` beacons is **4** — deliberately *fewer than all* beacons
   (Go parity). When the gate fires, the bootstrapper broadcasts
   `GetAcceptedFrontier` **once** to the beacon set, but
   `ava_network::Network::send` delivers only to *currently-connected* peers. If
   the 5th beacon has not completed its handshake at that instant (or any reply
   is later lost), it never contributes a frontier reply. The diagnostic
   confirmed the wedge sits with all 5 beacons connected and `gate = true` yet
   never reaches `NormalOp` — i.e. the stall is *downstream of connectivity*.

2. **Primary fault (production consensus).** In
   `crates/ava-engine/src/snowman/bootstrap/mod.rs`, `accepted_frontier` advances
   only when `frontier_replies.len() == cfg.beacons.len()` (all beacons), and
   `accepted` likewise (`accepted_replies.len() == beacons.len()`). There are
   **no** `get_accepted_frontier_failed` / `get_accepted_failed` handlers, and
   `crates/ava-engine/src/networking/engine_adapter.rs` does not dispatch
   `InboundOp::GetAcceptedFrontierFailed` / `GetAcceptedFailed` to anything (only
   `GetAncestorsFailed` → `get_ancestors_failed` is wired). So a missing reply
   from any beacon is unrecoverable. **Go's bootstrapper handles those `*Failed`
   ops** (`minority.RecordOpinion(node, nil)` /
   `majority.RecordOpinion(node, nil)`) and proceeds with the responding subset.

3. **Secondary fault (test-boot path only).** `boot_chain_over_network`
   (`crates/avalanchers/src/wiring/chains.rs:1571`) injects
   `MockClock::at(UNIX_EPOCH)` into the `AdaptiveTimeoutManager`.
   `MockClock::monotonic()` latches `tokio::time::Instant::now()` on first call
   and never advances (no `advance()` call on this path), while
   `fire_expired` (`crates/ava-engine/src/networking/timeout.rs`) compares
   `deadline <= clock.monotonic()`. Since `deadline = monotonic() + timeout` and
   `monotonic()` is frozen, `deadline <= now` is never true — **request timeouts
   never fire**. Production is unaffected: `chain_manager.rs:377` and
   `drive_startup_chains_over_network` (chains.rs:1937) use `RealClock`.

**Both #2 and #3 must be fixed.** A minimal experiment swapping only the clock to
`RealClock` (fixing #3 alone) left the wedge unchanged (~43% over 8 runs),
because the timeout *does* then synthesize `GetAcceptedFrontierFailed` but #2
swallows it. The timeout is registered for every queried beacon
(`sender.rs:250,273` call `register(nodes, …)` over the full set, independent of
delivery), so with both fixes a missing reply from any beacon recovers via the
`*Failed` path after the request timeout.

## Non-goals

- The **concurrent self-dial TOCTOU** in `ava-network`'s `run_dialer` (a second
  `handle_dial` can be dispatched to a node before the first completes its TLS
  upgrade and enters `connecting`, unlike Go's one-goroutine-per-tracked-IP
  model). This was investigated and **ruled out** as the cause of this wedge
  (connectivity was always healthy in the diagnostic). It is a separate latent
  bug and is recorded as a follow-up, not fixed here.
- Changing the beacon-gate threshold formula (`(3n+3)/4`) — it is correct Go
  parity; the fix is to tolerate the resulting reply gap, as Go does.

## Design

### Change 1 — Bootstrapper failure accounting (Go parity)

File: `crates/ava-engine/src/snowman/bootstrap/mod.rs`

- Track responded-or-failed beacons per phase, e.g. add
  `frontier_responded: BTreeSet<NodeId>` and `accepted_responded: BTreeSet<NodeId>`
  (or a `failed` set alongside the existing reply maps).
- `accepted_frontier(node, req, id)`: record the reply **and** mark the node
  responded.
- New `get_accepted_frontier_failed(node, req)`: honor the stale-request guard
  (`req`/phase check, as `accepted_frontier` does), then mark the node responded
  with **no** contributed id (empty opinion). Then run the same
  phase-completion check.
- Phase completes (begins agreement) when `frontier_responded == beacons`
  (every queried beacon responded or failed), building the frontier from the
  replies that actually arrived.
- Symmetric `get_accepted_failed(node, req)` and completion change for the
  `AgreeingFrontier` phase (`accepted` / `accepted_replies`).
- Empty-set edge: if a phase finalizes with zero contributed frontier/accepted
  ids, mirror Go — restart bootstrapping (bounded, to avoid a tight loop) rather
  than hang. (Unlikely in the e2e, which always has responders, but it is the
  parity behavior and gets a unit test.)

### Change 2 — Dispatch the `*Failed` ops

File: `crates/ava-engine/src/networking/engine_adapter.rs`

Route `InboundOp::GetAcceptedFrontierFailed { request_id }` →
`bootstrapper.get_accepted_frontier_failed(node, request_id)` and
`InboundOp::GetAcceptedFailed { request_id }` →
`bootstrapper.get_accepted_failed(node, request_id)`, mirroring the existing
`GetAncestorsFailed` → `get_ancestors_failed` wiring. The router already
synthesizes these ops on timeout (`router.rs:193–194`).

### Change 3 — Real monotonic clock for the test-boot timeout manager

File: `crates/avalanchers/src/wiring/chains.rs`

- In `boot_chain_over_network` (line ~1571), build the `AdaptiveTimeoutManager`
  with `RealClock` instead of `MockClock::at(UNIX_EPOCH)`, matching production
  (`chain_manager.rs:377`, `drive_startup_chains_over_network`).
- Add a comment (and consider a `debug_assert`/doc note on
  `AdaptiveTimeoutManager`) that the timeout manager requires an
  advancing monotonic clock; a frozen `MockClock` silently disables all
  timeouts.
- Leave the two in-process `RecordingSender` paths (chains.rs:406, 1082) on the
  frozen clock — loopback delivery cannot lose messages, so their timeouts are
  never needed — but add a one-line comment explaining why they differ.

### Change 4 — Un-gate the e2e; remove the diagnostic

File: `crates/avalanchers/tests/beaconed_bootstrap.rs`

- Remove `#[ignore]` from `follower_bootstraps_through_real_beacon_gate` and its
  stale doc paragraph about the "bring-up race". Keep the 120s bound.
- Remove the temporary `diag_beacon_wedge` diagnostic added during
  investigation.

## Testing / verification

- **Unit (deterministic, `ava-engine`):**
  - A bootstrapper with 5 beacons where 1 never replies: without the fix the
    frontier phase never advances; after `get_accepted_frontier_failed` for that
    node it finalizes on the other 4 and proceeds. RED→GREEN.
  - Symmetric test for `get_accepted_failed` in the agreement phase.
  - All-beacons-failed → restart edge.
  - Stale-request-id `*Failed` is ignored.
- **Integration:** run `follower_bootstraps_through_real_beacon_gate` **~30×**
  with zero wedges; re-run `networked_bootstrap.rs` (also uses
  `boot_chain_over_network`) for no regression.
- **Gates:** `./scripts/run_task.sh lint-all`; `test-unit` for `ava-engine`,
  `avalanchers`, `ava-node`.

## Follow-ups (out of scope)

- Port Go's one-dialer-per-tracked-IP model (or an in-flight-dial guard) to
  `ava-network`'s `run_dialer` to eliminate the concurrent self-dial TOCTOU.
- Nightly-gated live `mixed_network` two-binary arm remains gated by design.
