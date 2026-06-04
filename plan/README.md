# avalanche-rs — Implementation Plan (Milestone Backlog)

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`
> (recommended) or `superpowers:executing-plans` to implement these plans task-by-task.
> Tasks use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build, in dependency order, a from-scratch Rust node that is a byte-/behavior-exact
drop-in replacement for `avalanchego`, per the specification in [`specs/`](../specs/).

**Architecture:** A single Cargo workspace of `ava-*` crates (plus the `avalanchers` binary),
layered in strict tiers T0→T5 with a continuous cross-cutting tier X. EVM execution is rebuilt
on `reth`/`revm`; merkle state on `firewood`. Each milestone exits on named differential/golden
tests and leaves the `avalanchers` binary buildable and green.

**Tech stack:** Rust (stable, pinned), `tokio`, `tonic`/`prost`, `secp256k1`, `blst`, `rustls`,
`rust-rocksdb`, `firewood`, `reth`/`revm`, `proptest`, `cargo-nextest`, Bazel (bzlmod + rules_rust).

---

## 0. How to read this plan

This directory decomposes the roadmap ([`specs/16-implementation-roadmap.md`](../specs/16-implementation-roadmap.md))
into **PR-sized tasks**. One file per milestone, in build order:

| Plan file | Milestone | Owning specs | Exit gate (headline) |
|---|---|---|---|
| [`M0-foundations.md`](M0-foundations.md) | M0 — Foundations | 03, 15§4, 21, 24, 25 | MT19937 RNG bit-for-bit vs gonum (retires **R1**) |
| [`M1-storage.md`](M1-storage.md) | M1 — Storage | 04, 15, 19, 27 | RocksDB backends + Go-exact merkledb roots (retires **R2** scoping, part **R3**) |
| [`M2-networking.md`](M2-networking.md) | M2 — Networking handshake | 05, 15§3, 17, 18, 26 | Rust node TLS-handshakes a live Go Fuji node, stays connected (retires **R4**, **R5** part) |
| [`M3-consensus-vm-framework.md`](M3-consensus-vm-framework.md) | M3 — Consensus + VM framework | 06, 07, 24 | Test VM finalizes in a simulated cluster; windower parity (confirms **R1**) |
| [`M4-pchain.md`](M4-pchain.md) | M4 — P-Chain read-only sync | 08, 19, 20, 21, 23 | Sync Fuji P-Chain to tip; block IDs/state/validators == Go |
| [`M5-xchain.md`](M5-xchain.md) | M5 — X-Chain issue/accept | 09, 07 (ATOMIC-1) | 10k generated txs → identical block IDs + UTXO sets |
| [`M6-cchain.md`](M6-cchain.md) | M6 — C-Chain on reth | 10, 04§4, 20, 21 | Reexecute mainnet C-Chain range → state roots == Go (retires **R3**) |
| [`M7-saevm.md`](M7-saevm.md) | M7 — SAE VM | 11, 21, 27 | SAE async pipeline deterministic; crash-recovery idempotent |
| [`M8-node-config-api.md`](M8-node-config-api.md) | M8 — Node / config / API / wallet / genesis | 12, 13, 14, 23, 17, 18 | Zero flag diff; API parity; Mainnet+Fuji genesis IDs |
| [`M9-interop-hardening.md`](M9-interop-hardening.md) | M9 — Plugin interop + hardening | 07§5, 02§10–11, 26 | Bidirectional rpcchainvm; mixed Go+Rust net, no fork (**R-final**) |
| [`X-cross-cutting.md`](X-cross-cutting.md) | X — Continuous workstreams | 01, 02, 18, 22, 24 | Differential harness, golden extraction, CI, metrics/error/obs parity |

Read [`specs/00-overview-and-conventions.md`](../specs/00-overview-and-conventions.md)
and [`specs/02-testing-strategy.md`](../specs/02-testing-strategy.md) **before any task** —
they are the canonical conventions every task inherits. The owning spec(s) named in each task are
the code-level source of truth; tasks reference spec sections rather than duplicating them.

---

## 1. Crate build-order DAG (tiers)

A tier may only depend on tiers above it. Within a tier, crates are independent and parallelizable.

```
            T0 primitives ── R1 retired here
           /      |       \
      T1 storage  T2a wire  (crypto/codec feed all)
           \      |       /
            T2b consensus core ── R1 (windower) confirmed
                  |
            T3 VM framework
                  |
   ┌──────────────┼───────────────┐
 T4 P-Chain   T4 X-Chain   T4 C-Chain ── T4 SAE
   └──────────────┼───────────────┘
                  |
            T5 node/config/api/wallet/genesis
                  |
            interop + hardening
   (X: ava-differential + CI run alongside every tier)
