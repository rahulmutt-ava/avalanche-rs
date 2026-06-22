# M9 nightly arm — live mixed Go+Rust network harness (design)

**Date:** 2026-06-22
**Status:** approved (brainstorming), ready for implementation plan
**Owning plan task:** M9.15 (`differential::mixed_network`) — the last remaining live two-binary arm
**Owning specs:** `16` §5 (drop-in definition of done, clause 2: live mixed net, no fork, same tip), `02` §11 (differential harness, tier-X X.15), `00` §11.2 (risks)

## Problem

Every M9 task has a green **offline** arm in CI, and the nightly scaffolding exists
(`.github/workflows/nightly.yml` → `test-live` task → the `#[cfg(feature="live")]`
`#[ignore]` arms). But the **live** arms do not actually execute end-to-end:

- They early-return when `$AVALANCHEGO_PATH` is unset.
- The bodies are partially stubbed. `tests/differential/tests/mixed_network.rs`
  has a `TODO(operator)` where the RPC driver loop should be, and
  `tests/differential/src/network.rs::spawn_node` launches each binary with only
  `--network-id=local` + per-node ports — it does **not** wire a shared genesis,
  matching staking certs, or a bootstrap topology, so the launched nodes would
  never form one interconnected network.

Only M9.3's live handshake (a Rust plugin under a Go host) has actually been
driven green against the real `~/avalanchego` binary.

The shared bring-up + RPC-driver + observation-collection substrate is the
common dependency under `mixed_network`, `test-upgrade`, and `test-load`. This
design builds that substrate and proves it by landing the full `mixed_network`
arm for real this session.

## Goal & success criteria

Build a `LiveMixedNet` harness that boots a real two-binary Go+Rust network and,
driven by a transaction, proves no fork / same tip. Definition of done for this
session:

`cargo nextest run -p ava-differential --features live --run-ignored all -E 'test(mixed_network)'`
runs green against the real binaries, having actually:

1. completed a real TLS handshake between Go and Rust over the wire,
2. brought the Rust node to `info.isBootstrapped` = true for P, X, C (synced from Go),
3. issued and committed one transaction, and
4. asserted both nodes report a byte-identical normalized `Observation` (same
   per-chain last-accepted id + height + root + sorted validator set).

We build up through escalating rungs (handshake → bootstrapped → tip-at-rest →
driven-tx). If a rung exposes a real gap (e.g. Rust does not accept Go's gossiped
blocks), we stop and fix it via TDD with the lower rung green underneath, and
record the gap honestly rather than faking the assert.

## Approach (chosen: B — harness owns both launches, Go-as-sole-validator)

The harness directly launches both binaries with explicit, matched flags. Go is
the only validator with stake; the Rust node bootstraps from Go and follows.

Rejected alternatives:
- **A (lean on Go `tmpnet`):** couples the Rust harness to a Go test harness's
  internals/file layout; brittle across tmpnet versions.
- **C (symmetric mutual-validation):** strongest "no fork" proof, but needs
  Rust's engine to *win and answer* polls against Go under a real quorum — the
  least-proven path. Documented as a follow-up once B is green.

### Topology

```
            ~/avalanchego/build/avalanchego          target/release/avalanchers
                    (Go beacon)                            (Rust follower)
   role:    sole initial-staker / validator        non-validating bootstrapper
   cert:    staking/local/staker1.{crt,key}         staking/local/staker2.{crt,key}
   net:     --network-id=local                      --network-id=local  (byte-identical genesis)
   ports:   staking=PG_s  http=PG_h                 staking=PR_s  http=PR_h
   topo:    beacon (no bootstrap flags)             --bootstrap-ips=127.0.0.1:PG_s
                                                     --bootstrap-ids=<Go node-id, scraped>
```

- **Single source of consensus truth.** Go (staker1 ∈ local genesis initial
  stakers) proposes/finalizes; Rust bootstraps and accepts Go's finalized blocks.
  "No fork / same tip" = Rust faithfully follows. This sidesteps the unproven
  "Rust wins polls against Go" path.
