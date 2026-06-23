# M9.15 — Networked Peer Handshake → Bootstrap-to-Finished

**Date:** 2026-06-23
**Status:** Design approved, pending spec review
**Milestone:** M9 (interop & hardening), the live `differential::mixed_network` arm
**Related:** `plan/M9-interop-hardening.md`, `specs/05-networking-p2p.md`,
`specs/06-consensus.md`, `specs/19-state-sync-and-bootstrap.md`,
prior STEP-o/p/q `OutboundSender` work, memory `m9.15-live-mixed-net-blocker`.

## Problem

`avalanchers` cannot complete a P2P peer handshake with a remote node and then
bootstrap a chain over the network. Observed against a live Go `avalanchego`
beacon: the Rust follower dials, starts TLS-1.3 mutual auth, but **loops
reconnecting roughly every 250 ms**; the Go beacon never registers the peer;
networked bootstrap never starts; `info.isBootstrapped` stays false.

All of `avalanchers`' chain-readiness to date is **in-process / beaconless**
(`drive_startup_chains`). The standalone **networked-bootstrap-from-a-remote-peer**
path has never been operational. This is the last open frontier of M9.15 (and
hence M9): every CI-runnable *offline* arm is already done; only the live
two-binary arm remains, and it is blocked here.

### Key diagnostic facts (from code map, 2026-06-23)

- **The ~250 ms loop is a fast-failing connection, not a hang.** A peer stuck
  mid-handshake sits in `connecting`, which the dialer *skips*
  (`ava-network/src/network/net_impl.rs:276`). Re-dialing every 250 ms (=
  `DIAL_SCAN_INTERVAL`, `net_impl.rs:44`) means each attempt *completes with an
  error fast* — so the failure is at/around the TLS upgrade or the first
  application frame, not a stalled handshake.
- **`TrackedIp::delay` (exponential backoff, 1s→60s) is defined but never
  applied** (`tracked_ip.rs`); the dialer re-dials every unconnected tracked IP
  every 250 ms with zero backoff — a real correctness bug independent of Go
  interop.
- **Even with a perfect handshake, bootstrap still would not run.** At
  node-assembly, `RouterBridge::handle_inbound()` *drops* all consensus messages
  and `ExternalHandler::connected()`/`disconnected()` forward to nothing
  (`ava-node/src/init/networking.rs:82-98`).
- **The consensus engine is complete.** `bootstrap/mod.rs` (requester),
  `getter.rs` (responder), and `engine_adapter.rs` (inbound dispatch) all exist.
  But the `Getter` responder is **not** wired into `ChainHandler` dispatch — both
  engine adapters drop `Get*` ops (`engine_adapter.rs:146` bootstrap, `:224`
  snowman). So a beacon cannot currently *answer* a follower's
  `GetAcceptedFrontier`/`GetAncestors`.

The work is therefore **glue + a harness + a correctness fix**, not new consensus
logic.

## Goals

1. A Rust node completes a real networked peer handshake (over a TLS socket, not
   a loopback duplex) and registers the peer into the engine.
2. A follower Rust node bootstraps a chain from a beacon Rust node over the
   network **to `Phase::Finished`**, ending with `last_accepted == beacon.tip`.
3. The reconnect loop is replaced by correct exponential backoff.
4. The live Go-interop failure is root-caused and fixed if tractable; otherwise
   recorded honestly with the live arm left gated.

## Non-goals (explicitly out of scope)

- **PeerList gossip-driven dialing.** `handle_peer_list()` stores claimed IPs
  (`handshake.rs:125`) but the network never dials them; beacon-driven bootstrap
  does not need it. Documented-deferred.
- **BLS proof-of-possession re-verification** at handshake (`peer.rs:544-547`) —
  blocked on the validator-set source; TLS signed-IP verification stands.
- **Public-IP resolver** (`UnsupportedResolver` stub) — beacons are
  manually-tracked from config.

## Architecture: two parallel tracks

### Track 1 — Two-Rust-node networked bootstrap (CI spine)

Two in-process node stacks (`NetworkImpl` + engine) in one test binary, over
**real TLS on localhost**:
- Node **A** = beacon, seeded genesis-bootstrapped at height ≥ 1, in `NormalOp`.
- Node **B** = follower, with A as its sole configured beacon.

B must: handshake A → register A (beacon) → `on_sufficiently_connected` fires →
bootstrapper `start` → frontier discovery → ancestor fetch → `Phase::Finished`,
ending `B.last_accepted == A.tip`. Deterministic; no Go dependency; runs every CI.

A two-*process* variant (two `avalanchers` binaries) becomes the nightly live arm.

#### Components

**G1 — Inbound network→engine routing** (`ava-node/src/init/networking.rs`).
Replace the `handle_inbound()` no-op: decode the `proto/p2p` message via
`ava-message` into an `InboundOp`, then push onto the target chain's
`ChainHandlerSink`. This is the symmetric inverse of the existing `OutboundSender`
(engine→p2p). Reuse `ava-message` parse + the `InboundOp` mapping in
`ava-engine/src/networking/router.rs`.

