# ava-upgrade — porting notes

Tracks the Go → Rust port of the **upgrade** suite (M9.17): start a tmpnet on the
previous released Go binary, advance to just before an activation height, replace
nodes one-by-one with the Rust binary across the activation height (importing each
node's Go data dir → RocksDB, M9.16), and assert chain continuity / no fork while
the moving min-compatible floor (specs/26 §7) keeps Go and Rust peers connected
(specs/02 §10.4, specs/16 §5(8), specs/00 §4.4).

## Go source

- `tests/upgrade/` (specs/02 §10.4) — the Go rolling-upgrade harness.

## Status

Skeleton registered by the M9.17 prep commit; harness implemented by M9.17.
