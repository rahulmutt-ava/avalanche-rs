# X — Cross-Cutting Continuous Workstreams Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax. These workstreams run CONTINUOUSLY alongside M0–M9, not as a single sequential block.

**Goal:** Stand up and continuously deepen the eight cross-cutting workstreams — the deeper dev-env (Nix/Bazel), the `ava-differential` harness + golden-vector extraction, the metrics/error/observability parity gates, the determinism-audit PR gate, and the fuzzing/coverage/CI machinery — that together ENFORCE the buildable-&-green drop-in invariant at every milestone's exit.
**Tier:** X — Cross-cutting (continuous)
**Crates/areas:** `ava-differential` (`tests/differential/`), `tools/extract-vectors/` (= `tools/vectorgen/`), `xtask/`, `ava-testvectors`, CI (`.github/workflows/`), `flake.nix`, `MODULE.bazel`/`BUILD.bazel`/`.bazelrc`, `deny.toml`, `.config/nextest.toml`, per-crate `thiserror` error enums, `ava-logging`/`ava-api` metrics + tracing
**Owning specs:** 01 (dev-env), 02 (testing strategy), 18 (metrics/logging/OTel), 22 (vectors & oracle), 24 (determinism & clock)
**Depends on:** M0 workspace bootstrap (root `Cargo.toml`, `rust-toolchain.toml`, skeleton `avalanchers` bin, minimal `.config/nextest.toml`); deepens every milestone M0→M9
**Gate it enforces:** the buildable-&-green invariant (`cargo build --workspace`, `cargo build -p avalanchers`, `cargo nextest run --profile ci`, `cargo clippy --workspace -- -D warnings`) PLUS per-PR recorded-oracle differential + reexecute + fuzz-smoke + vectors-drift + dirty-tree + coverage floors, and a nightly job for live two-binary + mixed-net + upgrade + bench-guard.

---

## When each workstream lands (milestone cadence)

| # | Workstream (16 §4) | First lands | Deepens at | Gate it enforces |
|---|---|---|---|---|
| 1 | **Differential harness** (`ava-differential`, `tests/differential/`) | **M0** (recorded-oracle mode; reexecute on codec/sampler/genesis goldens) | **M2** (two-binary live + `p2p-framing`), M3 (vm-rpc `Observation`), M4 (P-Chain blocks/mempool), M5 (X-Chain), M6 (EVM state roots), M7 (SAE), M8 (API/validator views) | per-PR recorded-oracle + reexecute; nightly live two-binary + mixed-net; every subsystem adds an `Observation` collector |
| 2 | **Golden-vector extraction** (`tools/extract-vectors` = `tools/vectorgen/`) | **M0** (`codec`, `crypto-secp`, `crypto-bls`, `sampler-rng`, `address`, `genesis-ids`) | M1 (`merkle-roots`, `evm-state-roots` link), M2 (`p2p-framing`, `proto-wire`), M4 (`blocks-txs-pchain`, `fee-math`), M5 (`blocks-txs-xchain`, `warp`), M6 (`evm-state-roots`), M7 (saevm vectors) | `vectors-drift` re-extracts vs pinned Go commit and fails on diff; `vectors-verify` hashes/schema/orphans |
| 3 | **Metrics-name parity** (`ava-api`, `ava-logging`, per-subsystem registrars) | **M2** (network/handler families) | M3 (rpcchainvm/grpc), M4–M6 (per-VM `avalanche_<vm>_*`), M8 (full `/ext/metrics` snapshot, process collector) | golden `metrics_names` `insta` snapshot grows per milestone; Rust must be a superset of every Go family |
| 4 | **Error taxonomy** (per-crate `thiserror` enums) | **M0** (`ava-types`, `ava-codec`, `ava-utils`) | every milestone (new crate ⇒ new enum + sentinels) | `assert_matches!` tests mirror Go `errors.Is`; xtask lint forbids ad-hoc string errors on protocol paths |
| 5 | **Observability** (tracing/OTel) | **M0** (`ava-logging` span/level model) | M2 (network spans), M4–M7 (VM spans), **M8** (OTLP exporter wired) | span names mirror Go log messages; JSON-line shape golden; OTel resource attrs parity |
| 6 | **Bazel/Nix CI** | **M0** (`flake.nix`, `MODULE.bazel`, nextest `--profile ci`, cargo-deny, gazelle, dirty-tree gates) | every milestone (new crate ⇒ gazelle BUILD); M5+ TSan/loom jobs | dirty-tree on `Cargo.lock`/`MODULE.bazel.lock`/`BUILD.bazel`; `bazel build/test //...`; TSan/loom substitute for Go `-race` |
| 7 | **Fuzzing corpora** (cargo-fuzz per parser) | **M0** (`ava-codec`) | M1 (`ava-merkledb`), M2 (`ava-message`), M4–M7 (P/X/C/SAE block parsers) | `test-fuzz` smoke per PR; crash artifacts committed as seeds |
| 8 | **PORTING.md matrices** (`cargo xtask porting-report`) | **M0** (seed per crate from `go test -list`) | every milestone (matrix updated; subsystem "done" only when no `wip` rows) | aggregated report published in CI; a `wip` row blocks "subsystem done" |

> **Buildable-&-green invariant.** Task X.1 defines the CI gate that EVERY milestone's exit must pass. Each later task plugs an additional check into that same `tests-required` aggregator so the gate strictly grows.

---

## Tasks