- **Genesis parity** comes free from `--network-id=local` (both embed the same
  `UNMODIFIED_LOCAL_CONFIG`, the byte-faithful port of Go's local genesis). No
  genesis file is generated or passed.
- **Cert source.** Read from `$AVALANCHEGO_SRC/staking/local/staker{1,2}.{crt,key}`
  (+ `signer{1,2}.key` for BLS), fed via `--staking-tls-cert-file` /
  `--staking-tls-key-file`. Both binaries consume identical files, so node-IDs
  match the genesis initial stakers.

## Components & data flow (bring-up sequence)

Implemented in `tests/differential/src/network.rs`, extending the existing
`Network` / `spawn_node` / `NodeIdentity`.

1. **Pre-gate:** run `scripts/check_oracle_binary.sh` (binary-commit ==
   `~/avalanchego` HEAD; asserts rpcchainvm=45). Abort on mismatch.
2. **Port allocation:** acquire 4 free localhost ports (2 per node),
   deterministic-from-seed with a fallback to OS-assigned, mirroring
   `NodeIdentity`.
3. **Launch Go beacon** (staker1, no bootstrap flags). Poll `info.getNodeID` +
   `info.isBootstrapped` until healthy; capture the node-ID.
4. **Launch Rust follower** (staker2, `--bootstrap-ips/-ids` = Go's staking addr +
   scraped node-ID).
5. **`await_all_connected`:** poll `info.peers` on both until each lists the
   other (real TLS handshake done over the wire).
6. **`await_bootstrapped`:** poll `info.isBootstrapped` for P, X, C on the Rust
   node until all `true` (synced genesis chains from Go).
7. **Driver:** issue one funded **P-chain `CreateSubnetTx`** (local genesis
   well-known key) to the Go node's `/ext/bc/P`; poll `platform.getTxStatus`
   until `Committed`. **Dependency note:** `platform.issueTx` takes a *signed* tx,
   so the driver must build+sign the tx (via `ava-wallet` keyed off the local
   genesis alloc) — the same signing gap the M9.18 load arm left unwired. If
   wiring `ava-wallet` into the driver proves heavy, the rung-staging fallback is
   to drive the simplest signable tx available (e.g. a C-chain `eth_sendRawTransaction`
   transfer from a prefunded local key, signed with `ava-crypto` secp256k1) and
   assert same-tip off the C-chain `eth_blockNumber` — recorded honestly as the
   tx vehicle actually used.
8. **Settle + assert:** poll both nodes' `platform.getHeight` until equal/stable,
   then `Observation::collect(api_base).normalized()` on both → `assert_eq!`
   (same P/X/C last-accepted id + height + root + sorted validator set).
   Divergence ⇒ fail with the per-field diff.
9. **Teardown:** `kill_on_drop(true)` + explicit `shutdown()`.

`Observation::collect` already queries real endpoints (`info.getNodeID`,
`info.getNodeVersion`, `platform.getHeight`, `platform.getCurrentValidators`,
`eth_blockNumber`) — no changes needed there.

## Error handling & failure modes

Every step has a bounded timeout and produces a diagnostic, never a hang:

| Failure | Handling |
|---|---|
| Oracle binary stale (commit ≠ HEAD / not v45) | abort before any launch; message points at `cd ~/avalanchego && ./scripts/build.sh` |
| Cert files missing (`$AVALANCHEGO_SRC` unset / no `staking/local/`) | `NetworkError::CertSource` with the expected path; live test early-returns (treated like missing `$AVALANCHEGO_PATH`) |
| Spawn fails | existing `NetworkError::Spawn { slot, binary, source }` |
| Node never healthy / never bootstraps | timeout (`bootstrap` ~120s, `connect` ~60s) → `NetworkError::Timeout { stage, slot }`, with the node's `node.log` tail folded into the error so a nightly failure is debuggable from CI logs alone |
| Peers never see each other | same timeout path, stage=`connect` |
| tx never `Committed` | `NetworkError::TxUnconfirmed { tx_id, last_status }` |
| Tip diverges (real fork) | `assert_eq!` panic with the per-field `Observation` diff (the test signal) |
| Panic mid-run | `kill_on_drop(true)` on every child guarantees no orphaned nodes |

Timeouts are module-level constants (tunable for a slow CI runner). All polling
uses a fixed interval + deadline; no unbounded loops.

## Testing & verification

**Offline (every CI run):**
- Existing in-process `replay_recorded` determinism + proptest arms stay green
  (no behavior change to offline paths).
- New offline unit tests for the *pure* new logic only: port allocation, flag-
  vector assembly (assert the exact `--bootstrap-ips/-ids`, cert-path,
  `--network-id=local` args per role), and node-ID scraping parse. No binaries
  needed.

**Live arm (`mixed_network`, `#[cfg(feature="live")] #[ignore]`):**
- Replace the `TODO(operator)` body with a real call into
  `LiveMixedNet::boot_and_drive(seed)` → the bring-up sequence → the no-fork
  assert.
- Early-return cleanly when `$AVALANCHEGO_PATH` unset (keeps `test-live`
  build-safe).

**This session — the actual proof:**
- Build `avalanchers` release + confirm `~/avalanchego/build/avalanchego` via
  `check_oracle_binary.sh`.
- Run the live `mixed_network` arm against the real binaries and show it green,
  built up through the rungs.

## As-built gap (2026-06-22)

The substrate (bring-up + RPC driver + observation) was built and exercised
against the real binaries. The live arm reached: **Go beacon healthy + Rust
follower boots**, then the follower loops a TLS-1.3 mutual-auth handshake against
the beacon without completing peer establishment (Go never registers the peer),
so bootstrap never starts and `await_bootstrapped` times out. Two interop gaps
were fixed harness-side to get that far — `avalanchers` rejects the RSA local
staker keys Go uses (only ECDSA-P256 PKCS#8), so the non-validating follower uses
a generated ECDSA cert; and the release build lacks the optional `rocksdb`
backend, so both nodes run `--db-type=memdb`. The remaining blocker — a real
peer handshake + networked bootstrap from a live Go beacon in standalone
`avalanchers` — is avalanchego-side production work, not a harness task. The arm
is left correct and nightly-gated; it will pass once that gap closes. Recorded
per "stop and record the gap honestly rather than faking the assert."

## Out of scope (documented follow-ups)

- Symmetric mutual-validation topology (Approach C).
- Wiring `LiveMixedNet` bring-up into the `test-upgrade` / `test-load` live arms
  (they reuse the substrate but are separate plan tasks: M9.17 / M9.18).
- Nightly CI `$AVALANCHEGO_PATH` variable plumbing (already present in
  `nightly.yml`).
