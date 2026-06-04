# tests/vectors/sampler

Golden vectors for the rejection-sampling wrapper and the three deterministic
samplers. Produced by `tools/extract-vectors` (M0.2). Owning spec:
`specs/03-core-primitives.md` §4.1, §10.4 items 2–3.

> **Committed** (avalanchego `fb174e8`; see `../manifest.json`). Each file is an
> object `{ "_provenance": {...}, "cases": [...] }`.

## `uint64_inclusive.json`

`cases: [{ "seed": u64, "n": u64, "outputs": [u64] }]`. Covers the three
branches: `n=255` (power-of-two mask), `n = MaxUint64-1` (`n > MaxInt64`),
`n=10` (rejection). `outputs` is the first `Uint64Inclusive(n)` draw of a
freshly `seed`-ed MT19937-64 source (reached via `NewDeterministicUniform` with
length `n+1`, draw index 0 — see the extractor comment). Consumed by
`crates/ava-utils/tests/golden_uint64_inclusive.rs` (M0.4).

## `samplers.json`

`cases: [{ "kind": "uniform|weighted|wwr", "seed"?: u64, "weights"?: [u64],
"length"?: u64, "count"?: int, "sample_values"?: [u64],
"sampled_indices": [int] }]`. `uniform`/`wwr` draw from a `seed`-ed MT19937-64
source via the `NewDeterministic*` constructors; `weighted` is rng-free
(`sampled_indices[k]` is the index selected for `sample_values[k]`). Consumed by
`crates/ava-utils/tests/golden_samplers.rs` (M0.10).
