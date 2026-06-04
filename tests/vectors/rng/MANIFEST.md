# tests/vectors/rng

Golden raw-stream vectors for the gonum-exact MT19937 RNGs (the R1 gate).
Produced by `tools/extract-vectors` (M0.2); consumed by
`crates/ava-utils/tests/golden_rng.rs` (M0.3, `golden::sampler_mt19937_stream`).
Owning spec: `specs/03-core-primitives.md` §10.4 item 1.

> **Committed** (avalanchego `fb174e8` via gonum `prng`; see `../manifest.json`).

| File | Schema | Notes |
|---|---|---|
| `mt19937_64.json` | `[{ "seed": u64, "stream": [u64, ...] }]` | MT19937-64. Seeds `{0, 1, 5489, 0xDEADBEEF, u64::MAX, 1700000000000000000}`; first **320** `Uint64` each (320 > NN=312 forces a refill). Seed 0 is required. |
| `mt19937_32.json` | `[{ "seed": u64, "stream": [u64, ...] }]` | MT19937 (32-bit), `Uint64` composed high-word-first. Same seed set + length. |
