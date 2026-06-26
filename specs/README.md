# specs — Rust port of avalanchego

This directory is a **complete, standalone specification** for a from-scratch Rust
implementation of an Avalanche node that is a **drop-in replacement** for
`avalanchego`: byte-exact wire/codec/block compatibility, identical CLI flags and
APIs, the EVM layer rebuilt on [**reth**](https://github.com/paradigmxyz/reth), and
the Merkle state DB on [**Firewood**](https://github.com/ava-labs/firewood) as a
direct Rust dependency. A coding agent should be able to derive the implementation
from these documents.

> **Upstream provenance.** These specs were generated from avalanchego commit
> `fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11` (2026-06-03). Upstream commits through
> `cbea62895c` (2026-06-25) have been reviewed and folded in as **"Upstream
> delta"** callouts in the affected files (`04`, `08`, `10`, `11`, `12`, `14`,
> `18`, `21`, `27`) and plan files (`plan/M4`, `plan/M7`, `plan/M8`). When re-syncing
> against newer avalanchego, start the review from `cbea62895c`.
>
> The `cc3b103b91 → 0b0b57143c` sync (reviewed 2026-06-15) folded three SAE
> commits — ACP-194 minimum-gas floor enforcement (`0b0b57143c`, #5424), SAE
> C-Chain `ParseBlock` extData-hash verification (`5896c92fee`, #5447), and the
> `gastime.New(baseFee)` refactor (`3a5cba4a61`, #5485) — into `11`/`21`/`10` and
> `plan/M7` (tasks M7.35–M7.37).
>
> The `0b0b57143c → 72adc639e6` sync (reviewed 2026-06-17) folded five commits.
> Spec-relevant: **ACP-236 (4)** standard execution + state persistence for
> auto-renewed validators (`55a1512be1`, #5203) → `08` §2.4 + `plan/M4.16`
> (Helicon-gated, dormant/non-gating); and three **SAE metrics** commits — SAE
> execution-pressure metrics (`553742045d`, #5500), `accepted_gas_limit_total`
> (`a1e5e4beb4`, #5534), and the `in_memory_blocks` gauge (`72adc639e6`, #5535) →
> `18` §2.11 + a knock-on `ExecutionResults.GasConsumed`/`sendPostExecutionEvents`
> note in `11`, with the prometheus registration tracked as the existing
> M7.33→M8 handoff. **Irrelevant:** the Claude PR-review prompt/CI tweak
> (`e074c4d7bc`, #5364) — no spec surface.
>
> The `72adc639e6 → 9b48abd852` sync (reviewed 2026-06-17) folded one commit:
> the **SAE C-Chain Warp/ICM package** (`9b48abd852`, #5523) — a dedicated
> `vms/saevm/cchain/warp` consolidating the outbound-capture / message-store /
> ACP-118 sign-decision / inbound-predicate-pass lifecycle for the asynchronous
> C-Chain → `11` §8 + `10` §8.2 upstream-deltas + `plan/M7` task **M7.38**
> (non-gating: Helicon unscheduled, SAE C-Chain Warp interop not yet exercised).
>
> The `9b48abd852 → b1393ecb06` sync (reviewed 2026-06-17) folded two SAE
> commits, both non-gating (Helicon unscheduled). (1) **C-Chain `ParseBlock`
> rejects a non-zero block `Version`** (`4772ab3c97`, #5543) — a sibling
> syntactic check to the M7.37 extData-hash verify → `11` §8 + `10` §9
> upstream-deltas + `plan/M7` task **M7.39** (flagged blocker: the Rust approach-(B)
> carrier has no `BlockBodyExtra.Version` field yet). (2) **`adaptor` syncable-VM
> wrapper** `ConvertStateSync` (`b1393ecb06`, #5480) — a second generic bridge
> turning a `SyncableVM[SP]` into Snowman's `StateSyncableVM` → `11` §5
> upstream-delta + `plan/M7` task **M7.40** (dormant: SAE state sync itself
> unported). No irrelevant commits in this range.
>
> The `b1393ecb06 → 84533ec5b1` sync (reviewed 2026-06-18) folded one SAE commit.
> Spec-relevant: **`VM.GetBlock` drops an unexpected error** (`84533ec5b1`, #5547)
> — Go was returning `(b, nil)` after only translating `ErrNotFound`, silently
> swallowing a corrupt/failed height-index read; fixed to `return b, err`, with a
> companion `RestoreSettledBlock` `%v`→`%w` wrap so the chain survives → `11` §4
> upstream-delta + `plan/M7` task **M7.42** (a correctness fix, **not** Helicon-gated:
> the Rust VM code exists and should mirror it). **Irrelevant:** the nix-26.05
> toolchain bump (`86602f460f`, #5551) — regenerated `*.pb.go` / contract-binding
> outputs and `flake.{nix,lock}`; no spec surface (avalanche-rs has its own
> `flake.nix`).
>
> The `84533ec5b1 → 331bcfb106` sync (reviewed 2026-06-19) folded two SAE
> C-Chain commits, both non-gating (Helicon unscheduled). (1) **Coreth-compatible
> genesis** (`ff8f0e5020`, #5536) — `cchain.Initialize` replaces the placeholder
> `core.Genesis` unmarshal with a dedicated `genesis.go` (`parseGenesis` rejecting
> testing-only fields + synthesizing `ChainConfig` from `ctx.NetworkUpgrades` with
> per-chainID Berlin/London activation-height pins; `setup` writing the genesis
> block/state + mismatch/compatibility guards) → `11` §8 upstream-delta + `plan/M7`
> task **M7.43**. (2) **Millisecond block timestamps** (`484daf4593`, #5524) —
> `BuildHeader` now fills the Granite-gated `TimeMilliseconds` from an injected
> clock's `UnixMilli` and `BlockTime` reads the sub-second component back while
> anchoring the seconds to `h.Time` → `11` §8 + `10` §header-tail upstream-deltas
> + `plan/M7` task **M7.44**. **Non-gating/test-only:** the stateful-RPC test
> expansion (`b8da248b24`, #5283) — `sae/rpc/server.go` change is doc-comment-only
> (enumerates the already-registered stateful RPCs), the rest is tests — and the
> flaky-bloom-inclusion-check removal (`331bcfb106`, #5562) — `cchain/gossip_test.go`
> only. No irrelevant commits in this range.
>
> The `331bcfb106 → dbf0f71dc1` sync (reviewed 2026-06-24) folded five SAE
> C-Chain commits. **Wire/header-format (Helicon-dormant, non-gating):**
> (1) **settled-block marker** (`dbf0f71dc1`, #5573) — the `hook.Settled` quad is
> now four optional coreth header fields (`SettledHeight`/`SettledGasUnix`/
> `SettledGasNumerator`/`SettledExcess`); `BuildBlock` writes, `SettledBy` reads →
> `11` §1.3 + `10` §header-tail upstream-deltas + `plan/M7` **M7.45**.
> (2) **`MinPriceExponent` header field** (`cec35390e0`, #5437) — the ACP-283
> dynamic-min-gas-price exponent (already specced as the M7.34 `PriceExponent`
> integrator) gets its wire home → `10` §header-tail + `21` §6.x upstream-deltas +
> `plan/M7` **M7.46**. (3) **pre-AP1 / genesis `ParseBlock`** (`08ae32b741`, #5565)
> — resolves the M7.37 `TODO`: genesis & pre-`ApricotPhase1` blocks expect an empty
> `ExtDataHash` (or a hardcoded mainnet/fuji corpus value) → `11` §8 + `10`
> §header-tail upstream-deltas + `plan/M7` **M7.47** (needed for full coreth
> retirement). **Live (not Helicon-gated):** (4) **deprecated `avax.getAtomicTxStatus`**
> (`03cdf8e97c`, #5564) → `10` §9.2 upstream-delta + `plan/M7` **M7.48**;
> (5) **libevm bump + `PostRPCMarshal` extras + `ShouldRefundGas`** (`2471172fe1`,
> #5572) — custom header/block fields now surface in eth-RPC JSON → `10` §9.1
> upstream-delta + `plan/M7` **M7.49** (flagged: verify reth already suppresses
> post-AP1 gas refunds). **Irrelevant:** `lint-all` Taskfile safety (`17ba2b3f0d`,
> #5552) and e2e API-ordering test cleanup (`d295aca97b`, #5566) — tooling/test-infra,
> no spec surface. **Non-gating (reference input):** `sync/code` to-fetch-marker
> cleanup fix (`bda4f299dd`, #5356) — a `graft/evm` state-sync bug fix; the Rust
> EVM is reth (not a coreth transliteration target), so this is a reference input,
> not a port obligation.
>
> The `dbf0f71dc1 → cbea62895c` sync (reviewed 2026-06-26) folded three SAE
> C-Chain commits, all **non-gating** (Helicon unscheduled). (1) **Warp
> integration** (`5e040de53e`, #5514) — wires the M7.38 `cchain/warp` package into
> the VM lifecycle (config-supplied off-chain messages → `warp.NewStorage`,
> ACP-118 cached handler on `acp118.HandlerID`, `AfterExecutingBlock` persists
> messages from receipts, `BuildBlock` runs `VerifyBlock` and writes predicate
> bytes into `header.Extra`) → `11` §8 upstream-delta + `plan/M7` **M7.50**
> (completes the M7.38 deferred wiring). (2) **`MinPriceExponent` consumed in the
> block lifecycle** (`f72fee1347`, #5441) — `GasConfigAfter` now returns
> `MinPrice = priceExponent(header).Price()` (was hardcoded `1`) and `BuildHeader`
> advances the child exponent via `Toward(desired)`; new
> `dynamic.InitialPriceExponent = 0` → `11` §8 + `21` §6.x upstream-deltas +
> `plan/M7` **M7.51** (consumes the M7.34 integrator / M7.46 wire home; no formula
> change). (3) **Operator node config** (`cbea62895c`, #5574) — `Initialize`
> decodes `configBytes` into a documented `config` (pruning / commit-interval /
> tx-pool slots / `min-price-target`, with `defaultConfig()`) → `11` §8 + `10` §8.3
> upstream-deltas + `plan/M7` **M7.52**. **Non-gating (doc-only, no spec surface):**
> the `accepted_blocks_slot` proposervm-histogram comment clarification
> (`865cc483d0`, #5579) — explains the half-step bucket bounds (0.5/1.5/2.5/+Inf)
> of a metric not catalogued in `18`; no code/wire/name change, recorded here for
> the trail. **Non-gating (reference input):** in-memory-HashDB removal from EVM
> reexecution (`2a93974d34`, #5487) — a `graft/coreth`+`graft/evm`+`graft/subnet-evm`
> refactor of state reconstruction; the Rust EVM is reth + Firewood-direct, so this
> is a reference input, not a port obligation.

## Read this first

**[`00-overview-and-conventions.md`](00-overview-and-conventions.md)** is the
canonical reference: goals, the compatibility surface, the Cargo workspace / crate
layout, the binding external-dependency table, the cross-cutting engineering
conventions, the Go→Rust idiom mapping, and (§11) the **ratified cross-spec
decisions and open risks**. Every other file conforms to it.

## Reading order

| # | File | Subsystem |
|---|------|-----------|
| 00 | [overview-and-conventions](00-overview-and-conventions.md) | goals, layout, deps, conventions, ratified decisions & risks |
| 01 | [development-environment](01-development-environment.md) | Nix flakes, Bazel (bzlmod + rules_rust + gazelle), task/test runners, AGENTS.md/CLAUDE.md |
| 02 | [testing-strategy](02-testing-strategy.md) | unit + proptest, TDD, golden vectors, fuzzing, the differential Go-vs-Rust harness |
| 03 | [core-primitives](03-core-primitives.md) | ids, the linear codec, crypto (secp256k1/BLS/staking certs), utils, version/upgrade |
| 04 | [storage-and-databases](04-storage-and-databases.md) | `Database` family, RocksDB/mem backends, merkledb, blockdb, archivedb, Firewood |
| 05 | [networking-p2p](05-networking-p2p.md) | wire protocol, messages, peers/handshake (TLS), router, throttling, NAT |
| 06 | [consensus](06-consensus.md) | Snowball/Snowman, engines, validators, proposervm, simplex |
| 07 | [vm-framework](07-vm-framework.md) | VM traits, rpcchainvm plugins, avax UTXO components, fx/secp256k1fx, chains manager |
| 08 | [platformvm-pchain](08-platformvm-pchain.md) | P-Chain: staking, subnets, L1s (ACP-77), validator state, warp |
| 09 | [avm-xchain](09-avm-xchain.md) | X-Chain: assets, UTXOs, nftfx/propertyfx, atomic import/export |
| 10 | [cchain-evm-reth](10-cchain-evm-reth.md) | C-Chain & EVM subnets on reth + Firewood-ethhash; atomic txs; warp/precompiles |
| 11 | [saevm](11-saevm.md) | SAE / ACP-194 streaming asynchronous execution; gas-as-time; the minimal C-Chain |
| 12 | [node-config-api-wallet](12-node-config-api-wallet.md) | node assembly, config/flags (drop-in parity), APIs, indexer, wallet, genesis |
| 13 | [config-flags-reference](13-config-flags-reference.md) | exhaustive verbatim catalog of every CLI/config flag — name, type, default, env var, Rust/clap type |
| 14 | [api-rpc-reference](14-api-rpc-reference.md) | exhaustive catalog of every exposed API/RPC endpoint with params/returns |
| 15 | [serialization-and-wire-formats](15-serialization-and-wire-formats.md) | authoritative catalog of all protobuf/gRPC packages, the linear codec, p2p framing/zstd, RLP, address/string encodings |
| 16 | [implementation-roadmap](16-implementation-roadmap.md) | dependency-ordered milestones M0–M9, each ending in an automatable differential/golden exit criterion; risk burndown |
| 17 | [runtime-architecture](17-runtime-architecture.md) | the tokio task/channel topology, backpressure, the cancellation tree, and exact shutdown ordering |
| 18 | [metrics-and-logging](18-metrics-and-logging.md) | verbatim Prometheus metric-name catalog (a parity surface) + the logging/tracing/OTel model |
| 19 | [state-sync-and-bootstrap](19-state-sync-and-bootstrap.md) | state-sync → bootstrap → consensus, the per-VM sync matrix, merkledb/EVM sync |
| 20 | [warp-icm](20-warp-icm.md) | Avalanche Warp / Interchain Messaging end-to-end: formats, signing, aggregation, verification, precompile |
| 21 | [fee-economics-math](21-fee-economics-math.md) | every fee/economics formula with worked integer vectors (consensus-critical) |
| 22 | [test-vectors-and-oracle](22-test-vectors-and-oracle.md) | the golden-vector corpus, its manifest, and the Go extraction harness that makes TDD executable |
| 23 | [genesis-construction](23-genesis-construction.md) | exact per-chain genesis byte/ID assembly; the expected genesis IDs per network |
| 24 | [determinism-and-clock](24-determinism-and-clock.md) | the determinism audit checklist (PR gate) + the injectable clock / virtual-time abstraction |
| 25 | [key-management-and-signing](25-key-management-and-signing.md) | staking-TLS & BLS key lifecycle; the local/remote signer abstraction |
| 26 | [versioning-and-compatibility](26-versioning-and-compatibility.md) | the version taxonomy + handshake compatibility matrix; the wire version string to report |
| 27 | [crash-consistency-and-recovery](27-crash-consistency-and-recovery.md) | atomic-commit invariant, crash-point→recovery matrix, per-VM recovery, crash-injection tests |

> Files 13–15 are reference catalogs (config, APIs, serialization). Files 16–17 are
> cross-cutting wiring (build order, runtime topology). Files 18–21 consolidate
> diffuse surfaces (metrics, sync, warp, fee math). Files 22–24 make the build
> executable & correct (test oracle, genesis, determinism). Files 25–27 close
> operational completeness (keys/signing, versioning, crash recovery).

## Conventions used by every spec

Each file: Go source paths covered → public Rust API (traits/types) → invariants &
protocol constants → Go→Rust mapping → external crates (consistent with `00` §4) →
test plan (per `02`) → "Performance notes / improvements over Go". Cross-references
use the filename.

## Highest-priority risks before implementation

See `00` §11.2. In short: **R1** — reproduce gonum's MT19937/MT19937-64 stream
bit-for-bit for the consensus sampler and proposervm windower (golden-gated);
**R3** — reth's library API is unstable (pin a vendored revision, wrap every
touch-point); **R2** — migrating from a Go Pebble/LevelDB data dir needs an import
tool.
