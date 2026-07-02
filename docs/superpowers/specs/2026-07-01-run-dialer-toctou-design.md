# M9.15 follow-up — `run_dialer` concurrent self-dial TOCTOU

**Date:** 2026-07-01
**Crate:** `ava-network`
**Status:** design approved, ready for plan
**Source:** the "Remaining follow-up" recorded in `plan/M9-interop-hardening.md`
(AS-BUILT, 2026-07-01 bootstrap-failure-accounting) — the last named open bug
from the beacon-gate work.

## Problem

`NetworkImpl::run_dialer` (`crates/ava-network/src/network/net_impl.rs`) ticks
every `DIAL_SCAN_INTERVAL` (250ms) and selects a tracked node to dial when it is
**not** in `connected` or `connecting`. `handle_dial` then spawns an async task
that performs the TCP dial + TLS upgrade and only inserts the node into
`connecting` **after** the upgrade completes (via `admit_peer`). During that
window the node is in neither set.

`TrackedIp::record_attempt` gates re-dials for ~1–2s (the jittered initial
backoff), so for fast dials the gap is invisible. But when an upgrade **stalls
beyond the backoff window** — exactly the rustls↔Go TLS-1.3 mutual-handshake
stall observed in the live mixed-net work — the next qualifying dialer tick
re-selects the same node and launches a **second** concurrent dial, then a
third, and so on. `admit_peer` dedups the eventual winners (all but one socket
is dropped), but the churn of duplicate half-open connections to the Go peer is
real and aggravates the half-dead-connection failure mode.

Go avoids this structurally: `network.go` runs **one dial goroutine per tracked
IP**, so a given IP has exactly one in-flight dial at a time. Our scan-based
dialer has no equivalent guard.

This bug was investigated during the beacon-gate work and **ruled out** as the
cause of the `beaconed_bootstrap` wedge, but it is a genuine latent bug and the
last named M9.15 follow-up.

## Approach (chosen: in-flight-dial guard set)

Add a `dialing: Mutex<HashSet<NodeId>>` guard set that records nodes with an
in-flight outbound dial. The scan dialer skips nodes already in `dialing` (in
addition to `connected`/`connecting`); `handle_dial` clears the mark on any exit
path. This gives Go's one-dial-per-IP property without restructuring the scan
loop.

Alternatives considered and rejected:

- **Port Go's per-IP dialer loop** (one long-lived task per tracked IP). Truest
  parity but a real restructure of `run_dialer` plus per-IP task lifecycle/
  teardown — larger surface and risk for the same observable guarantee.
- **Insert a placeholder into `connecting` at dial launch.** `connecting` holds
  `PeerHandle`s and no peer actor exists until after the upgrade, so this means
  faking a handle or widening the set's element type — messier than a dedicated
  guard set.

## Design

### 1. Data structure & selection

Add to `NetworkImpl`:

```rust
/// Node-ids with an in-flight outbound dial (spawned, not yet admitted or
/// failed). The scan dialer skips these so a slow/stalling TLS upgrade does
/// not accumulate duplicate concurrent dials to the same peer — Go runs one
/// dial goroutine per tracked IP; this guard set is the scan-loop equivalent.
dialing: Mutex<std::collections::HashSet<NodeId>>,
```

Factor the tick body into a testable helper:

```rust
fn select_dial_targets(&self, now: Instant) -> Vec<(NodeId, SocketAddr)>
```

Under the `tracked_ips` and `dialing` locks it:

- skips nodes in `connected`, `connecting`, or `dialing`;
- applies `should_dial(now)`;
- calls `record_attempt(now)`;
- **inserts the node into `dialing`**;
- returns the `(node_id, addr)` targets.

`run_dialer` calls `select_dial_targets(Instant::now())` on each tick and spawns
`handle_dial(node_id, addr)` per target. `select_dial_targets` marks each node
before returning, and only the single dialer task inserts, so no target is lost
or double-launched.

### 2. Clearing the guard (`handle_dial`)

`handle_dial` gains a `node_id: NodeId` parameter (currently discarded by the
caller). The dial task has three exit paths — dial-failure early `return`,
upgrade-failure, and successful `admit_peer` — so the mark is cleared with a
`Drop` guard rather than repeated removal at each exit:

```rust
/// Removes `node` from the in-flight `dialing` set on drop, covering every
/// exit path of the dial task (dial failure, upgrade failure, admit).
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

`handle_dial` constructs the guard at the top of its spawned task:
`let _guard = DialGuard { net: Arc::clone(&this), node: node_id };`.
`parking_lot::Mutex` is synchronous, so the `Drop` lock holds no lock across an
`.await` — consistent with the existing "leaf lock, never across await"
invariant on `peers_lock`. This makes clearing panic- and cancellation-safe.

### 3. Testing

Primary deterministic guard (mirrors `admit_peer_dedups_same_node_id`), a unit
test on `select_dial_targets` driven by a synthetic `Instant` clock:

1. Register one tracked IP. `select_dial_targets(t0)` → returns the node; node
   now in `dialing`.
2. Advance past the backoff window: `select_dial_targets(t0 + 3s)` → returns
   **empty** (node held out by `dialing` even though `should_dial` would now
   pass). *This assertion fails on current `main` — it is the RED test.*
3. Simulate dial completion: `net.dialing.lock().remove(&node)`.
   `select_dial_targets(t0 + 6s)` → returns the node again (re-dial after the
   guard clears).

Also assert `dialing` membership transitions directly, and add a focused test
that `DialGuard::drop` removes the mark. No timing-dependent end-to-end test —
the stall race is factored out into the synchronous helper.

## Verification

- TDD: write the step-2 assertion first (RED on current code), then add the
  guard set + helper (GREEN).
- `cargo nextest run -p ava-network` and `-p ava-node` (the beacon tests
  exercise the dialer).
- `./scripts/run_task.sh lint` (clippy `-D warnings` + rustfmt + license).

## Non-goals

- No change to the backoff/jitter model (`TrackedIp`).
- No restructure into per-IP dial tasks (approach B).
- No change to `admit_peer`'s dedup, which remains the correctness backstop for
  the inbound path and for any dial that still races through.
