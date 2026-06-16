# ava-load — porting notes

Tracks the Go → Rust port of the **load** suite (M9.18): issue a sustained
C-Chain transfer + X/P tx stream against a tmpnet network, scrape Prometheus
(parity metric names, specs/00 §7.3), and assert throughput/latency SLOs hold
with zero errors (specs/02 §10.3, specs/16 §5 perf).

## Go source

- `tests/load/` (specs/02 §10.3) — the Go load generator + SLO harness.

## Status

Skeleton registered by the M9.18 prep commit; harness implemented by M9.18.
