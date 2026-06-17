# CLAUDE.md

Guidance for Claude Code working in the **avalanche-rs** repository — the Rust
implementation of an Avalanche node (a drop-in replacement for `avalanchego`).
Read this before making changes; it captures how to build, test, lint, and
conform to conventions so your work passes CI on the first try.

> The canonical, tool-neutral version of this guidance is `AGENTS.md`. This file
> is identical in substance with Claude-Code-specific notes. When they diverge,
> `AGENTS.md` + `specs/00-overview-and-conventions.md` win.

## Repo at a glance

- **Module:** a single Cargo workspace; `ava-*` crates under `crates/`; the binary
  is `avalanchers`.
- **Rust version:** pinned exactly in `rust-toolchain.toml`. CGO-equivalent FFI
  (rocksdb, firewood, blst, secp256k1) needs `clang`/`libclang` — provided by the
  Nix dev shell.
- **Multi-tool build:** Cargo (inner loop), Bazel (bzlmod + rules_rust +
  crate_universe + gazelle_rust, hermetic/CI), Nix (pinned toolchain), Task
  (runner). Cargo is the source of truth; `crate_universe` consumes `Cargo.lock`.
- **EVM = reth; state DB = Firewood (direct dep).** The grafted Go EVM forks
  (`coreth`/`evm`/`subnet-evm`) are *reference inputs*, not transliteration
  targets.

### Active stricter area: `crates/ava-saevm*`

SAE (Streaming Asynchronous Execution, ACP-194). No API-stability guarantees.
Dedicated stricter lint pass `lint-saevm` (clippy pedantic + `arithmetic_side_effects`
+ `cast_*` deny, overflow checks). Use `checked_*`/`saturating_*`, never raw casts.
See `specs/11-saevm.md` and `00` §7.7.

## Directory map (crates under `crates/`)

| Crate | Purpose |
|-------|---------|
| `ava-types` | ids, fixed byte arrays, primitive newtypes, errors |
| `ava-codec` (+ `ava-codec-derive`) | hand-written linear codec (byte-exact) |
| `ava-crypto` | secp256k1, BLS (blst), hashing, TLS/staking certs |
| `ava-utils` | set, bag, sampler, math, timers, windows |
| `ava-version` | versions + network upgrade schedule |
| `ava-database`, `ava-merkledb`, `ava-blockdb`, `ava-archivedb` | storage (rocksdb, Firewood) |
| `ava-message`, `ava-network` | P2P wire + networking |
| `ava-snow`, `ava-engine`, `ava-validators`, `ava-proposervm`, `ava-simplex` | consensus |
| `ava-vm`, `ava-vm-rpc`, `ava-secp256k1fx` | VM framework + rpcchainvm plugin |
| `ava-platformvm`, `ava-avm`, `ava-evm`, `ava-saevm*` | the VMs |
| `ava-chains`, `ava-api`, `ava-indexer`, `ava-wallet`, `ava-genesis`, `ava-config` | node services |
| `ava-node`, `avalanchers` | node assembly + binary |

## Running tasks

```sh
./scripts/run_task.sh <task>     # primary entrypoint (wraps Nix dev shell)
./scripts/run_task.sh --list     # list tasks
```

CI always calls a task, never `cargo`/`bazel` directly.

## Build / Test / Lint

```sh
./scripts/run_task.sh build               # release binary
./scripts/run_task.sh test-unit           # nextest --all-features --profile ci + doctests
./scripts/run_task.sh test-unit-fast      # fast local nextest
./scripts/run_task.sh lint                # clippy -D warnings + rustfmt + license + TOML
./scripts/run_task.sh lint-fix            # auto-fix
./scripts/run_task.sh lint-saevm          # stricter SAE pass
./scripts/run_task.sh lint-all            # everything lint-all-ci runs
```

Single test: `cargo nextest run -p <crate> -E 'test(Name)'`.

### Live Go oracle (`avalanchego` binary, for differential/interop tests)

A buildable Go `avalanchego` checkout lives at **`~/avalanchego`** with a built
binary at **`~/avalanchego/build/avalanchego`** (`avalanchego/1.14.2`,
`rpcchainvm=45`). This **unblocks the live-Go differential/interop tests** that
need a real Go node or plugin host — the M9 interop tasks (`differential::plugin_rust_in_go`,
`plugin_go_in_rust`, `mixed_network`, `test-upgrade`, `test-reexecute`,
version/compat matrix). Build / refresh:

```sh
cd ~/avalanchego && ./scripts/build.sh   # CGO + firewood ffi v0.6.0; ld macOS-version warnings are harmless
~/avalanchego/build/avalanchego --version
```

