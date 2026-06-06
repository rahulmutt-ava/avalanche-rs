# ava-validators — porting matrix (M3.6 + M3.7 + M3.8)

Tracks the Go `snow/validators` surfaces ported in M3, plus the `snow/uptime`
manager/calculator (M3.8).

| Go source | Rust home | Test(s) | Status |
|---|---|---|---|
| `snow/validators/validator.go::Validator` + `state.go::GetValidatorOutput` | `validator::{Validator, GetValidatorOutput}` | exercised via `set`/`manager` tests | done |
| `snow/validators/set.go::Set` (per-subnet weights, NodeId-sorted) | `set::Set` (`BTreeMap` keyed by `NodeId`) | `set_weight_overflow`, `add_remove_weight_roundtrip`, `subset_weight`, `sorted_snapshot_is_node_id_ascending` | done |
| `snow/validators/set.go::Set.Sample` (deterministic weighted-without-replacement) | `set::Set::sample` (reuses `ava_utils::sampler::WeightedWithoutReplacementGeneric`) | `prop::sample_determinism` | done |
| `snow/validators/manager.go::Manager` | `manager::{ValidatorManager, DefaultManager}` | `manager_tracks_subnets_and_samples` | done |
| `snow/validators/manager.go::SetCallbackListener` | `manager::ManagerCallbackListener` | covered via add/remove notify paths | done |
| `snow/validators/state.go::State` (BTreeMap determinism binding) | `state::ValidatorState` (`#[async_trait]`, returns `BTreeMap<NodeId,_>`) | `validator_set_is_sorted` | done |
| `snow/validators/state.go::{GetCurrentValidatorOutput, WarpSet}` | `state::{GetCurrentValidatorOutput, WarpSet}` | type-checked via `FakeState` | done |
| `snow/validators/state.go::cachedState` | `state_adapters::CachedState` (self-contained bounded LRU) | `cached_state_memoizes_validator_set` | done (LRU is local; see deviation note below) |
| `snow/validators/state.go::lockedState` | `state_adapters::LockedState` (`tokio::sync::Mutex`) | `locked_state_forwards` | done |
| `snow/validators/connector.go::Connector` + connected-stake tracking | `connected::{Connector, ConnectedValidators}` | `connected_tracker_min_percent` | done |
| `snow/uptime/manager.go::Manager` (Tracker + Calculator) | `uptime::manager::{UptimeManager, Calculator}` | `uptime_accumulates_on_connect_disconnect`, `start_tracking_accrues_offline_time`, `connect_disconnect_before_tracking`, `stop_tracking_persists_uptime`, `unrelated_node_disconnect_is_isolated` | done |
| `snow/uptime/manager.go::{CalculateUptimePercent, CalculateUptimePercentFrom}` | `uptime::manager::Calculator::calculate_uptime_percent{,_from}` | `calculate_uptime_percent_non_validator_is_not_found`, `calculate_uptime_percent_div_by_zero_is_one` | done |
| `snow/uptime/state.go::State` (interface) | `uptime::state::UptimeState` (`DbError::NotFound` for non-validators) | exercised via all uptime tests | done |
| `snow/uptime/test_state.go::TestState` | `uptime::state::MemUptimeState` (`Clone`, interior-mutable; `InjectedError` for `dbReadError`/`dbWriteError`) | backing store for all uptime tests | done |
| (no Go equivalent — DB-backed store lives in `vms/platformvm`) | `uptime::state::DbUptimeState` (`Arc<dyn DynDatabase>`; 24-byte BE record) | type-checked | done (deviation note below) |
| `snow/uptime/locked_calculator.go::LockedCalculator` | `uptime::locked::LockedCalculator` (`tokio::sync::Mutex` query lock + std-mutex slot) | `locked_calculator_gates_then_forwards` | done |
| `snow/uptime/no_op_calculator.go::noOpCalculator` | not ported (no consumer yet; trivial to add when one appears) | — | n/a |

## Deviations / notes

- **`Id`/`NodeId` live in `ava-types`, not `ava-snow`.** The M3.6 plan note says
  the crate "re-uses ava-snow Id" — that is incorrect. `ava-validators` depends only
  on `ava-types` / `ava-crypto` / `ava-utils` and does NOT depend on `ava-snow`.
- **LRU is local.** `specs/06` §6.1 says "LRU via ava-utils", but `ava-utils` has no
  cache module yet. `CachedState` carries a minimal insertion-order LRU to avoid a
  new dependency / coupling to an unbuilt crate. Swap to an `ava-utils::lru` when one
  exists.
- **Poll sampling RNG.** `ValidatorManager::sample` is non-deterministic (seeds the
  M0 `Mt19937_64` from coarse OS entropy) because *which* validators are asked does
  not affect *which* block is decided (`specs/06` §6.2). The windower path instead
  calls `Set::sample` with its own seeded gonum MT source — the deterministic stream.

### Uptime (M3.8, `specs/06` §6.3)

- **Off the determinism-critical path.** Uptime feeds reward accounting, not block
  decisions, so it reads wall time via the injected `ava_utils::clock::Clock`
  (`Arc<dyn Clock>`; tests use `MockClock` + `.advance`) and uses float division in
  `calculate_uptime_percent*`, mirroring Go directly.
- **State split mirrors Go.** Go's `snow/uptime` ships only the `State` *interface*
  + a `TestState`; the concrete DB store lives in `vms/platformvm`. We ported the
  trait (`UptimeState`), the in-memory port (`MemUptimeState` = `TestState`), and a
  new `DbUptimeState` backed by `Arc<dyn DynDatabase>` (24-byte big-endian record:
  `up_secs ‖ last_updated_secs ‖ start_secs`, keyed by the 20-byte node id). A
  malformed/short record reads back as `NotFound` (best-effort degradation; this is
  off the consensus path). This adds `ava-database` as a new `ava-validators`
  dependency — the crate doc was updated accordingly.
- **`database.ErrNotFound` contract.** `UptimeState` returns `ava_database::Error`,
  so non-validators surface as `Error::Database(DbError::NotFound)`; `update_uptime`
  swallows that (Go "we don't track the uptimes of non-validators"), and
  `Error::as_db_error()` lets callers match the sentinel (Rust analog of
  `errors.Is`).
- **`InjectedError` instead of arbitrary `dbReadError`/`dbWriteError`.** `MemUptimeState`
  exposes only the two cloneable DB sentinels (`Closed`/`NotFound`) for failure-path
  injection, because reconstructing `DbError::Other` needs `anyhow`, which is barred
  from library code. The realistic backend failure modes are covered.
- **`LockedCalculator`.** Ported with a `tokio::sync::Mutex<()>` serializing the inner
  query (Go's `calculatorLock`) and a `std::sync::Mutex<Option<Arc<dyn Calculator>>>`
  for the swappable slot (Go's `RWMutex` + bootstrap `Atomic[bool]`). The `None` slot
  encodes "still bootstrapping". `set_calculator`/`clear` are sync (slot-only);
  queries are `async`. `no_op_calculator.go` is not ported (no consumer yet).
