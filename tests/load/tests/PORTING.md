# ava-load — porting notes

Tracks the Go → Rust port of the **load** suite (M9.18): issue a sustained
C-Chain transfer + X/P tx stream against a tmpnet network, scrape Prometheus
(parity metric names, specs/00 §7.3), and assert throughput/latency SLOs hold
with zero errors (specs/02 §10.3, specs/16 §5 perf).

## Go source

- `tests/load/` (specs/02 §10.3) — the Go load generator + SLO harness.

## Status — M9.18 implemented

Two-arm shape, matching every M9 task (CI-runnable offline arm + gated live arm).

### Offline arms (run every CI run, no feature, not `#[ignore]`)

- `src/generator.rs` — deterministic, seed-derived load stream + integer rate
  pacing:
  - `LoadGenerator` produces a reproducible stream of `TxDescriptor`s (C/X/P
    round-robin; `from`/`to`/`amount` derived via splitmix64 — no RNG crate, no
    floats). `descriptor_at(i)` is a pure function of `(seed, accounts, i)`.
  - `PacingSchedule` is the rate math: `total_count = floor(rate * duration)`,
    `interval = 1s / rate`, `deadline_of(i)` saturating at the run duration. All
    arithmetic is `checked_*`/`saturating_*` (no panics, no floats) — exercised
    by a hostile-extremes test.
  - Tests: `tests/generator_offline.rs` (6 tests) — same-seed byte-identical /
    distinct-seed-differs / well-formed descriptors / 2-account no-self-transfer
    / exact pacing math / never-panics-on-extremes.
- `src/metrics.rs` — Prometheus text-format parser + pure SLO logic:
  - `Exposition::parse` (handles `# HELP`/`# TYPE`, blank lines, quoted labels
    with embedded commas, `+Inf`/`-Inf`/`NaN`), `has_metric`/`sample`/`sum`/
    `missing_parity_names`.
  - `slo_holds` / `slo_violations` — pure verdict over `SloMeasurement` vs
    `SloThresholds` (throughput ≥ min, latency ≤ max, **errors == 0**).
  - `REQUIRED_PARITY_METRICS` (in `lib.rs`) — the parity-critical
    `avalanche_<subsystem>_<name>` families from specs/00 §7.3 / specs/18 §2.
  - Fixtures: `tests/fixtures/ext_metrics_{good,regressed}.prom` — representative
    `/ext/metrics` scrapes using parity names. The good one passes the SLOs; the
    regressed one keeps every parity name (so the *naming* check still passes)
    but fails throughput + latency + has non-zero parse-failure errors.
  - Tests: `tests/metrics_offline.rs` (5 tests) + `tests/sustained_load.rs`
    `sustained_load_pipeline_offline` (ties generator + pacing + parse + parity +
    SLO together end-to-end over the fixture).

12 offline tests total; all green; clippy `-D warnings` clean (checked
arithmetic throughout, no raw indexing).

### Live arm (`#[cfg(feature = "live")]` + `#[ignore]`)

`tests/sustained_load.rs::sustained_load` — boots one `avalanchers` node
(`src/network.rs::LoadNode`, modeled on `tests/differential/src/network.rs`),
runs the generator for `--load-timeout`, scrapes `/ext/metrics` over a
hand-rolled HTTP/1.1 GET on `tokio::net::TcpStream` (no HTTP-client crate —
specs/00 §4), and asserts parity names + SLOs + zero errors. Returns early if no
`avalanchers` binary (`$AVALANCHERS_PATH` or `target/{release,debug}/avalanchers`).
Never runs in CI / this sandbox. Run it via:

```sh
cargo nextest run -p ava-load --features live -- --ignored
# or: cargo xtask test-load -- --load-timeout=30s   (offline arms only in CI)
```

`--load-timeout` is read from `$AVA_LOAD_TIMEOUT_SECS` (default 30s).

## Deferrals / operator handoff (honest gaps)

- **Tx signing/issuance.** The live arm runs the deterministic generator and
  proves the scrape→parse→SLO pipeline against the live `/ext/metrics`, but does
  **not** sign+issue each `TxDescriptor`. Turning a descriptor into a signed,
  issued tx needs `ava-wallet` keyed off the genesis pre-funded allocation. That
  wiring is the nightly operator's remaining step; `ava-wallet` is deliberately
  **not** a dependency of this crate so the offline CI build stays light and the
  `unused_crate_dependencies` lint is honest (a dep referenced only in deferred
  live code would be dead weight). Marked inline as `LIVE-ARM:`.
- **Single-node genesis + key allocation.** `LoadNode::start` sketches
  `--network-id=local`; the operator supplies the genesis + pre-funded keys the
  generator's `from`/`to` account indices map onto — the same deferral the
  differential harness's `spawn_node` carries.
- **SLO thresholds** (200 tx/s, 2000 ms, 0 errors) are placeholder defaults; a
  nightly operator tunes them to the target hardware / network size.