Point tmpnet-based live tests at it via `AVALANCHEGO_PATH`. `~/avalanchego` is
also where the env-gated in-repo Go-oracle emitters are copied (the recorded-oracle
pattern from M5–M8; `specs/02` §9/§11). Its HEAD commit is the upstream oracle
pin — re-run `build.sh` after pulling and re-verify `rpcchainvm=45` + the firewood
ffi tag before trusting live roots (`specs/04` §4.2 upstream-delta).

> **MANDATORY pre-gate check — the binary must match the checkout.** A `git pull`
> in `~/avalanchego` advances HEAD without rebuilding, so the binary's *embedded*
> commit silently drifts from the source and every live/oracle gate then compares
> Rust against the **wrong** Go source. Before any live differential /
> recorded-oracle / `test-live` gate, run:
> ```sh
> ./scripts/check_oracle_binary.sh   # exit 1 if binary commit != ~/avalanchego HEAD; also asserts rpcchainvm=45
> ```
> On FAIL, rebuild (`cd ~/avalanchego && ./scripts/build.sh`) and re-run until it
> prints `OK`. (It WARNs, non-fatal, when HEAD differs from the vectors corpus pin
> `tests/vectors/manifest.json:avalanchego_revision` — that affects only
> `vectors-drift` re-extraction, not live consensus roots.)

## Code generation (commit nothing generated by default)

| What | Command |
|------|---------|
| Protobuf | `./scripts/run_task.sh generate-protobuf` (build.rs tonic/prost; buf lint+breaking) |
| Mocks | `./scripts/run_task.sh generate-mocks` (mockall macros; compile-check only) |
| Deps | `./scripts/run_task.sh deps-tidy` (cargo update --locked + cargo deny + bazelisk mod tidy) |
| Bazel BUILD | `./scripts/run_task.sh bazel-gazelle-generate` (commit results) |

## Before you push — pass CI

1. `./scripts/run_task.sh lint-all`
2. `./scripts/run_task.sh test-unit`
3. Touched `proto/`? `check-generate-protobuf`.
4. Changed deps? `deps-tidy`; commit `Cargo.lock` + `MODULE.bazel.lock`.
5. Touched `.rs` affecting Bazel? `bazel-check-metadata`; commit `BUILD.bazel`.
6. **Never** raw-cast or do unchecked `Duration` math on a `Tau` quantity — the
   `taulint` gate fails on it. Use `params::TAU` + `checked_*`.

## Rust coding conventions (enforced)

- **License header** on every `.rs`:
  ```rust
  // Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
  // See the file LICENSE for licensing terms.
  ```
- **4-space** indent for `.rs` (`.editorconfig`), LF endings, final newline.
- **Import grouping** std → external → crate (`group_imports = "StdExternalCrate"`).
- **Errors:** `thiserror` per-crate enum + `Result<T>`; `anyhow` only in the
  binary/tests. Sentinel errors → variants; assert via `assert_matches!`.
- **No `unwrap()`/`expect()`/`dbg!`/`todo!`** in library code (clippy denies).
- **`#![forbid(unsafe_code)]`** except FFI wrappers with `// SAFETY:` + tests.
- Lints on: `clippy::all` (deny), `unwrap_used` (deny), `arithmetic_side_effects`
  (warn; deny in SAE), `missing_docs` (warn on libs), `unused_crate_dependencies`.

### Forbidden patterns

- No direct `HashMap` serialization in consensus/codec — sort keys / `BTreeMap`.
- No floating-point in codec/consensus paths.
- No second crate for a job covered by `00` §4 (`cargo deny` enforces;
  `mockall` only; `rustls` over `native-tls`).
- No raw `as` casts in `ava-saevm*`.

## Testing conventions

- **`cargo-nextest`** is the runner; doctests via `cargo test --doc`.
- Use **`proptest`** for property tests; **`mockall`** for narrow local mocks
  (`#[cfg_attr(test, automock)]`).
- Table tests via arrays/`rstest`; assertions via `assert_matches!` /
  `pretty_assertions`.
- Differential tests against the Go node guard protocol parity — see
  `specs/02-testing-strategy.md`.

## Key files

`Cargo.toml` · `rust-toolchain.toml` · `Taskfile.yml` · `scripts/run_task.sh` ·
`flake.nix` · `MODULE.bazel` · `.bazelrc` · `rustfmt.toml` · `clippy.toml` ·
`deny.toml` · `.config/nextest.toml` · `.github/workflows/ci.yml` ·
`specs/00-overview-and-conventions.md`