**G2 — Peer lifecycle→engine** (`ava-node/src/init/networking.rs:92-98`).
Wire `ExternalHandler::connected()` through the `BeaconManager`
(`init/networking.rs:374-389`, already counts beacons and fires
`on_sufficiently_connected` at the `(3·n+3)/4` threshold) into the bootstrapper
start trigger; wire `disconnected()` to the engine's peer-tracking. Respect the
existing init ordering (ExternalHandler created step 16, `engine_router` slot
filled step 20) — forward via the settable slot.

**G3 — `Getter` responder wiring** (`ava-engine`). Dispatch `Get*` ops
(`GetAcceptedFrontier`/`GetAncestors`/`GetAccepted`/`Get`) to the `Getter`
(`getter.rs`) inside `ChainHandler` *before* engine-specific dispatch, mirroring
Go's `common.AllGetsServer`, so they are answered in **every** engine state
(a `NormalOp` beacon still serves frontier/ancestors). Today both adapters drop
these ops.

**G4 — Reconnect backoff** (`ava-network/src/network/{net_impl,tracked_ip}.rs`).
Apply `TrackedIp::delay` in the dial scan: skip an IP whose backoff has not
elapsed, double the delay on a failed dial (cap 60s), reset to 1s on a successful
connection. Clock-injected via `Arc<dyn Clock>` for determinism.

**G5 — Two-node test harness.** Build the in-process A↔B harness over real
localhost TLS; assert B reaches `Phase::Finished` and matches A's tip. CI-runnable.

### Track 2 — Live Go diagnosis (parallel, heavy/sequential)

**D1 — App-level handshake logging** (`ava-network/src/peer/{handshake,upgrader}.rs`).
Add structured `tracing` at each handshake rung — TLS upgraded, Handshake
sent/received, each validation check (network ID, clock skew, version, subnets,
ACP, IP+port, signed-IP), PeerList received, `finish_handshake`. Only rustls
DEBUG surfaces today. Independently useful; the diagnostic key.

**D2 — Live capture.** Verify the oracle binary (`scripts/check_oracle_binary.sh`;
mise-PATH gotcha: prepend `~/.local/share/mise/installs/go/1.25.10/bin`), boot a
Go beacon + Rust follower (reuse `boot_mixed`), and pin the failing rung.
Leading hypothesis: **signed-IP signature** mismatch (a fast post-TLS reject fits
the 250 ms loop). Alternatives: `Handshake` message wire incompat (ACP fields,
client-version string format), or a rustls↔Go TLS quirk.

**D3 — Fix if tractable.** Apply the fix in priority order (signed-IP signature
scheme/hash → Handshake wire → TLS). If large, record findings + leave the live
arm `#[cfg(feature="live")] #[ignore]`; Track 1 still lands green.

## Testing & quality gates

- **Determinism:** all new clock reads (backoff, handshake timestamps) injected
  via `Arc<dyn Clock>`; no wall-clock. Passes `lint-determinism`.
- **CI:** G5 in-process test runs every CI. Two-process + live-Go arms are
  `#[cfg(feature="live")] #[ignore]`, nightly-gated (`nightly.yml` `test-live`).
- **Unit tests:** G1 (decode round-trip p2p→InboundOp→sink), G2 (connected→start
  trigger fires at threshold), G3 (beacon answers `GetAcceptedFrontier`/
  `GetAncestors` in `NormalOp`), G4 (backoff schedule + reset, MockClock).
- **Lints:** `clippy -D warnings`, rustfmt, no `unwrap`/`expect` in lib code,
  per-crate `thiserror` errors. End each worktree wave with a full-workspace
  `nextest` + `fmt --check` in the main tree (stale-binary worktree gotcha).

## Parallelization

G1/G2 (ava-node) and G3 (ava-engine) and G4 (ava-network) touch disjoint crates →
parallel-worktree-safe. G5 depends on G1–G4 (integration). Track 2 (D1–D3) is
sequential/live, single-track. The writing-plans step will sequence these.

## Risks & unknowns

- **`InboundOp` mapping completeness** (G1): `router.rs` maps op tags for
  *failed* synthesis; the real `p2p::Message → InboundOp` decode may need
  building for the frontier/ancestors ops. Verify early.
- **Init ordering** (G2): the ExternalHandler/engine_router two-phase wiring must
  not introduce a race; forward through the existing settable slot.
- **Getter state-independence** (G3): confirm the `Getter` reads the VM's
  last-accepted + ancestors without requiring `NormalOp`-only invariants.
- **Live fix size** (D3): the signed-IP or wire incompat could be deep; the
  design tolerates "diagnose + record" as a valid session outcome with Track 1
  green.
