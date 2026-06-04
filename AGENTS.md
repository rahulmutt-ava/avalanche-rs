# AGENTS.md

Guidance for AI coding agents working in **avalanche-rs** — a from-scratch Rust
implementation of an Avalanche node, a drop-in replacement for `avalanchego`.
Read this before making changes; it captures how to build, test, lint, and
conform to repo conventions so your work passes CI on the first try.

## Repo at a glance

- **Workspace:** a single Cargo workspace; crates live under `crates/`, all named
  `ava-*` (the binary is `avalanchego` for drop-in invocation).
- **Rust version:** pinned exactly in `rust-toolchain.toml` (e.g. `1.90.0`). Bump
  it in lock-step with `MODULE.bazel` and the CI matrix; `check-rust-version`
  asserts they agree.
- **Goal:** byte-for-byte wire/codec/API/genesis compatibility with `avalanchego`.
  EVM execution is built on **reth**; the Merkle state DB is **Firewood** (direct
  Rust dep, no FFI shim).
- **Build tooling:** [Task](https://taskfile.dev) (`Taskfile.yml`) is the
  canonical runner; Nix (`flake.nix`) pins the toolchain; Bazel (`bazelisk`,
  bzlmod + rules_rust + crate_universe + gazelle_rust) is the hermetic path.
- **Test runner:** `cargo-nextest` (doctests via `cargo test --doc`).

### Stricter bar for SAE (`crates/ava-saevm*`)

`ava-saevm*` implements **SAE (Streaming Asynchronous Execution, ACP-194)**.
No API-stability guarantees. It has a **dedicated stricter lint pass**
(`lint-saevm`: clippy pedantic + `arithmetic_side_effects` + `cast_*` deny,
overflow checks everywhere) — the analogue of avalanchego's `gosec G115`. Hold
this code to a higher bar; use `checked_*`/`saturating_*`, never raw casts.

## Running tasks

Everything goes through the Task runner. You do **not** need `task` installed —
use the bootstrap wrapper (it uses `task` from PATH, else `go tool`, and wraps
tasks in the Nix dev shell when Nix is present):

```sh
./scripts/run_task.sh <task-name>     # primary entrypoint
./scripts/run_task.sh --list          # list tasks
```

CI does **not** call `cargo`/`bazel` directly — it always goes through tasks.

## Build

```sh
./scripts/run_task.sh build               # cargo build -p avalanchego --release
./scripts/run_task.sh build-debug-checks  # overflow + debug assertions
./scripts/run_task.sh bazel-build         # hermetic Bazel build
```

## Test

```sh
./scripts/run_task.sh test-unit       # nextest --all-features --profile ci + doctests
./scripts/run_task.sh test-unit-fast  # nextest, no all-features/checks (fast)
./scripts/run_task.sh test-coverage   # cargo llvm-cov
```

Single crate / test:

```sh
cargo nextest run -p ava-codec
cargo nextest run -p ava-snow -E 'test(TestName)'
```

## Lint & format

```sh
./scripts/run_task.sh lint          # clippy -D warnings + rustfmt --check + TOML + license
./scripts/run_task.sh lint-fix      # clippy --fix + cargo fmt + taplo fmt + add headers
./scripts/run_task.sh lint-saevm    # stricter pedantic/overflow pass on ava-saevm*
./scripts/run_task.sh lint-all      # everything CI's lint-all-ci runs
```

`rustfmt` (config in `rustfmt.toml`) handles all formatting — there is no separate
fmt-only gate beyond `cargo fmt --check`.

## Code generation

Rust prefers build-time/macro generation; we commit **no** generated code.

| What | Command | Notes |
|------|---------|-------|
| Protobuf/gRPC | `./scripts/run_task.sh generate-protobuf` | `build.rs` via tonic/prost; runs `buf lint`+`buf breaking` |
| Mocks | `./scripts/run_task.sh generate-mocks` | `mockall` is macro-generated; this just checks it compiles |
| Deps tidy | `./scripts/run_task.sh deps-tidy` | `cargo update --locked` + `cargo deny check` + `bazelisk mod tidy` |
| Bazel BUILD | `./scripts/run_task.sh bazel-gazelle-generate` | `gazelle_rust`; commit the result |

## Before you push — pass CI

1. `./scripts/run_task.sh lint-all` (clippy, rustfmt, license, SAE, shell,
   actionlint, yaml, deps, rust-version, bazel-metadata).
2. `./scripts/run_task.sh test-unit`.
3. If you touched `proto/`: `./scripts/run_task.sh check-generate-protobuf`.
4. If you changed dependencies: `./scripts/run_task.sh deps-tidy` and commit
   `Cargo.lock` + `MODULE.bazel.lock`.
5. If you touched `.rs` files affecting Bazel: `./scripts/run_task.sh
   bazel-check-metadata` and commit regenerated `BUILD.bazel`.
6. **Never** apply a raw `as` cast or unchecked `Duration` math to a `Tau`
   quantity — the `taulint` CI gate (`scripts/tau_lint.sh`) fails on it. Use
   `params::TAU` (a typed `Duration`) and `checked_*`.

## Conventions (see `specs-rust/00-overview-and-conventions.md` for the full set)

- **Edition 2021**, tabs are *not* used for Rust (`.editorconfig` → 4 spaces).
- **License header** on every `.rs` file:
  ```rust
  // Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
  // See the file LICENSE for licensing terms.
  ```
- **Errors:** per-crate `thiserror` `Error` enum + `pub type Result<T>`; `anyhow`
  only in the `avalanchego` binary and tests. Preserve Go sentinel errors as
  variants; assert with `assert_matches!` (mirrors Go's `ErrorIs` rule).
- **Imports:** grouped std → external → crate (`group_imports = "StdExternalCrate"`).
- **No `unwrap()`/`expect()`** in non-test library code (clippy denies it) except
  with a documented proven invariant.
- **`#![forbid(unsafe_code)]`** by default; opt out only in FFI wrapper modules
  (blst, firewood, rocksdb, secp256k1) with a `// SAFETY:` rationale.
- **Determinism:** never serialize a `HashMap` directly — sort keys / use
  `BTreeMap` exactly where Go sorts. Checked arithmetic, no float in
  consensus/codec paths.
- **Tests:** `cargo-nextest`; table tests via arrays + `for`/`rstest`; property
  tests via `proptest`; mocks via `mockall` (narrow, local, `#[cfg_attr(test, automock)]`).

## Forbidden patterns

- No `unwrap()`/`expect()`/`dbg!`/`todo!` in library code (clippy-denied).
- No direct `HashMap` serialization in consensus/codec paths.
- No second crate for a job already covered by `00` §4 (`cargo deny` bans
  alternatives; `mockall` is the only mock lib; `rustls` over `native-tls`).
- No raw `as` casts in `ava-saevm*` (use `checked_*`/`try_into()`).
- No floating-point in codec/consensus.

## Key files

`Cargo.toml` · `rust-toolchain.toml` · `Taskfile.yml` · `scripts/run_task.sh` ·
`flake.nix` · `MODULE.bazel` · `.bazelrc` · `rustfmt.toml` · `clippy.toml` ·
`deny.toml` · `.config/nextest.toml` · `.github/workflows/ci.yml` ·
`specs-rust/00-overview-and-conventions.md`
