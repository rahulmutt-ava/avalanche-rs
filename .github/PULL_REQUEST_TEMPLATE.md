<!-- specs/01 §12, specs/24 §A — keep this template in sync with the Go repo's. -->

## Problem

<!-- What is broken / missing? Link the issue. -->

## Solution

<!-- What does this PR change, and why this approach? -->

## Determinism audit (required for any diff touching consensus/codec/VM crates)

Tick every box that applies; consensus paths are `ava-codec`, `ava-snow`,
`ava-engine`, `ava-proposervm`, `ava-validators`, `ava-*vm`, `ava-utils`
(specs/24 PART A, §A.2):

- [ ] No `HashMap`/`HashSet`/`IndexMap` on a serialized/consensus path (sort keys / `BTreeMap`).
- [ ] No floating-point in codec/consensus code.
- [ ] All arithmetic on protocol paths is `checked_*`/`saturating_*` (no silent wrap).
- [ ] No new consensus RNG outside the vendored `ava-utils` MT19937 source.
- [ ] No wall-clock (`SystemTime::now`/`Instant::now`) outside `ava-utils::clock` + bin wiring.
- [ ] No raw `as` cast or unchecked `Duration` math on a `Tau` quantity (use `params::TAU`).
- [ ] `cargo xtask lint-determinism` reports zero findings.

## Checklist

- [ ] `./scripts/run_task.sh lint-all` is green.
- [ ] `./scripts/run_task.sh test-unit` is green.
- [ ] Touched deps? ran `deps-tidy` and committed `Cargo.lock` + `MODULE.bazel.lock`.
- [ ] Touched `.rs` affecting Bazel? ran `bazel-check-metadata` and committed `BUILD.bazel`.
- [ ] Updated each touched crate's `tests/PORTING.md`.
