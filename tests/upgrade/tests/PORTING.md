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

Harness implemented by M9.17 (offline arms green every CI run; live arm gated).

### Layout

- `src/plan.rs` — the swap / import orchestration. `RollingUpgrade::start_on_go`
  models an N-node network on the previous-Go binary; `swap(i, dst_root)` runs the
  REAL M9.16 Go-dir → RocksDB import facade
  (`ava_database::migrate::import::import_source_into_rocksdb`) over an injected
  `GoDbSource` (`GoNodeData`, a §10-layout in-memory source), then **re-opens** the
  imported `v1.4.5/` RocksDB dir and byte-verifies the migrated KV set against the
  source — the load-bearing continuity-of-state check. Rejects double-swap /
  out-of-range as planning bugs.
- `src/continuity.rs` — `assert_no_fork` over `ava_differential::Observation`
  (normalize → per-chain LA id/height + root + sorted validator set; a real
  divergence survives normalization and is reported as a `ForkError`), and
  `MovingFloor` (specs/26 §7), the moving min-compatible floor modelled with the
  real `ava_version::Compatibility` checker + a `MockClock` so the floor can be
  driven across the activation boundary.

### Arms (established M9 offline/live split)

- **Offline arms** (`tests/go_to_rust.rs`, run every CI run, no feature flag):
  `rolling_swap_imports_each_node_byte_identically` (full N-node roll, REAL on-disk
  RocksDB import + byte-verify), `double_swap_and_out_of_range_are_planning_bugs`,
  `no_fork_holds_across_cutover_and_a_divergence_is_caught`,
  `moving_floor_keeps_go_and_rust_peers_connected`.
- **Live arm** `go_to_rust` (`#[cfg(feature = "live")] #[ignore]`): previous-Go
  tmpnet bring-up → advance to pre-activation → per-node Go→Rust swap with data-dir
  import across the activation barrier → continuity/no-fork + moving-floor
  assertions over the *live*-collected `Observation`s. Returns early if
  `$AVALANCHEGO_PATH` is unset; never runs in CI / this sandbox. The bring-up +
  per-node swap + activation barrier is the operator-driven nightly harness (it
  needs the `ava-differential` `Network` two-binary spawner + tmpnet upgrade-schedule
  wiring); the body documents the `LIVE-ARM:` operator handoff inline rather than a
  partial spawn that would rot.

## Go source

- `tests/upgrade/` (specs/02 §10.4) — the Go rolling-upgrade harness.