### Task X.1: The buildable-&-green CI gate (`tests-required` aggregator + base build/lint/test jobs)
**Workstream:** 6 (Bazel/Nix CI)  ·  **First lands:** M0  ·  **Depends on:** M0 bootstrap (`Cargo.toml`, `rust-toolchain.toml`, skeleton `avalanchers`)  ·  **Spec:** 01 §10, 02 §1
**Files:** `.github/workflows/ci.yml`, `.github/actions/install-nix/action.yml`, `scripts/run_task.sh` (carried from M0/Go), `scripts/nix_run.sh`, `Taskfile.yml` (extend with `build`, `test-unit`, `lint`, `lint-all-ci`)
- [ ] **Step 1 — Red/first check:** Add a `ci.yml` with a `tests-required` aggregator job whose `needs:` references the not-yet-existing jobs `Unit`, `Lint`. Push a branch; the gate must FAIL because the referenced jobs don't exist / the workspace doesn't yet build cleanly under `-D warnings`.
- [ ] **Step 2 — Confirm red:** `act -j tests-required` (or push + observe Actions) → fails: "Failed: Unit Lint" or a clippy `-D warnings` error from the skeleton.
- [ ] **Step 3 — Green:** Implement `.github/workflows/ci.yml` per 01 §10 verbatim shape: `concurrency` cancel-in-progress; `Unit` matrix (`macos-26, ubuntu-22.04, ubuntu-24.04, ubuntu-24.04-arm`) running `./scripts/run_task.sh test-unit`; `Lint` running `lint-all-ci`; the `tests-required` aggregator that `jq`-checks every `needs.*.result == success`. Every job uses `.github/actions/install-nix` and `shell: nix develop --command bash -x {0}` (CI calls a TASK, never `cargo`/`bazel` directly). Define `build`/`test-unit`/`lint`/`lint-all-ci` tasks in `Taskfile.yml` (01 §5.2) wrapping `cargo build --workspace` + `cargo build -p avalanchers` + `cargo nextest run --workspace --all-features --profile ci` + `cargo test --doc` + `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] **Step 4 — Confirm green:** `./scripts/run_task.sh build && ./scripts/run_task.sh test-unit && ./scripts/run_task.sh lint` all pass locally; pushed `tests-required` is green.
- [ ] **Step 5 — Commit:** `ci: tests-required aggregator + buildable-&-green base jobs (M0)`

> **This is the milestone exit gate.** Every subsequent task in this plan ADDS a job to `tests-required`. State in the PR description that each milestone (M0–M9) exits only when `tests-required` is green, which by construction includes all checks added so far.

---

### Task X.2: Pinned Nix dev shell (`flake.nix`) + `rust-toolchain.toml` deepening + `install-nix` action
**Workstream:** 6 (Bazel/Nix CI)  ·  **First lands:** M0  ·  **Depends on:** X.1, M0 `rust-toolchain.toml`  ·  **Spec:** 01 §2, §3
**Files:** `flake.nix`, `flake.lock`, `.envrc`, `rust-toolchain.toml` (deepen: components + targets), `.github/actions/install-nix/action.yml`, `scripts/install_nix.sh`
- [ ] **Step 1 — Red/first check:** CI job `Unit` on a fresh runner must reproduce the exact toolchain; before the flake exists, `nix develop --command rustc --version` fails (no flake).
- [ ] **Step 2 — Confirm red:** `nix develop --command bash -c 'rustc --version && cargo nextest --version'` → error: no `flake.nix` / missing tools.
- [ ] **Step 3 — Green:** Write `flake.nix` per 01 §3.1 verbatim: inputs `nixpkgs/nixos-25.11`, `oxalica/rust-overlay` (follows nixpkgs), `flake-utils`; the four `allSystems`; `rust-bin.fromRustupToolchainFile ./rust-toolchain.toml`; `packages` = pinned toolchain + `cargo-nextest cargo-deny cargo-audit cargo-llvm-cov cargo-machete taplo just go-task` + FFI deps (`clang llvmPackages.libclang cmake pkg-config openssl`) + `bazelisk` (+ `bazel` symlink) + `buildifier` + `protobuf buf` + monitoring/kube + `shellcheck yamlfmt actionlint jq ripgrep solc s5cmd`; `LIBCLANG_PATH`; `shellHook` (PATH `scripts`+`bin`, `CARGO_HOME`). Add `.envrc` (01 §3.2). Deepen `rust-toolchain.toml` to add `components = [rustfmt, clippy, rust-src, llvm-tools]` and the four cross `targets` (01 §2). Commit `flake.lock`. Write `.github/actions/install-nix` (installs Nix + warms flake).
- [ ] **Step 4 — Confirm green:** `nix develop --command bash -c 'rustc --version && cargo nextest --version && cargo deny --version && bazel version'` succeeds and `rustc` matches `rust-toolchain.toml`.
- [ ] **Step 5 — Commit:** `ci: pinned Nix dev shell via rust-overlay + install-nix action (M0)`

---

### Task X.3: Bazel bzlmod + rules_rust + crate_universe + gazelle_rust + dirty-tree metadata gate
**Workstream:** 6 (Bazel/Nix CI)  ·  **First lands:** M0  ·  **Depends on:** X.1, X.2, M0 `Cargo.lock`  ·  **Spec:** 01 §4
**Files:** `MODULE.bazel`, `MODULE.bazel.lock`, `BUILD.bazel`, `.bazelrc`, `.bazelversion`, `Taskfile.yml` (`bazel-*` tasks), `scripts/bazel_workspace_status.sh`, `scripts/check_clean_branch.sh`, `.github/workflows/ci.yml` (add `bazel` job)
- [ ] **Step 1 — Red/first check:** Add the `bazel` CI job (`bazel-check-metadata` + `bazel-build` + `bazel-test`) to `tests-required.needs`. Before BUILD files exist, `bazelisk build //...` fails / gazelle diff is dirty.
- [ ] **Step 2 — Confirm red:** `bazelisk build //...` → error: no module / no targets; `bazelisk run //:gazelle_check` reports a dirty tree.
- [ ] **Step 3 — Green:** Write `MODULE.bazel` per 01 §4.2 (`rules_rust 0.70.0`, `rules_proto`, `toolchains_protoc`, `gazelle 0.45.0`, `gazelle_rust 0.2.0`; `rust.toolchain(versions=["1.90.0"])` in lock-step with `rust-toolchain.toml`; `crate.from_cargo(cargo_lockfile="//:Cargo.lock", manifests=["//:Cargo.toml"])`; prost/tonic toolchain). Write root `BUILD.bazel` (01 §4.3: `gazelle_binary` w/ `@gazelle_rust//rust_language`, `:gazelle`, `:gazelle_check` mode=diff; `# gazelle:rust_cargo_lockfile` + `# gazelle:rust_crates_prefix @crates//:`). Write `.bazelrc` (01 §4.4: `--lockfile_mode=update`, overflow/debug-assertions ON by default + `:fast`/`:opt`/`:release` configs, `LIBCLANG_PATH`, macOS toolchain). Pin `.bazelversion` = `8.0.1`. Add `bazel-build`/`bazel-test`/`bazel-gazelle-generate`/`bazel-fmt`/`bazel-check-metadata` tasks; `bazel-check-metadata` runs gazelle + buildifier + `check-clean-branch` (dirty-tree gate on `Cargo.lock`/`MODULE.bazel.lock`/`BUILD.bazel`). Commit `MODULE.bazel.lock`.
- [ ] **Step 4 — Confirm green:** `./scripts/run_task.sh bazel-check-metadata && ./scripts/run_task.sh bazel-build && ./scripts/run_task.sh bazel-test` all pass; `bazel` job green in `tests-required`.
- [ ] **Step 5 — Commit:** `ci: Bazel bzlmod + crate_universe + gazelle_rust with dirty-tree metadata gate (M0)`

> **Deepens:** each later milestone adds crates; `bazel-gazelle-generate` regenerates their `BUILD.bazel`, and the dirty-tree gate fails any PR that adds `.rs` without committing the regenerated BUILD.

---

### Task X.4: `cargo xtask` task surface + `run_task.sh` parity (the canonical task launcher)
**Workstream:** 6 (CI) / 8 (PORTING)  ·  **First lands:** M0  ·  **Depends on:** X.1  ·  **Spec:** 01 §5, 02 §1, 24 §A.2
**Files:** `xtask/Cargo.toml`, `xtask/src/main.rs`, `xtask/src/{test.rs,vectors.rs,porting.rs,lint_determinism.rs}` (modules stubbed; filled by later tasks), `Taskfile.yml`, `justfile`, `scripts/run_task.sh`, `scripts/nix_run.sh`
- [ ] **Step 1 — Red/first check:** `cargo xtask --help` must list `test-unit`, `test-unit-fast`, `test-fuzz`, `test-differential`, `test-reexecute`, `vectors`, `porting-report`, `lint-determinism`. Before xtask exists, the command fails.
- [ ] **Step 2 — Confirm red:** `cargo xtask porting-report` → error: no such package `xtask`.
- [ ] **Step 3 — Green:** Add `xtask/` as a workspace member with `#![forbid(unsafe_code)]` and the license header. Implement a `clap`-based dispatcher mirroring Taskfile names (02 §1, §1 mapping table). Wire subcommands as thin shells initially (each prints "not yet implemented" and exits non-zero for the unimplemented ones so later tasks turn them green). Ensure `Taskfile.yml` test/lint tasks call `cargo xtask` where appropriate (02 §1: `cargo xtask test-*`). Confirm `scripts/run_task.sh` + `scripts/nix_run.sh` (carried from Go) wrap tasks in `nix develop --command`. Add the `justfile` thin wrapper (01 §5.3).
- [ ] **Step 4 — Confirm green:** `cargo xtask --help` lists every subcommand; `./scripts/run_task.sh --list` shows the task surface.
- [ ] **Step 5 — Commit:** `build: cargo xtask task surface mirroring Taskfile (M0)`

---

### Task X.5: `cargo-deny` dependency policy + `deps-tidy`/`deps-unused`/`deps-audit` + dirty-`Cargo.lock` gate
**Workstream:** 6 (Bazel/Nix CI)  ·  **First lands:** M0  ·  **Depends on:** X.1, X.3  ·  **Spec:** 01 §9, §5.1
**Files:** `deny.toml`, `Taskfile.yml` (`deps-tidy`, `deps-unused`, `deps-audit`), `.github/workflows/ci.yml` (add `check_deps_tidy` job)
- [ ] **Step 1 — Red/first check:** Add a `check_deps_tidy` job to `tests-required`. Introduce a deliberately banned dep (e.g. `native-tls`) in a scratch member; `cargo deny check` must fail.
- [ ] **Step 2 — Confirm red:** `cargo deny check` → error: banned `openssl`/`native-tls` (or stale `Cargo.lock` from `cargo update --locked`).
- [ ] **Step 3 — Green:** Write `deny.toml` per 01 §9 (advisories `yanked=deny`; permissive license allow-list; `[bans]` `wildcards=deny`, ban `native-tls`/duplicate mock libs per 00 §4; `[sources]` allow-git for reth + firewood). Add `deps-tidy` (`cargo update --workspace --locked` + `cargo deny check` + `bazelisk mod tidy`), `deps-unused` (`cargo machete`), `deps-audit` (`cargo audit`). Remove the scratch banned dep. Add `check_deps_tidy` CI job calling `deps-tidy` (dirty-`Cargo.lock`/`MODULE.bazel.lock` gate).
- [ ] **Step 4 — Confirm green:** `./scripts/run_task.sh deps-tidy && ./scripts/run_task.sh deps-unused` pass; `check_deps_tidy` green.
- [ ] **Step 5 — Commit:** `ci: cargo-deny policy + deps-tidy dirty-lock gate (M0)`

---

