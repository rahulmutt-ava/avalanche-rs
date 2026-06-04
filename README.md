# avalanche-rs

A from-scratch **Rust implementation of an Avalanche node** designed to be a
**drop-in replacement** for [`avalanchego`](https://github.com/ava-labs/avalanchego)
(the Go reference node): byte-for-byte wire/codec/block compatibility, identical CLI
flags and APIs, and the same consensus behavior. A Rust node must join Mainnet, Fuji,
and local networks and interoperate with Go nodes *indistinguishably*.

Two deliberate technology swaps from the Go node:

- **EVM execution** (C-Chain and EVM subnets) is rebuilt on [**reth**](https://github.com/paradigmxyz/reth) / `revm` instead of `coreth`/`libevm`.
- **Merkle state DB** uses [**Firewood**](https://github.com/ava-labs/firewood) as a direct Rust dependency (no CGO/FFI shim).

> **Status:** Specification phase. The [`specs-rust/`](specs-rust/) directory is a
> complete, standalone specification from which the implementation is derived. The
> Cargo workspace (`crates/…`) described below is the *target* layout, not yet built.

## Start here

Read [`specs-rust/00-overview-and-conventions.md`](specs-rust/00-overview-and-conventions.md)
first — it is the canonical reference (goals, compatibility surface, crate layout,
binding dependency choices, engineering conventions, the Go→Rust idiom mapping, and
§11 the **ratified decisions and open risks**). Every other spec conforms to it.
[`specs-rust/README.md`](specs-rust/README.md) is the annotated table of all 28 spec
documents in reading order.

## Compatibility surface (the contract)

The protocol surfaces that must remain byte-/behavior-exact with Go:

| Surface | Requirement |
|---|---|
| P2P wire protocol | Message IDs, framing, TLS handshake, peer gossip, ping/pong |
| Linear codec | Big-endian, length-prefixed, version-tagged |
| Block/Tx formats | P-, X-, C-Chain (incl. Atomic) and SAE blocks — hashes match |
| Merkle roots | `merkledb` roots (P/X) and Firewood-ethhash EVM state roots |
| JSON-RPC / Connect APIs | Method names, request/response shapes, error codes |
| CLI flags & config | Every flag, default, and precedence (flag > env > file) |
| Genesis | Identical genesis block IDs for Mainnet/Fuji |
| `rpcchainvm` gRPC plugin | A Rust VM hostable by a Go node and vice-versa |

The one carve-out from byte-exactness is **zstd** compression (R4): we assert a Go node
*accepts* our frames, not that compressed bytes are identical.

## Target crate layout

A single Cargo workspace, also driven by Bazel. Dependencies flow strictly downward
(`primitives → storage/codec/crypto → consensus/network → vms → node`). Crates are
prefixed `ava-`:

`ava-types` · `ava-codec` · `ava-crypto` · `ava-utils` · `ava-version` ·
`ava-database` · `ava-merkledb` · `ava-blockdb` · `ava-archivedb` · `ava-message` ·
`ava-network` · `ava-snow` · `ava-engine` · `ava-validators` · `ava-proposervm` ·
`ava-simplex` · `ava-vm` · `ava-vm-rpc` · `ava-secp256k1fx` · `ava-platformvm` ·
`ava-avm` · `ava-evm` · `ava-saevm` · `ava-chains` · `ava-api` · `ava-indexer` ·
`ava-wallet` · `ava-genesis` · `ava-config` · `ava-node` · `avalanchego` (the binary).

See [`00` §3](specs-rust/00-overview-and-conventions.md) for the full layout and
[`00` §4](specs-rust/00-overview-and-conventions.md) for the binding external-crate
table (tokio, tonic/prost, secp256k1, blst, rustls, rocksdb, firewood, reth, etc.).

## Build, test & tooling

Tooling mirrors the Go repo. **Cargo is the source of truth; Bazel consumes it.**

- **Task runner:** [`Task`](https://taskfile.dev) via `./scripts/run_task.sh <task>` (single entrypoint, wrapped in the Nix dev-shell when available). Tasks: `build`, `test-unit`, `lint`, `lint-saevm`, `generate-protobuf`, `bazel-build`, …
- **Toolchain:** pinned via `rust-toolchain.toml` (read by both the Nix flake and Bazel).
- **Build systems:** Cargo + Bazel (bzlmod + `rules_rust` + `crate_universe` + `gazelle_rust`).
- **Test runner:** [`cargo-nextest`](https://nexte.st) (`--profile ci`); coverage via `cargo llvm-cov`.
- **Lint/deps:** `clippy -D warnings`, `rustfmt`, `cargo-deny`, `cargo-audit`.

Details: [`01-development-environment.md`](specs-rust/01-development-environment.md).

### Testing strategy

A five-layer pyramid — unit → `proptest` property tests → golden/conformance vectors
→ integration → **differential**. The headline deliverable is the **differential
Go-vs-Rust harness** ([`tests/differential/`](specs-rust/02-testing-strategy.md)):
proptest generates a randomized program of actions (issue tx, API call, advance time,
restart, partition), replays it against both a Go and a Rust node under a controlled
clock, and asserts identical block IDs, state/merkle roots, API responses, and
validator sets. It runs in a cheap recorded-oracle mode per PR and a live two-binary
mode nightly. Plus fuzzing (`cargo-fuzz`), `criterion` benchmarks, and `loom` for
concurrency. See [`02-testing-strategy.md`](specs-rust/02-testing-strategy.md) and
[`22-test-vectors-and-oracle.md`](specs-rust/22-test-vectors-and-oracle.md).

## Implementation roadmap

Dependency-ordered milestones, each ending in an *automatable* exit criterion (see
[`16-implementation-roadmap.md`](specs-rust/16-implementation-roadmap.md)):

| | Milestone | Exit criterion |
|---|---|---|
| **M0** | Foundations | MT19937 RNG bit-for-bit parity with gonum (retires R1) |
| **M1** | Storage | RocksDB backends + Go-exact merkledb roots |
| **M2** | Networking handshake | Rust node TLS-handshakes a live Go node, stays connected |
| **M3** | Consensus + VM framework | Test VM finalizes in a simulated cluster; windower parity |
| **M4** | P-Chain read-only sync | Sync Fuji P-Chain to tip, block IDs/state match Go |
| **M5** | X-Chain issue/accept | 10k generated txs → identical block IDs + UTXO sets |
| **M6** | C-Chain on reth | Re-execute mainnet blocks; state roots match Go (closes reth gaps G0–G8) |
| **M7** | SAE VM | Streaming async pipeline deterministic; crash-recovery idempotent |
| **M8** | Node/config/API/wallet/genesis | Zero flag diff; API parity; Mainnet+Fuji genesis IDs |
| **M9** | Plugin interop + hardening | Bidirectional rpcchainvm; mixed Go+Rust network with no fork |

## Highest-priority risks

- **R1 — Deterministic RNG parity (highest).** The consensus sampler and proposervm windower use gonum's MT19937/MT19937-64; the Rust port must reproduce its stream bit-for-bit (golden-gated, retired in M0).
- **R2 — On-disk migration.** RocksDB replaces Go's Pebble/LevelDB; booting from a Go data dir needs an import tool.
- **R3 — reth API instability.** reth's library crates have no stable public API — pin a vendored revision and wrap every touch-point.

Full list with mitigations: [`00` §11.2](specs-rust/00-overview-and-conventions.md).

## License

See [`LICENSE`](LICENSE).
