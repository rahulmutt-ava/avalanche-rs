# tests/vectors/upgrade

Golden fork-activation vectors. Produced by `tools/extract-vectors` (M0.2);
consumed by `crates/ava-version/tests/golden_upgrade.rs` (M0.23,
`golden::upgrade_activation`). Owning spec: `specs/03-core-primitives.md` §11.3.

> **Committed** (avalanchego `fb174e8` via `upgrade.GetConfig`; see
> `../manifest.json`). Object `{ "_provenance": {...}, "cases": [...] }`.

```json
{ "cases": [
  { "network": "mainnet | fuji",
    "fork": "apricot_phase_1 | ... | granite | helicon",
    "fork_time_rfc3339_nano": "..",
    "samples": [
      { "at_rfc3339_nano": "<forkTime-1ns>", "is_active": false },
      { "at_rfc3339_nano": "<forkTime>",     "is_active": true  },
      { "at_rfc3339_nano": "<forkTime+1ns>", "is_active": true  }
    ] } ] }
```

`is_active(fork, t)` is `t >= fork_time` (inclusive at the boundary). Covers
`networkID ∈ {Mainnet, Fuji}` for all **15** time-gated forks.

> **Spec note:** the live avalanchego config (`fb174e8`) includes a `helicon`
> fork beyond what `specs/03` §11.2 lists. `ava-version` (M0.23) should mirror
> the forks present in these vectors; update `specs/03` §11.2 accordingly.