### Task X.6: Lint config + license-header + SAE strict + `taulint` gates
**Workstream:** 6 (Bazel/Nix CI) / 4 (Error taxonomy nudge)  ·  **First lands:** M0  ·  **Depends on:** X.1, X.2  ·  **Spec:** 01 §7, 24 §8
**Files:** `rustfmt.toml`, `clippy.toml`, `.editorconfig` (Rust stanza), `header.yml`, root `Cargo.toml` `[workspace.lints]`, `scripts/{check_license_headers.sh,add_license_headers.sh,lint_saevm.sh,tau_lint.sh,shellcheck.sh}`, `Taskfile.yml` (`lint`, `lint-fix`, `lint-saevm`, `lint-shell`, `lint-action`, `check-yaml-fmt`), `.github/workflows/ci.yml` (`taulint` job)
- [ ] **Step 1 — Red/first check:** Drop a `.rs` file with no license header and a SAE crate placeholder with a raw `as` cast on `TauSeconds`. `check_license_headers.sh` + `lint_saevm.sh` + `tau_lint.sh` must all fail.
- [ ] **Step 2 — Confirm red:** `./scripts/run_task.sh lint` → "missing license header"; `./scripts/run_task.sh lint-saevm` → clippy `cast_possible_truncation`; `./scripts/tau_lint.sh` → "use a typed Duration".
- [ ] **Step 3 — Green:** Write `rustfmt.toml` (01 §7.1: `group_imports=StdExternalCrate`, `newline_style=Unix`), `clippy.toml` (01 §7.3), `.editorconfig` Rust stanza (01 §7.2), `header.yml`. Set `[workspace.lints.rust]`/`[workspace.lints.clippy]` in root `Cargo.toml` (01 §7.3: `unsafe_code=forbid`, `unwrap_used=deny`, `dbg_macro=deny`, `arithmetic_side_effects=warn`). Write the five scripts (01 §7.4/§7.6, §10 `tau_lint.sh`). Define `lint`/`lint-fix`/`lint-saevm`/`lint-shell`/`lint-action`/`check-yaml-fmt` tasks (these are already aggregated by `lint-all-ci` from X.1). Add the `taulint` CI job to `tests-required`.
- [ ] **Step 4 — Confirm green:** `./scripts/run_task.sh lint && ./scripts/run_task.sh lint-saevm && ./scripts/tau_lint.sh` all pass; `taulint` green.
- [ ] **Step 5 — Commit:** `ci: clippy/rustfmt/license + SAE strict + taulint gates (M0)`

---

### Task X.7: Codegen dirty-tree gates (`generate-protobuf`/`generate-mocks` + `check-generate-*`)
**Workstream:** 6 (Bazel/Nix CI)  ·  **First lands:** M0  ·  **Depends on:** X.1, X.2  ·  **Spec:** 01 §8
**Files:** `scripts/{protobuf_codegen.sh,generate_mocks.sh,check_clean_branch.sh,check_rust_version.sh}`, `Taskfile.yml` (`generate-protobuf`, `generate-mocks`, `check-generate-protobuf`, `check-generate-mocks`, `check-rust-version`), `.github/workflows/ci.yml` (`check_generated_protobuf`, `check_mocks` jobs)
- [ ] **Step 1 — Red/first check:** Add `check_generated_protobuf` + `check_mocks` jobs to `tests-required`. Before scripts exist, the jobs fail.
- [ ] **Step 2 — Confirm red:** `./scripts/run_task.sh check-generate-protobuf` → error: missing `protobuf_codegen.sh`.
- [ ] **Step 3 — Green:** Write `protobuf_codegen.sh` (01 §8.1: `buf lint` + `buf breaking --against '.git#branch=master'` + `cargo check --workspace --all-features` — proto is `build.rs`-generated, NOT committed), `generate_mocks.sh` (01 §8.2: `cargo check --workspace --all-features --tests` — mockall is macro-generated), `check_rust_version.sh` (assert `rust-toolchain.toml` == `MODULE.bazel` == CI matrix), `check_clean_branch.sh`. Define the tasks; `check-generate-*` run the generator then `check-clean-branch`. Add the two CI jobs.
- [ ] **Step 4 — Confirm green:** `./scripts/run_task.sh check-generate-protobuf && ./scripts/run_task.sh check-generate-mocks && ./scripts/run_task.sh check-rust-version` pass.
- [ ] **Step 5 — Commit:** `ci: codegen + rust-version dirty-tree gates (M0)`

---

### Task X.8: `.config/nextest.toml` CI profile + coverage floors (`cargo-llvm-cov`) 🟡 COVERAGE-FLOOR GATE LANDED (2026-06-17)
**Workstream:** 6 (CI) / 1 (differential timeouts)  ·  **First lands:** M0  ·  **Depends on:** X.1; M0 minimal `.config/nextest.toml`  ·  **Spec:** 01 §6, 02 §11.6 timeouts, 02 §12
**Files:** `.config/nextest.toml` (deepen), root `Cargo.toml` `[profile.dev-checks]`/`[profile.ci]`, `Taskfile.yml` (`test-coverage`), `scripts/check_coverage_floor.sh`, `.github/workflows/ci.yml` (`coverage` job)
- [ ] **Step 1 — Red/first check:** Add a `coverage` job enforcing per-crate floors (90% protocol-critical / 80% VM / 70% glue, 02 §12). With a crate below floor, `check_coverage_floor.sh` must fail.
- [ ] **Step 2 — Confirm red:** `./scripts/run_task.sh test-coverage && ./scripts/check_coverage_floor.sh` → "ava-codec 71% < floor 90%".
- [ ] **Step 3 — Green:** Deepen `.config/nextest.toml` (01 §6: `[profile.ci]` retries=1, slow-timeout 120s, junit; `[[profile.ci.overrides]]` 900s for `package(ava-saevm)` / `test(/differential_/)` — 02 §11.6). Add `[profile.dev-checks]`/`[profile.ci]` to root `Cargo.toml` (01 §6). Define `test-coverage` (`cargo llvm-cov nextest --all-features --lcov`). Write `check_coverage_floor.sh` parsing `lcov.info` against a committed per-crate floor table (02 §12; "a PR may not lower a crate below its floor"). Add `coverage` CI job.
- [ ] **Step 4 — Confirm green:** `./scripts/run_task.sh test-coverage` emits `lcov.info`; `check_coverage_floor.sh` passes for current crates.
- [ ] **Step 5 — Commit:** `ci: nextest CI profile + coverage floors (M0)`

> **Deepens:** each milestone's new crates add a floor row; the floor table is the per-crate gate.

