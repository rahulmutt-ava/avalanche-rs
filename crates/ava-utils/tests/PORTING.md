# PORTING.md — `ava-utils`

Parity against avalanchego `utils/sampler/`, `utils/set/`, `utils/bag/`,
`utils/bits/`, `utils/linked/`, `utils/math/`, `utils/units/`, `utils/formatting/`
(CB58), `utils/timer/` (clock). One row per upstream Go test; status `todo` /
`wip` / `ported` / `na`. No `wip` rows at the M0.25 exit gate.

Owning tasks: M0.3 (RNG — R1 gate), M0.4 (Uint64Inclusive), M0.9 (collections +
safemath + units), M0.10 (samplers), M0.11 (CB58), M0.12 (clock).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `utils/sampler/rand_test.go` (MT19937) | `tests/golden_rng.rs` | todo |
| `utils/sampler/rand_test.go` (Uint64Inclusive) | `tests/golden_uint64_inclusive.rs` | todo |
| `utils/sampler/uniform_test.go` | `tests/golden_samplers.rs` | todo |
| `utils/sampler/weighted_test.go` | `tests/golden_samplers.rs` | todo |
| `utils/sampler/weighted_without_replacement_test.go` | `tests/golden_samplers.rs` | todo |
| `utils/math/safe_math_test.go` | `tests/safemath.rs` | todo |
| `utils/bits/bits_test.go` | `tests/bits.rs` | todo |
| `utils/linked/hashmap_test.go` | `tests/linked.rs` | todo |
| `utils/formatting/cb58_test.go` | `tests/golden_cb58_codec.rs` | todo |
| `utils/timer/mockable/clock_test.go` | `tests/clock_parity.rs` | todo |
