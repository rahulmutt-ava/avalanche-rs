# ava-validators — porting matrix (M3.6 + M3.7)

Tracks the Go `snow/validators` surfaces ported in M3. The uptime manager
(`snow/uptime/`, M3.8) is a later task and is NOT covered here.

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
| `snow/uptime/manager.go` | `uptime::Manager` | — | wip (M3.8) |

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