> **AS-BUILT (2026-06-17, commit 63faa81 / merge 593b36b).** `.config/nextest.toml` `[profile.ci]`
> was already in place (overrides for `ava-saevm`/differential at 900s). This wave made
> `scripts/check_coverage_floor.sh` a **real lcov-parsing per-crate floor gate**: it parses `SF:`/`DA:`
> records, groups source files by the `crates/<name>/` path segment, computes per-crate line %, and
> exits non-zero on any crate below its floor (a FLOORS-listed crate absent from the lcov is skipped
> with a WARN so scoped runs don't spuriously break). Floors are **measured-then-ratcheted**
> (floor = measured rounded down to nearest 5, never above measured): `ava-types`=75 (meas 79%),
> `ava-utils`=65 (meas 66%), `ava-version`=80 (meas 82%). Wired via a `coverage-floor` Taskfile task
> into `nightly.yml` (NOT per-PR `tests-required` — a full instrumented `cargo llvm-cov` over
> reth/firewood/saevm is too heavy per-PR). Red/green proven on synthetic lcov fixtures
> (above-floor→exit 0, below-floor→exit 1) + a real scoped `-p ava-types -p ava-utils -p ava-version`
> run. **Remaining (deepens):** ratchet floors + add rows for VM/glue crates as scoped coverage is
> measured per milestone; the per-PR `coverage` CI job stays deferred until measured floors exist for
> the heavy crates.

---

### Task X.9: Per-crate `thiserror` error taxonomy + `assert_matches!` mirrors of Go `errors.Is`
**Workstream:** 4 (Error taxonomy)  ·  **First lands:** M0  ·  **Depends on:** X.1  ·  **Spec:** 02 §3.1, 24 (sentinel determinism), 00 §7.1
**Files:** `crates/ava-types/src/error.rs`, `crates/ava-codec/src/error.rs`, `crates/ava-utils/src/error.rs` (+ each later crate), `xtask/src/lint_errors.rs` (optional grep for ad-hoc `Error::Other(String)` on protocol paths), per-crate `tests/error_parity.rs`
- [ ] **Step 1 — Red/first check:** For each M0 crate, write `tests/error_parity.rs` asserting `assert_matches!(result, Err(Error::<Variant>{..}))` for every Go sentinel that crate must reproduce (e.g. codec `ErrUnknownVersion`, `ErrCantPackVersion`; safemath overflow). The test fails because the variants don't exist yet.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -E 'test(error_parity)'` → compile error / `assert_matches!` mismatch (no `Error::UnknownVersion`).
- [ ] **Step 3 — Green:** Define a `thiserror` `Error` enum + `pub type Result<T>` per crate (00 §7.1), with one variant per Go sentinel error, citing the Go symbol in a doc comment. Map each Go `errors.Is(err, ErrFoo)` site that the spec covers to a typed variant; tests use `assert_matches!`, never string compare (02 §3.1, mirrors Go's `ErrorIs` lint rule). Optionally add `xtask lint-errors` forbidding stringly-typed errors on codec/consensus paths.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -E 'test(error_parity)'` passes for all M0 crates.
- [ ] **Step 5 — Commit:** `errors: thiserror taxonomy + errors.Is parity tests (M0)`

> **Deepens:** every milestone's new crate adds its enum + `error_parity.rs`; "subsystem done" requires its sentinels mirrored.

---

### Task X.10: `tools/extract-vectors` (`vectorgen`) Go harness + `tests/vectors/` corpus + manifest (M0 categories)
**Workstream:** 2 (Golden-vector extraction)  ·  **First lands:** M0  ·  **Depends on:** X.4 (xtask), pinned Go tree  ·  **Spec:** 22 §1–§4, §8; 02 §6
**Files:** `tools/extract-vectors/{go.mod,main.go,emit.go,emit_codec.go,emit_crypto.go,emit_sampler.go,emit_genesis.go}`, `tests/vectors/manifest.json`, `tests/vectors/schema/record.schema.json`, `tests/vectors/{codec,crypto-secp,crypto-bls,sampler-rng,address,genesis-ids}/*.json`
- [ ] **Step 1 — Red/first check:** Stand up the corpus directories with the JSON-record schema (22 §1.1) but no files yet; `cargo xtask vectors verify` must fail because the manifest lists categories with no files / no hashes.
- [ ] **Step 2 — Confirm red:** `cargo xtask vectors verify` → "no vectors found for category codec" / manifest orphan.
- [ ] **Step 3 — Green:** Write the Go `vectorgen` per 22 §3 (`go.mod` pinned to `manifest.avalanchego_revision`; `Emitter` interface; `Writer` canonicalizing JSON sorted-keys 2-space + per-file sha256 + manifest provenance). Implement the M0 emitters per 22 §0/§8 (22 §3.2 sketches): `emit_codec` (real `linearcodec.Marshal` on canonical UTXO/tx ⇒ `output_hex`+`output_id`), `emit_crypto` (RFC6979 secp lift + BLS sign/agg/PoP), `emit_sampler` (raw gonum `MT19937`/`MT19937_64` streams — the R1 gate, 00 §11.2), `emit_genesis` (`FromConfig` Mainnet/Fuji/local ⇒ `genesis_sha256`+`block_id`), `address`. Run it to emit `tests/vectors/{codec,crypto-secp,crypto-bls,sampler-rng,address,genesis-ids}/`. Write `manifest.json` (22 §2.1) + `record.schema.json` (22 §1.1).
- [ ] **Step 4 — Confirm green:** `cargo xtask vectors verify` passes (hashes match, schema valid, no orphans, consensus-critical categories `static+live`).
- [ ] **Step 5 — Commit:** `vectors: extract-vectors harness + M0 corpus (codec/crypto/sampler/genesis/address)`

> **Deepens:** M1 `merkle-roots`, M2 `p2p-framing`/`proto-wire`, M4 `blocks-txs-pchain`/`fee-math`, M5 `blocks-txs-xchain`/`warp`, M6 `evm-state-roots`, M7 saevm — each adds an emitter + category (22 §8).

---

### Task X.11: `ava-testvectors` generic loader + `golden_*` tests + `cargo xtask vectors verify` 🟡 `vectors verify` REAL (2026-06-17)
**Workstream:** 2 (Golden-vector extraction)  ·  **First lands:** M0  ·  **Depends on:** X.10  ·  **Spec:** 22 §2.2, §6
**Files:** `crates/ava-testvectors/{Cargo.toml,src/lib.rs}` (feature `testutil`, `#![forbid(unsafe_code)]`), `xtask/src/vectors.rs` (`verify`/`diff`/`regen`), `crates/ava-codec/tests/golden_codec.rs`, `crates/ava-utils/tests/golden_sampler.rs`, `crates/ava-crypto/tests/golden_crypto.rs`, `crates/ava-genesis/tests/golden_genesis.rs`
- [ ] **Step 1 — Red/first check:** Write `golden_codec_utxo`, `golden_sampler_mt19937_stream`, BLS/secp, genesis-id tests (22 §6.2) that load via `load_vectors::<I,O>` and assert byte/value equality. They fail until the loader + Rust impls produce matching bytes.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -E 'test(golden_)'` → "byte mismatch: utxo/min" / "MT stream diverges from gonum".
- [ ] **Step 3 — Green:** Implement `ava-testvectors` `load_vectors::<I,O>(category)` + `Vector<I,O>` per 22 §6.1 (resolve `tests/vectors/<category>/`, concat records, panic with clear message). Implement `xtask vectors verify` (22 §2.2: schema_version, orphans, recompute sha256 vs manifest, validate records, oracle-mode rule), `vectors diff --against <dir>`, `vectors regen`. Make each `golden_*` test green by ensuring the M0 Rust impls (codec/sampler/crypto/genesis) match the extracted bytes (this is the red→green pull on those subsystems).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -E 'test(golden_)' && cargo xtask vectors verify` pass.
- [ ] **Step 5 — Commit:** `vectors: ava-testvectors loader + golden_* tests + xtask vectors verify (M0)`

> **AS-BUILT (2026-06-17, commit dec7f5b / merge 571a06a).** `cargo xtask vectors verify`/`diff`/`regen`
> are now **real** (replaced the no-op scaffold in `xtask/src/vectors.rs`). `verify` runs three checks
> over `tests/vectors/`: (1) JSON-schema validity of every `*.json`; (2) orphan/coverage — every
> on-disk surface dir ↔ a `manifest.json.surfaces` key (both directions); (3) **checksum** — recomputes
> sha256 of all 48 vector files vs a new committed `tests/vectors/checksums.txt`, failing on
> mismatch/missing/extra/absent-checksums. `regen` (re)writes `checksums.txt`; `diff --against <dir>`
> does path-set + per-file byte compare (the Rust half of the drift flow). Added 7 missing surface keys
> to `manifest.json` (`archivedb`, `merkledb`, `message`, `saevm`, `sync`, `tls`; `codec` promoted from
> TODO). Red/green proven (corrupt vector → exit 1; restore → exit 0). The existing `vectors_verify` CI
> job is now a meaningful gate. **Still scaffold:** `ava-testvectors::load_vectors` typed loader +
> `golden_*` red→green pull (separate, owned by the milestone subsystems) remain as-is.

---

### Task X.12: `vectors-drift` + `vectors-verify` CI jobs (re-extract vs pinned Go commit) 🟡 verify REAL; drift extraction still Go-gated (2026-06-17)
**Workstream:** 2 (Golden-vector extraction)  ·  **First lands:** M0  ·  **Depends on:** X.10, X.11  ·  **Spec:** 22 §7, 02 §6.2
**Files:** `.github/workflows/ci.yml` (`vectors_verify`, `vectors_drift` jobs), `Taskfile.yml` (`test-vectors`), `scripts/vectors_drift.sh`
- [ ] **Step 1 — Red/first check:** Add `vectors_drift` to `tests-required`. Hand-edit one committed vector byte; the drift job must fail when it re-extracts from the pinned Go commit and diffs.
- [ ] **Step 2 — Confirm red:** Run `scripts/vectors_drift.sh` locally (checks out `manifest.avalanchego_revision`, `go run ./tools/extract-vectors --out $TMP`, `cargo xtask vectors diff --against $TMP`) → fails on the tampered byte.
- [ ] **Step 3 — Green:** Implement `vectors_verify` job (22 §7: `cargo xtask vectors verify` + `cargo nextest run -E 'test(golden_)'`, no Go needed) and `vectors_drift` job (22 §7: pin Go to `avalanchego_revision`, re-run `vectorgen`, `cargo xtask vectors diff`). Revert the tampered byte. Document the "deliberate protocol change" flow (bump revision → `vectors regen` → review vector diff, 22 §7).
- [ ] **Step 4 — Confirm green:** Both jobs green on a clean tree; tampering re-fails them.
- [ ] **Step 5 — Commit:** `ci: vectors-verify + vectors-drift gates (M0)`

> **AS-BUILT (2026-06-17).** `vectors-verify` is now a real gate (the `vectors_verify` CI job calls
> `cargo xtask vectors verify`, which enforces the checksum/schema/orphan corpus invariants per X.11
> AS-BUILT). The `diff --against <dir>` half is implemented. **`vectors-drift` remains gated** on the
> Go re-extraction step (`scripts/vectors_drift.sh` still documents but does not run
> `go run ./tools/extract-vectors` — it needs a pinned `avalanchego` checkout); wiring that into a CI
> job is the remaining work, alongside `tools/extract-vectors` corpus completion (X.10).

---

### Task X.13: `ava-differential` crate skeleton — `LockstepDriver` + `Observation` + recorded-oracle mode
**Workstream:** 1 (Differential harness)  ·  **First lands:** M0  ·  **Depends on:** X.4, X.10/X.11 (recorded goldens), M0 codec/genesis/sampler  ·  **Spec:** 02 §11, §10.5
**Files:** `tests/differential/{Cargo.toml,src/lib.rs,src/driver.rs,src/observation.rs,src/program.rs,src/network.rs}` (member `ava-differential`, `#![forbid(unsafe_code)]`), `tests/differential/proptest-regressions/`, `xtask/src/test.rs` (`test-differential`, `test-reexecute`), `.config/nextest.toml` (already leashes `test(/differential_/)`)
- [ ] **Step 1 — Red/first check:** Write `differential_recorded_oracle_agrees` (proptest, small `cases`) that replays an `arb_program()` against the Rust binary/VM only and compares each `AwaitFinalization` `Observation` to a Go-recorded oracle (the reexecute/golden path, 02 §11.1 recorded mode, §10.5). It fails until the driver + observation + recorded fixtures exist.
- [ ] **Step 2 — Confirm red:** `cargo xtask test-differential --recorded` → compile error / "no Observation collector registered" / mismatch printing `DIFFERENTIAL_SEED=<n>`.
- [ ] **Step 3 — Green:** Create `ava-differential` per 02 §11: `Action` enum (`IssueTx/ApiCall/AdvanceTime/RestartNode/Partition/AwaitFinalization`, `arbitrary::Arbitrary`), `arb_program()` (02 §11.2), `LockstepDriver` (derives all tx/key bytes deterministically from the program seed; feeds the same seed to the sampler RNG, 00 §6.1), `Observation::collect(&node).normalized()` (02 §11.3/§11.4: block IDs/heights, state/merkle roots, normalized API JSON; strip timestamps, sort collections, mask per-instance IDs). Implement recorded-oracle mode (replay vs Go-recorded outputs, 02 §11.1) using the M0 goldens + reexecute artifacts. Wire `xtask test-differential [--seed N] [--recorded]` (02 §11.5: single-seed replay) and `test-reexecute`. Commit `proptest-regressions/`.
- [ ] **Step 4 — Confirm green:** `cargo xtask test-differential --recorded` passes; `cargo xtask test-differential --seed <n>` deterministically replays one program.
- [ ] **Step 5 — Commit:** `differential: ava-differential LockstepDriver + Observation + recorded-oracle mode (M0)`

> **THIS IS THE CENTRAL HARNESS TASK.** Deepens: each subsystem adds its `Observation` collector as it lands — M2 peer/handshake (live), M3 vm-rpc, M4 P-Chain mempool order, M5 X-Chain, M6 EVM state roots, M7 SAE, M8 validator/API views (02 §11.3 table, §13.6 contract).

---

### Task X.14: Differential CI wiring — per-PR recorded-oracle + reexecute job
**Workstream:** 1 (Differential harness)  ·  **First lands:** M0  ·  **Depends on:** X.13  ·  **Spec:** 02 §11.7
**Files:** `.github/workflows/ci.yml` (`differential` job), `Taskfile.yml` (`test-differential`, `test-reexecute`)
- [ ] **Step 1 — Red/first check:** Add the `differential` job (recorded-oracle + reexecute) to `tests-required`. Before wired, the job fails (no task).
- [ ] **Step 2 — Confirm red:** Push → `differential` job fails: "task test-differential not found".
- [ ] **Step 3 — Green:** Add `differential` job per 02 §11.7 (per-PR: recorded-oracle on a small `cases` budget + the reexecute suite) running `./scripts/run_task.sh test-differential` + `test-reexecute`. Ensure any failure prints + commits the `DIFFERENTIAL_SEED` so it replays forever (02 §11.7, §13.6).
- [ ] **Step 4 — Confirm green:** `differential` green in `tests-required`.
- [ ] **Step 5 — Commit:** `ci: per-PR recorded-oracle differential + reexecute (M0)`

---

### Task X.15: Live two-binary mode + mixed Go↔Rust network (`Network::start(Binary::{Go,Rust})`)
**Workstream:** 1 (Differential harness)  ·  **First lands:** M2  ·  **Depends on:** X.13; M2 networking/p2p; tmpnet  ·  **Spec:** 02 §11.1/§11.6, §10.2, 22 §5
**Files:** `tests/differential/src/network.rs` (tmpnet integration), `tests/e2e/` (mixed-net harness), `Taskfile.yml` (`test-e2e`, `test-differential-live`)
- [ ] **Step 1 — Red/first check:** Write `differential_two_binary_agree` (02 §11.6 sketch) that boots a Go network and a Rust network via tmpnet with identical genesis/config/seed and asserts `Observation` equality at each `AwaitFinalization`; plus a mixed Go+Rust net reaching the same height with no fork (02 §10.2). Fails until tmpnet wiring exists.
- [ ] **Step 2 — Confirm red:** `cargo xtask test-differential-live` → "tmpnet binary not found" / divergence with seed dump.
- [ ] **Step 3 — Green:** Implement `Network::start(Binary, &cfg)` as a different `AVALANCHEGO_PATH` handed to tmpnet (02 §11.6) reusing tmpnet as-is (02 §10.2). Implement `NetworkConfig::deterministic(seed, nodes)` assigning the i-th Go and i-th Rust node identical seed-derived node IDs/TLS certs (02 §11.4). Add the mixed-net interop scenario. Gate consensus-critical `static+live` categories (22 §5).
- [ ] **Step 4 — Confirm green:** `cargo xtask test-differential-live` passes on a small `cases` budget against locally built Go + Rust binaries.
- [ ] **Step 5 — Commit:** `differential: live two-binary + mixed Go↔Rust net (M2)`

---

### Task X.16: Per-parser `cargo-fuzz` targets + corpora + `test-fuzz` smoke
**Workstream:** 7 (Fuzzing corpora)  ·  **First lands:** M0 (`ava-codec`)  ·  **Depends on:** X.4  ·  **Spec:** 02 §8
**Files:** `crates/ava-codec/fuzz/{Cargo.toml,fuzz_targets/decode_utxo.rs}`, `crates/ava-codec/fuzz/corpus/decode_utxo/`, `xtask/src/test.rs` (`test-fuzz`, `test-fuzz-long`), `.github/workflows/ci.yml` (`fuzz` job)
- [ ] **Step 1 — Red/first check:** Write the `decode_utxo` fuzz target (02 §8 sketch: decode arbitrary bytes; on success, round-trip `decode→encode→decode` must be byte-stable). Run smoke; it fails until `ava-codec` decode is panic-safe + round-trip-stable.
- [ ] **Step 2 — Confirm red:** `cargo xtask test-fuzz` → libfuzzer crash artifact written to `fuzz/artifacts/` (a panic on malformed input).
- [ ] **Step 3 — Green:** Add `ava-codec/fuzz/` (`libfuzzer-sys` + `arbitrary`) with `decode_utxo`. Implement `xtask test-fuzz` (brief smoke per target, like Go `test-fuzz`) and `test-fuzz-long FUZZTIME=...`. Commit seed corpus under `fuzz/corpus/<target>/`; commit any crash as a regression seed (02 §8). Add the `fuzz` CI job (nightly + smoke per PR on x86-64/aarch64 Linux, nightly toolchain).
- [ ] **Step 4 — Confirm green:** `cargo xtask test-fuzz` runs all targets briefly with no crash.
- [ ] **Step 5 — Commit:** `fuzz: ava-codec cargo-fuzz target + corpus + test-fuzz smoke (M0)`

> **Deepens:** M1 `ava-merkledb` (structure-aware op stream), M2 `ava-message` (wire frames), M4–M7 P/X/C/SAE block parsers (02 §8 mandated targets).

---

### Task X.17: Metrics-name parity — registry model + `metrics_names` golden snapshot
**Workstream:** 3 (Metrics-name parity)  ·  **First lands:** M2  ·  **Depends on:** X.13 (live Observation), M2 network metrics  ·  **Spec:** 18 §1, §2, §3, §4
**Files:** `crates/ava-api/src/metrics/{mod.rs,prefix_gatherer.rs,label_gatherer.rs}`, `tests/snapshots/metrics_names.snap` (insta), `tests/differential/src/observation.rs` (add metrics-schema collector), `.github/workflows/ci.yml` (extend `differential`/nightly)
- [ ] **Step 1 — Red/first check:** Write `metrics_names_superset_of_go` (18 §3): boot Go node → `GET /ext/metrics` → parse `{(name, type, sorted(label_keys))}` schema as the golden snapshot; boot Rust node and assert its set is a superset. Fails until the Rust registry emits the `avalanche_*` families.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -E 'test(metrics_names)'` → "missing Go family avalanche_network_peers".
- [ ] **Step 3 — Green:** Implement `PrefixGatherer`/`LabelGatherer`/`make_and_register` in `ava-api` per 18 §1.2 (`NAMESPACE_SEP="_"`, `PLATFORM_NAME="avalanche"`, `CHAIN_LABEL="chain"`, primary-alias label values). Register the M2 network/handler families (18 §2.1–§2.5). Snapshot the Go schema as the `metrics_names` golden; assert superset with the §4 process/`go_*` waiver documented (do NOT fake `go_*`). Add the metrics-schema collector to `Observation`.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -E 'test(metrics_names)'` passes; snapshot reviewed via `cargo insta`.
- [ ] **Step 5 — Commit:** `metrics: PrefixGatherer/LabelGatherer + metrics_names parity snapshot (M2)`

> **Deepens:** M3 rpcchainvm/grpc families (18 §2.15), M4–M6 per-VM `avalanche_<vm>_*` (18 §2.11), M8 full snapshot incl. process collector (18 §4). The snapshot grows per milestone; a rename is a compatibility break (18 §3).

---

### Task X.18: Observability — `ava-logging` span/level model + JSON-line shape golden + OTel exporter 🟡 JSON-line shape golden LANDED (2026-06-17); OTel still M8-deferred
**Workstream:** 5 (Observability)  ·  **First lands:** M0 (log model)  ·  **Depends on:** X.1  ·  **Spec:** 18 §5, §6
**Files:** `crates/ava-logging/src/{lib.rs,level.rs,format.rs,factory.rs}`, `tests/log_shape.rs` (JSON-line shape), `crates/ava-logging/src/otel.rs` (M8), `tests/differential/...` (span-name checks)
- [ ] **Step 1 — Red/first check:** Write `json_log_line_shape` asserting a `tracing` JSON event renders exactly `{"level","timestamp","logger","caller","msg",...}` with lowercase level string + integer-nanosecond durations (18 §5.2). Fails until the custom format layer exists.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -E 'test(json_log_line_shape)'` → key/order/level-string mismatch.
- [ ] **Step 3 — Green:** Implement `AvaLevel` (8 names + Go's Trace-above-Debug ordering, 18 §5.1), the plain/colors/json formats (18 §5.2, exact key order + lowercased level + `[01-02|15:04:05.000]` console layout), per-chain rolling file layer (`<alias>.log`, lumberjack-equivalent rotation, 18 §5.3/§5.4), and `reload` handles for `setLoggerLevel`. Ensure span names mirror Go log messages so greps keep working (00 §7.3). Defer the OTLP exporter (`otel.rs`: `opentelemetry-otlp` + `tracing-opentelemetry`, `Sampler::TraceIdRatioBased`, resource attrs parity) to **M8** wiring (18 §6) — stub it now behind `--tracing-exporter-type=disabled` (no-op).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -E 'test(json_log_line_shape)'` passes; M8: OTLP exporter emits spans to a local collector.
- [ ] **Step 5 — Commit:** `obs: ava-logging level/format model + JSON-line golden (M0); OTLP exporter (M8)`

> **AS-BUILT (2026-06-17, commit 6aeaf99 / merge 78014d2).** The `AvaLevel`/format model already
> existed (`format.rs` plain/colors/json + `level.rs` 8-name taxonomy). This wave added the missing
> **JSON-line shape golden**: `format::golden::json_line_shape_is_frozen` drives the real
> `AvaFormat::new(Format::Json)` through a `tracing_subscriber` registry into an in-memory buffer and
> freezes the byte shape — reserved-key order `level < timestamp < logger < caller < msg`, then
> structured fields in sorted (BTreeMap) order — against committed golden
> `crates/ava-logging/tests/vectors/log_json_shape.golden` (timestamp normalized to `<TS>`; raw value
> asserted ISO8601 `…%H:%M:%S%.3fZ`). Output matches specs/18 §5.2 exactly (no discrepancy). Red proof:
> wrong key-order golden fails. **Still deferred to M8 (per Step 3):** the OTLP exporter (`otel.rs`,
> `opentelemetry-otlp` + `tracing-opentelemetry`) — not yet wired (no `opentelemetry-*` dep present).

---

### Task X.19: Determinism-audit PR gate — `cargo xtask lint-determinism` + clippy determinism lints + repeat-N proptest
**Workstream:** 4/6 (determinism is cross-cutting; gates merges)  ·  **First lands:** M0  ·  **Depends on:** X.4, X.6  ·  **Spec:** 24 PART A, §A.2, §B.6
**Files:** `xtask/src/lint_determinism.rs`, `xtask/determinism-allowlist.toml`, root `Cargo.toml` (consensus-crate `clippy::float_arithmetic=deny`, `arithmetic_side_effects=deny`), `.github/workflows/ci.yml` (fold into `Lint`/`lint-all`), `crates/ava-utils/src/clock.rs` (`Clock`/`RealClock`/`MockClock`), `tests/determinism_repeat.rs`, `.github/PULL_REQUEST_TEMPLATE.md`
- [ ] **Step 1 — Red/first check:** Introduce a `SystemTime::now()` outside `ava-utils::clock` and a `HashMap` field in a `#[derive(Codec)]` type. `cargo xtask lint-determinism` must flag both (hazards #5, #1, 24 §A). Also write the headline determinism repeat-N proptest (24 §B.6: same seed+workload+MockClock ⇒ byte-identical over N≥16 runs).
- [ ] **Step 2 — Confirm red:** `cargo xtask lint-determinism` → "wall-clock outside allowlist: …" + "HashMap in Codec type: …"; repeat-N test fails if any nondeterminism leaks.
- [ ] **Step 3 — Green:** Implement `lint-determinism` (24 §A.2, `syn`-based AST pass): ban wall-clock outside `ava-utils::clock` + bin wiring (#5), ban `HashMap`/`HashSet`/`IndexMap` in codec-derive types (#1), ban non-vendored RNG in sampler/proposervm (#4), Tau bare-second add grep (#8); allowlist entries require an inline `// determinism-allow: <reason>` the xtask verifies. Add the `Clock` trait + `RealClock`/`MockClock` to `ava-utils` (24 PART B verbatim). Raise `clippy::float_arithmetic`/`arithmetic_side_effects` to `deny` in consensus crates (24 §A.2). Fold `lint-determinism` into `lint-all`/`lint-all-ci` (X.1/X.6). Add the audit checklist (24 §A table) to `.github/PULL_REQUEST_TEMPLATE.md` as a tick-box gate. Remove the planted violations.
- [ ] **Step 4 — Confirm green:** `cargo xtask lint-determinism` reports zero findings; `cargo nextest run -E 'test(determinism_repeat)'` passes; `lint-all` green.
- [ ] **Step 5 — Commit:** `ci: determinism-audit gate (lint-determinism + clock + repeat-N proptest) (M0)`

> **AS-BUILT (2026-06-16f) — core lint-determinism pass LANDED.** `xtask/src/lint_determinism.rs`
> is now a real `syn`-based AST pass (was a no-op scaffold) covering hazards **#1** (HashMap/HashSet/
> IndexMap/IndexSet fields on `#[derive(AvaCodec)]` types), **#4** (non-vendored RNG on the sampler +
> consensus crates), **#5** (wall-clock reads — `SystemTime::now`/`Utc::now`/`Local::now`; monotonic
> `Instant::now` is deliberately NOT flagged, see `24` §A refinement), and **#8** (bare-`Tau` second
> arithmetic). Allowlisting is via `xtask/determinism-allowlist.toml` (file+symbol granularity, per-
> site reason) plus pre-existing inline `// determinism-allow: <reason>` comments. Fixture-driven
> red→green tests in `xtask/tests/lint_determinism.rs`. **First workspace-wide run found 3 genuine
> hazard-#5 violations** — `PlatformVm`/`AvmVm`/`EvmVm` `build_block` each stamped block time from
> `SystemTime::now()` — all three fixed the same wave (injected `Arc<dyn Clock>`); the pass is now
> **green workspace-wide and wired into `lint-all`/`lint-all-ci`**. The `Clock`/`RealClock`/`MockClock`
> trait (PART B) was already in `ava-utils`.