```

| Tier | Crates | Milestone(s) |
|---|---|---|
| **T0 — Primitives** | `ava-types`, `ava-codec` (+`-derive`), `ava-crypto`, `ava-utils`, `ava-version` | M0 |
| **T1 — Storage** | `ava-database`, `ava-merkledb`, `ava-blockdb`, `ava-archivedb` (+`firewood`) | M1 |
| **T2a — Wire** | `ava-message`, `ava-network` | M2 |
| **T2b — Consensus** | `ava-snow`, `ava-engine`, `ava-validators`, `ava-proposervm`, `ava-simplex` | M3 |
| **T3 — VM framework** | `ava-vm`, `ava-vm-rpc`, `ava-secp256k1fx`, `ava-chains` | M3 |
| **T4 — VMs** | `ava-platformvm`, `ava-avm`, `ava-evm`, `ava-saevm` | M4–M7 |
| **T5 — Node/APIs** | `ava-api`, `ava-indexer`, `ava-wallet`, `ava-genesis`, `ava-config`, `ava-node`, `avalanchers` (bin) | M8 |
| **X — Cross-cutting** | `ava-differential`, `tools/extract-vectors`, CI, metrics/error/obs | M0→M9 (continuous) |

**Introduced internal sub-crates.** Beyond the 32 canonical crates above, the milestone plans
factor in a few internal helpers (consistent with `00` §3, which mandates the SAE sub-workspace
and the reth-touch-point wrapping):
- `ava-saevm-{core,exec,cchain,blocks,saedb,gastime,gasprice,proxytime,hook,adaptor,txgossip,intmath,cmputils,types,params,worstcase}` — the SAE sub-workspace (M7), mirroring Go `vms/saevm/*`.
- `ava-evm-reth` — the reth/revm **facade** sub-crate (M6); the *only* crate allowed to name `reth_*`/`revm` directly (the R3 mitigation, `00` §11.1.6).
- `ava-warp` — shared Warp/ICM crate (spec 20) consumed by both `ava-platformvm` (M4) and `ava-evm` (M6).
- `ava-logging` (tracing/log routing) and `ava-testvectors` / `ava-differential` (test infra, tier X).

**Cross-milestone parallelism.** Once M3 lands, **M5 (X), M6 (C)** may run in parallel with each
other; M4 (P) is sequenced first because it serves `ValidatorState` to every chain's consensus.
M6 has no dependency on M5 and may be pulled ahead if EVM compatibility is prioritized. M7 (SAE)
depends on M6 (shares the revm executor + Firewood layout, per `00` §11.1.5). Tier X runs
continuously from M0.

---

## 2. Conventions every task follows

**Task IDs.** `M<n>.<k>` (e.g. `M0.3`). Cross-milestone dependencies use the full ID
(e.g. a M3 task may `depend on: M1.4, M2.2`). Cross-cutting tasks are `X.<k>`.

**Dependency notation.** Each task header carries `**Depends on:** <ids | none>`. Tasks with no
unmet dependency in the same milestone form a **parallel group** and may be dispatched concurrently
to subagents. Each milestone file opens with a dependency DAG / parallel-wave table.

**TDD red→green (mandatory, `02` §2).** Every task lists its **first failing test** and the exact
command to confirm it fails for the right reason, then the minimal implementation, then the command
to confirm green, then a commit. Test names follow `02`: `golden::*`, `prop::*`, `conformance::*`,
`differential::*`, plus in-module unit tests.

**The buildable-&-green invariant (project requirement).** *At the end of every milestone* the
workspace MUST satisfy, with no exceptions deferred:

```
cargo build --workspace                 # whole workspace compiles
cargo build -p avalanchers              # the binary links
cargo nextest run --profile ci          # all tests green (incl. this milestone's exit tests)
cargo clippy --workspace -- -D warnings # lint clean (SAE crates add clippy::pedantic)
```

The `avalanchers` binary is created as a skeleton in M0 and grows each milestone (it must always
compile and respond to `--version`/`--help`; chains/APIs are wired in as their crates land). The
**last task of every milestone** is an explicit "Milestone exit gate" task that runs the four
commands above plus the milestone's named exit tests, and updates each touched crate's
`tests/PORTING.md`.

**Per-crate contracts (`02` §13).** Every crate ships: a `proptest` suite with a committed
`proptest-regressions/` corpus; golden vectors under `tests/vectors/<surface>/` for any protocol
surface; a `tests/PORTING.md` matrix; a `cargo-fuzz` target if it contains a parser/decoder.

**Determinism (`00` §6.1, `24`).** No `HashMap` on serialization paths (sort like Go); checked
arithmetic, never silent wrap; floats forbidden in consensus/codec; injected `Clock` in tests
(virtual time, no wall-clock sleeps).

**Files & headers.** Exact paths in every task. Every `.rs` file carries the Ava Labs license
header (`00` §8). `#![forbid(unsafe_code)]` except behind audited binding wrappers.

---

## 3. Risk burndown (`00` §11.2)

| Risk | Retired by | Proof |
|---|---|---|
| **R1** — gonum MT19937/-64 RNG parity (HIGHEST) | M0 (sampler), confirmed M3 (windower) | `golden::sampler_mt19937_stream`; `golden::windower_schedule` |
| **R2** — Go Pebble/LevelDB → RocksDB migration | M1 (scoped), exercised M9 | M1 import-tool spec; `test-upgrade` Go-dir import |
| **R3** — reth library API instability | M1 (Firewood link), M6 (wrappers, G0–G8) | `differential::cchain_state_root` |
| **R4** — zstd not byte-identical | M2 | `differential::interop_handshake` (accept-not-byte-equal) |
| **R5** — Connect endpoint enumeration | M2 (network svcs), M8 (full API) | `differential::api_parity` |

---

## 4. Definition of done (M9 exit, `16` §5)

The port is accepted as a drop-in replacement only when **all** are simultaneously green: joins
Mainnet & Fuji and tracks tip without forking; indistinguishable in a mixed Go+Rust network; full
`differential::*` suite passes (incl. reexecute); zero flag diff; API parity; exact Mainnet/Fuji
genesis IDs; bidirectional rpcchainvm (v45); upgrade continuity incl. Go-dir import; perf gates hold.