> **AS-BUILT (2026-06-16g) — X.19 deferred follow-ups RESOLVED.** The three items left open by
> the 2026-06-16f pass are now done (two parallel-worktree agents + reconciliation):
> - **`clippy::float_arithmetic` + `clippy::arithmetic_side_effects = deny` on the consensus crates**
>   (hazards #2/#3). Applied as crate-level inner attributes in each `src/lib.rs` of `ava-snow`,
>   `ava-engine`, `ava-proposervm`, `ava-validators`, `ava-simplex` (NOT command-line `-- -D` flags —
>   inner attributes scope to the crate and avoid the M7.10 dependency-leak that forced
>   `lint_saevm.sh --no-deps`). ~56 violations triaged: integer sites → `checked_*`/`saturating_*`
>   (snowball confidence counters, `tree.rs` bit-prefix math, Kahn in-degree, `UNIX_EPOCH + Duration`
>   reconstruction from persisted u64 seconds); the legitimate **non-consensus floats** got *targeted*
>   `#[allow(clippy::float_arithmetic)]` with a spec-24-§B.3 justification — the adaptive-timeout
>   averager (`ava-engine/networking/timeout.rs`, module-level), resource-usage tracker
>   (`ava-engine/networking/tracker.rs`), and uptime/connectivity **percentages**
>   (`ava-validators/{connected,uptime/manager}.rs`, mirroring Go's `float64(up)/float64(best)`). No
>   blanket crate-wide float allow (so NEW consensus-path floats still trip). Only `ava-engine` needed
>   `#![cfg_attr(test, allow(...))]` (one `#[cfg(test)]` `UNIX_EPOCH + Duration` helper); the other
>   four crates' test modules were already arithmetic-clean. Verified: `cargo clippy --workspace
>   --all-targets --all-features -- -D warnings` clean (no leakage), 132 consensus-crate tests green.
>   *(Note: this only tightens the two named determinism lints; these three crates still lack a
>   `[lints] workspace = true` opt-in, so the broader workspace restriction lints — `unwrap_used`
>   etc. — are not yet enforced on them. Tracked as a separate, lower-priority follow-up.)*
> - **`determinism_repeat` N≥16 proptest** (§B.6 headline test): `tests/differential/tests/determinism_repeat.rs`
>   — `differential::determinism_repeat` runs the seeded `Program::from_seed`/`LockstepDriver` X-Chain
>   replay **16×** (fresh driver each iteration) and asserts every run's `.normalized()` `Observation`
>   vec is byte-identical to the first; mismatch prints `DIFFERENTIAL_SEED=<seed>`. A companion
>   `determinism_repeat_detects_a_fork` proves the equality check is load-bearing (an injected
>   divergence breaks it), so it can't degenerate into a tautology. Clock injection into the driver was
>   deliberately NOT added (the X-Chain `build_block` wall-clock is already pinned by the harness's
>   fixed `GENESIS_TS`); threading an `Arc<dyn Clock>` seam through `LockstepDriver` remains a separate
>   follow-up.
> - **`.github/PULL_REQUEST_TEMPLATE.md` determinism tick-box checklist**: found already present
>   (the 7-box "Determinism audit" section keyed to spec 24 PART A) — the 2026-06-16f "deferred" note
>   was stale; no change needed.
>
> **STILL DEFERRED (not yet done):** `clippy::cast_possible_truncation = warn` on consensus crates
> (would surface broadly under `-D warnings`; left for a dedicated pass), and the optional cross-triple
> (CI matrix) repeat of `determinism_repeat`.

> **Deepens:** every consensus/codec/VM crate added later is in scope; the PR template checklist applies to any diff touching `ava-codec`/`ava-snow`/`ava-engine`/`ava-proposervm`/`ava-validators`/`ava-*vm`/`ava-utils` (24 §A.2).

---

### Task X.20: PORTING.md matrices + `cargo xtask porting-report` aggregation
**Workstream:** 8 (PORTING.md matrices)  ·  **First lands:** M0  ·  **Depends on:** X.4, pinned Go tree  ·  **Spec:** 02 §10.1
**Files:** `crates/*/tests/PORTING.md`, `scripts/seed_porting_matrix.sh` (`go test -list '.*' ./...`), `xtask/src/porting.rs` (`porting-report`), `.github/workflows/ci.yml` (`porting_report` job, non-blocking publish + `wip`-blocks-done check)
- [ ] **Step 1 — Red/first check:** Seed each M0 crate's `tests/PORTING.md` from `go test -list` of the corresponding Go package; every row starts `wip`. `cargo xtask porting-report` must report a non-100% number and list `wip` rows.
- [ ] **Step 2 — Confirm red:** `cargo xtask porting-report` → "ava-codec: 0% ported (N wip rows)".
- [ ] **Step 3 — Green:** Write `seed_porting_matrix.sh` enumerating Go tests (02 §10.1: `go test -list '.*' ./...`) into each crate's `tests/PORTING.md` table (Go test → Rust counterpart or "N/A — reason" → status `ported`/`wip`/`na`). Implement `xtask porting-report` aggregating all matrices into one report + per-subsystem percent-ported (02 §10.1). Add a `porting_report` CI job that publishes the matrix and asserts a subsystem flagged "done" in its spec has no `wip` rows.
- [ ] **Step 4 — Confirm green:** `cargo xtask porting-report` runs and emits the aggregate matrix; CI publishes it.
- [ ] **Step 5 — Commit:** `test: PORTING.md matrices + xtask porting-report (M0)`

> **Deepens:** updated every milestone; a subsystem is "done" only when its matrix has no `wip` rows and every non-`na` Go test maps to a passing Rust test (02 §10.1, §13.4).

---

### Task X.21: Bench-guard (`criterion`) + TSan/loom jobs (the Go `-race` substitute)
**Workstream:** 6 (CI) / 1 (perf must pass differential)  ·  **First lands:** M1 (first hot path); TSan/loom M5  ·  **Depends on:** X.1, X.13  ·  **Spec:** 02 §9, §5 (loom), §1 (`-race`→TSan)
**Files:** `crates/*/benches/*.rs`, `scripts/bench_guard.sh`, `Taskfile.yml` (`bench`, `bench-guard`, `test-tsan`, `test-loom`), `.github/workflows/ci.yml` (nightly `bench_guard`, `tsan`, `loom` jobs)
- [ ] **Step 1 — Red/first check:** Add a `criterion` bench for a critical path (codec encode/decode) + `bench_guard.sh` comparing to a committed baseline; planting a >10% regression must fail the guard. Add a `loom` test for a lock-free structure that fails under a bad interleaving.
- [ ] **Step 2 — Confirm red:** `./scripts/run_task.sh bench-guard` → ">10% regression vs baseline"; `cargo test --cfg loom` → interleaving assertion failure on the planted bug.
- [ ] **Step 3 — Green:** Add `criterion` benches reusing the dbtest/codec input generators (02 §9), `bench_guard.sh` (`--save-baseline`/compare, default 10% threshold per bench). Add `test-tsan` (`RUSTFLAGS=-Zsanitizer=thread`, nightly — the `-race` substitute, 02 §1) and `test-loom` (`#[cfg(loom)]` exhaustive interleaving for sharded validator set / arc-swap / mpsc shutdown, 02 §5) tasks + nightly CI jobs. State that any 00 §9 perf optimization MUST show a bench win AND pass the differential suite (02 §9).
- [ ] **Step 4 — Confirm green:** `./scripts/run_task.sh bench-guard && ./scripts/run_task.sh test-loom` pass on clean code.
- [ ] **Step 5 — Commit:** `ci: criterion bench-guard + TSan/loom jobs (M1/M5)`

---

### Task X.22: Nightly aggregate job (live two-binary + mixed-net + upgrade + bench-guard + fuzz-long)
**Workstream:** 1/6/7 (nightly tier)  ·  **First lands:** M2 (live); upgrade M8  ·  **Depends on:** X.15, X.16, X.21  ·  **Spec:** 02 §11.7, §10.4, §8
**Files:** `.github/workflows/nightly.yml`, `Taskfile.yml` (`test-upgrade`, `test-differential-live`, `test-load`)
- [ ] **Step 1 — Red/first check:** Create `nightly.yml` (`schedule:` cron) whose `nightly-required` aggregator needs `differential-live`, `mixed-net`, `upgrade`, `bench-guard`, `fuzz-long`. Before wired, the aggregator fails.
- [ ] **Step 2 — Confirm red:** Trigger `workflow_dispatch` → fails: missing jobs/tasks.
- [ ] **Step 3 — Green:** Implement `nightly.yml` per 02 §11.7 (nightly/pre-release: live two-binary with larger `cases`, mixed Go↔Rust interop net, the upgrade suite (02 §10.4: start on prev Go binary → swap to Rust across an activation height, assert continuity + no fork), `bench-guard` (X.21), `test-fuzz-long`). Each job calls a task. Auto-file the seed + observation diff on differential failure and commit to the regression corpus (02 §11.7).
- [ ] **Step 4 — Confirm green:** `nightly.yml` `workflow_dispatch` run is green against locally built binaries.
- [ ] **Step 5 — Commit:** `ci: nightly live two-binary + mixed-net + upgrade + bench-guard + fuzz-long (M2/M8)`

---

### Task X.23: `AGENTS.md` / `CLAUDE.md` + `bazel-vs-cargo` smoke + final `tests-required` consolidation
**Workstream:** 6 (CI) / docs  ·  **First lands:** M0 (docs); smoke M0  ·  **Depends on:** all prior X tasks  ·  **Spec:** 01 §12, §4.5
**Files:** `AGENTS.md`, `CLAUDE.md`, `.github/workflows/ci.yml` (`bazel_vs_cargo` job; final `tests-required.needs` list)
- [x] **Step 1 — Red/first check:** Add a `bazel_vs_cargo` job that builds `avalanchers` both ways and asserts both succeed (01 §4.5). Before wired it fails; also assert `tests-required.needs` lists every gate added across X.1–X.22.
- [x] **Step 2 — Confirm red:** Push → `bazel_vs_cargo` missing / `tests-required` doesn't reference all jobs.
- [x] **Step 3 — Green:** Copy `AGENTS.md` + `CLAUDE.md` verbatim from 01 §12.1/§12.2. Add the `bazel_vs_cargo` smoke job (01 §4.5: `cargo build -p avalanchers --release` and `bazel build //crates/ava-node:avalanchers` both green — proves Cargo/Bazel agree on `Cargo.lock`). Consolidate `tests-required.needs` to include: `Unit`, `Lint`, `taulint`, `check_generated_protobuf`, `check_mocks`, `check_deps_tidy`, `coverage`, `bazel`, `bazel_vs_cargo`, `vectors_verify`, `vectors_drift`, `differential`, `fuzz`, `porting_report`.
- [x] **Step 4 — Confirm green:** `tests-required` green with the full needs list; `bazel_vs_cargo` green.
- [x] **Step 5 — Commit:** `ci: AGENTS/CLAUDE docs + bazel-vs-cargo smoke + consolidate tests-required (M0)`

> **AS-BUILT (2026-06-16, merge of `x23-ci-consolidation`).** X.23 is complete with two
> deliberate, documented scope refinements vs the original step text:
> - **`AGENTS.md` + `CLAUDE.md`** were already committed (2026-06-15); no change needed this wave.
> - **`bazel_vs_cargo` smoke job + Taskfile `bazel-vs-cargo` task** landed (commit `c25f119`). The
>   real Bazel target is **`//crates/avalanchers:avalanchers`** (not the `//crates/ava-node:...`
>   placeholder in the step-3 prose — the binary crate is `crates/avalanchers`). The task runs
>   `cargo build -p avalanchers --release` + `bazelisk build //crates/avalanchers:avalanchers`.
> - **`tests-required.needs` consolidation:** the four gates with *real working backing* were
>   wired and added — **`vectors_verify`** (`vectors-verify`), **`porting_report`**
>   (`porting-report`, non-zero on any `wip` row), **`fuzz`** (`test-fuzz` smoke; the task enters
>   the nightly `fuzz` dev shell via its own `NIX_DEV_SHELL=fuzz`), and **`bazel_vs_cargo`**. The
>   full `needs:` list is now: `Unit`, `Lint`, `taulint`, `check_generated_protobuf`,
>   `check_mocks`, `check_deps_tidy`, `bazel`, `differential`, `vectors_verify`, `porting_report`,
>   `fuzz`, `bazel_vs_cargo`.
> - **`coverage` (X.8) and `vectors_drift` (X.12) were deliberately NOT added** to `needs:`:
>   `scripts/check_coverage_floor.sh` still has an **empty floor table** (scaffold, exits 0) and
>   `scripts/vectors_drift.sh` is a **documented scaffold** (the Go vector-extraction harness was
>   superseded by the recorded-oracle pattern). Wiring them now would gate on no-ops. They remain
>   deepen-later follow-ups: populate the per-crate coverage floor table (X.8), and decide whether
>   to revive the Go-side `vectors_drift` extraction or formally retire it in favour of the
>   recorded-oracle path (X.12). `single_runtime_lint` is also a real, passing gate not yet in
>   `needs:` — a one-line follow-up if a future pass wants it as a hard merge gate.
> Verified by the implementer: `actionlint` + `yamlfmt -lint` clean on `ci.yml`; `vectors-verify`
> and `porting-report` exit 0 on the merged tree; `task --list` shows `bazel-vs-cargo`.

---

## Spec coverage check

| Source | Item | Task(s) |
|---|---|---|
| **16 §4 #1** | Differential harness (recorded M0, live M2; per-subsystem Observation) | X.13, X.14, X.15 |
| **16 §4 #2** | Golden-vector extraction + vectors-drift | X.10, X.11, X.12 |
| **16 §4 #3** | Metrics-name parity golden | X.17 |
| **16 §4 #4** | Error taxonomy (thiserror + assert_matches) | X.9 |
| **16 §4 #5** | Observability (tracing/OTel, M8) | X.18 |
| **16 §4 #6** | Bazel/Nix CI + dirty-tree + TSan/loom | X.1, X.2, X.3, X.5, X.7, X.21 |
| **16 §4 #7** | Fuzzing corpora per parser | X.16 |
| **16 §4 #8** | PORTING.md + porting-report | X.20 |
| **01 §2** | rust-toolchain.toml pinning | X.2 |
| **01 §3** | Nix flake / .envrc / install-nix | X.2 |
| **01 §4** | Bazel bzlmod/rules_rust/crate_universe/gazelle_rust + .bazelrc; §4.5 bazel-vs-cargo | X.3, X.23 |
| **01 §5** | Task runner + run_task.sh + xtask surface; justfile | X.4 |
| **01 §6** | Cargo profiles + .config/nextest.toml | X.8 |
| **01 §7** | rustfmt/clippy/.editorconfig/license/SAE/tau | X.6 |
| **01 §8** | Codegen (protobuf/mocks) dirty-tree gates | X.7 |
| **01 §9** | deny.toml dependency policy | X.5 |
| **01 §10** | ci.yml + tests-required aggregator | X.1, X.23 |
| **01 §12** | AGENTS.md / CLAUDE.md | X.23 |
| **02 §1** | nextest runner + xtask task names | X.4, X.8 |
| **02 §2** | Red→Green TDD methodology | every task's step structure |
| **02 §3** | require mapping / assert_matches | X.9 |
| **02 §4** | proptest mandate + regression corpus | X.11, X.13, X.19 |
| **02 §5** | fixtures + goleak + loom | X.21 |
| **02 §6** | golden/conformance + extraction procedure | X.10, X.11, X.12 |
| **02 §7** | mocking + dbtest battery | (subsystem plans; xtask/CI host them via X.1/X.8) |
| **02 §8** | fuzzing (cargo-fuzz + arbitrary) | X.16 |
| **02 §9** | criterion bench-guard | X.21 |
| **02 §10** | e2e/load/upgrade/reexecute + PORTING | X.15, X.20, X.22, X.13 |
| **02 §11** | differential harness (LockstepDriver/Observation, recorded+live, seed repro) | X.13, X.14, X.15 |
| **02 §12** | coverage floors | X.8 |
| **02 §13** | cross-spec test contract | X.11, X.12, X.13, X.16, X.20 |
| **22 §1–2** | corpus layout + manifest + schema | X.10 |
| **22 §3** | Go extraction harness (vectorgen) | X.10 |
| **22 §4** | provenance + drift detection | X.10, X.12 |
| **22 §5** | static vs live oracle rule | X.11, X.12, X.15 |
| **22 §6** | Rust loader + golden_* + verify | X.11 |
| **22 §7** | vectors-drift / vectors-verify CI | X.12 |
| **22 §8** | category→milestone matrix | X.10 (deepens per milestone) |
| **18 §1–3** | registry model + metric catalog + parity golden | X.17 |
| **18 §4** | process/go_* collector waiver | X.17 |
| **18 §5** | logging level/format model | X.18 |
| **18 §6** | tracing/OpenTelemetry exporter (M8) | X.18 |
| **24 PART A** | determinism audit checklist (9 hazards) | X.19 |
| **24 §A.2** | lint-determinism xtask + clippy + review tiers | X.19 |
| **24 PART B** | injectable Clock (RealClock/MockClock) | X.19 |
| **24 §B.6** | determinism repeat-N proptest + clock-skew tests | X.19 |
| **Buildable-&-green invariant** | build/clippy/nextest + per-PR differential/vectors/fuzz/dirty-tree + nightly | X.1 (defines), X.23 (consolidates), all jobs |
