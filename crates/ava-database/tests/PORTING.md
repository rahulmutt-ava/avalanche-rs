# PORTING.md — `ava-database`

Tracks parity of this crate against its avalanchego source packages under
`database/` (`database`, `database/dbtest`, `database/corruptabledb`,
`database/leveldb`, `database/pebbledb`, `database/memdb`, `database/meterdb`,
`database/prefixdb`, `database/versiondb`, `database/linkeddb`,
`database/rpcdb`, `database/heightindexdb/...`). One row per upstream Go test;
status is one of `todo` / `wip` / `ported` / `na`. The milestone exit gate
(M1.26) requires no `wip` rows for shipped surfaces. See
`specs/02-testing-strategy.md` §10.1.

Seeded from `go test -list '.*'` over `./database/...` at avalanchego rev
`fb174e8925`.

Owning tasks: M1.1 (trait + errors + helpers), M1.2 (dbtest battery + BTreeMap
oracle), M1.3 (memdb), M1.4 (rocksdb), M1.5 (prefixdb), M1.6 (versiondb),
M1.7 (meterdb), M1.8 (corruptabledb), M1.9 (linkeddb), M1.10 (heightindexdb),
M1.11 (rpcdb), M1.24 (R2 Go-data-dir import tool).

## Top-level `database/` helpers + iterator utilities

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `database` `TestSortednessUint64` | `src/helpers.rs` (`unit::helpers_byte_exact`); sorted iteration covered by `dbtest::iterator*` | ported |
| `database` `TestSortednessUint32` | `src/helpers.rs` (`unit::helpers_byte_exact`) | ported |
| `database` `TestOrDefault` | `src/helpers.rs` `with_default` (`unit::helpers_byte_exact`) | ported |
| `database/errors.go` (sentinel strings) | `tests/sentinels.rs` / `unit::error_variants` (`Error::NotFound`/`Closed` `#[error]` strings) | ported |

## `database/dbtest` conformance battery (the shared `Database` suite)

Each Go `dbtest.Test*` helper maps to a private fn inside
`src/dbtest.rs::run_database_suite`, driven by every backend's
`conformance::run_database_suite`. Status here = "the suite covers it";
per-backend pass is in the backend rows below.

| Go source (test) | Rust counterpart (`src/dbtest.rs`) | Status |
|---|---|---|
| `TestSimpleKeyValue` | `simple_key_value` | ported |
| `TestSimpleKeyValueClosed` | `simple_key_value_closed` | ported |
| `TestOverwriteKeyValue` | `overwrite_key_value` | ported |
| `TestKeyEmptyValue` | `key_empty_value` (nil⇔empty, 04 §1.1) | ported |
| `TestEmptyKey` | `empty_key` | ported |
| `TestPutGetEmpty` | `put_get_empty` | ported |
| `TestMemorySafetyDatabase` | `memory_safety_database` | ported |
| `TestMemorySafetyBatch` | `memory_safety_batch` | ported |
| `TestModifyValueAfterPut` | `modify_value_after_put` | ported |
| `TestModifyValueAfterBatchPut` | `modify_value_after_batch_put` | ported |
| `TestModifyValueAfterBatchPutReplay` | `modify_value_after_batch_put_replay` | ported |
| `TestNewBatchClosed` | `new_batch_closed` | ported |
| `TestBatchPut` | `batch_put` | ported |
| `TestBatchDelete` | `batch_delete` | ported |
| `TestBatchReset` | `batch_reset` | ported |
| `TestBatchReuse` | `batch_reuse` | ported |
| `TestBatchRewrite` | `batch_rewrite` | ported |
| `TestBatchReplay` | `batch_replay` | ported |
| `TestBatchReplayPropagateError` | `batch_replay_propagate_error` | ported |
| `TestBatchInner` | `batch_inner` | ported |
| `TestBatchLargeSize` | `batch_large_size` | ported |
| `TestIterator` | `iterator` | ported |
| `TestIteratorStart` | `iterator_start` | ported |
| `TestIteratorPrefix` | `iterator_prefix` | ported |
| `TestIteratorStartPrefix` | `iterator_start_prefix` | ported |
| `TestIteratorMemorySafety` | `iterator_memory_safety` | ported |
| `TestIteratorClosed` | `iterator_closed` | ported |
| `TestIteratorError` | `iterator_error` | ported |
| `TestIteratorErrorAfterRelease` | `iterator_error_after_release` | ported |
| `TestIteratorSnapshot` | `iterator_snapshot` | ported |
| `TestCompactNoPanic` | `compact_no_panic` | ported |
| `TestClear` | `clear` | ported |
| `TestClearPrefix` | `clear_prefix` | ported |
| `TestAtomicClear` | `atomic_clear` | ported |
| `TestAtomicClearPrefix` | `atomic_clear_prefix` | ported |
| `TestConcurrentBatches` | `concurrent_batches` | ported |
| `TestManySmallConcurrentKVPairBatches` | `many_small_concurrent_kv_batches` | ported |

## `database/memdb` (M1.3)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `memdb` `TestInterface` | `tests/conformance_memdb.rs` `conformance::run_database_suite` | ported |
| `memdb` `FuzzKeyValue` | `tests/conformance_memdb.rs` `prop::db_oracle_btreemap` | ported |
| `memdb` `FuzzNewIteratorWithPrefix` | `prop::db_oracle_btreemap` (Iterate ops incl. prefix) | ported |
| `memdb` `FuzzNewIteratorWithStartAndPrefix` | `prop::db_oracle_btreemap` (Iterate ops incl. start+prefix) | ported |
| `memdb` `BenchmarkInterface` | n/a (benchmark) | na — perf bench, not a parity assertion |

## `database/leveldb`

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `leveldb` `TestInterface` | `tests/conformance_rocksdb.rs` `conformance::run_database_suite` | na — leveldb backend replaced by rocksdb on-disk default (00 §4.4); same conformance battery |
| `leveldb` `FuzzKeyValue` / `FuzzNewIteratorWith*` | `tests/conformance_rocksdb.rs` `prop::db_oracle_btreemap` | na — leveldb→rocksdb (00 §4.4) |
| `leveldb` `BenchmarkInterface` | n/a (benchmark) | na — perf bench |

## `database/pebbledb`

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `pebbledb` `TestInterface` | `tests/conformance_rocksdb.rs` `conformance::run_database_suite` | na — pebbledb backend replaced by rocksdb on-disk default (00 §4.4) |
| `pebbledb` `TestBatch` | `dbtest::batch_*` via rocksdb conformance | na — pebble-specific; rocksdb `WriteBatch` covered by battery |
| `pebbledb` `TestKeyRange` | rocksdb prefix/range iteration in `dbtest::iterator_prefix`/`iterator_start` | na — pebble-specific; range semantics covered by battery on rocksdb |
| `pebbledb` `FuzzKeyValue` / `FuzzNewIteratorWith*` | `tests/conformance_rocksdb.rs` `prop::db_oracle_btreemap` | na — pebble→rocksdb (00 §4.4) |
| `pebbledb` `BenchmarkInterface` | n/a (benchmark) | na — perf bench |

## `database/rocksdb` (M1.4 — Rust on-disk default; no Go counterpart pkg)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| (replaces leveldb/pebbledb `TestInterface`) | `tests/conformance_rocksdb.rs` `conformance::run_database_suite` | ported |
| (replaces leveldb/pebbledb fuzz) | `tests/conformance_rocksdb.rs` `prop::db_oracle_btreemap` | ported |

## `database/prefixdb` (M1.5)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `prefixdb` `TestInterface` | `tests/conformance_prefixdb.rs` `conformance::run_database_suite` | ported |
| `prefixdb` `TestPrefixLimit` | range-bounded `compact` (`dbLimit = increment(prefix)`); exercised via `dbtest::compact_no_panic` over `PrefixDb` | ported |
| `prefixdb` (MakePrefix/JoinPrefixes SHA-256 namespacing) | `tests/golden_prefix.rs` `golden::prefix_namespacing` (Go-extracted SHA-256 vector, 04 §10.1) | ported |
| `prefixdb` `FuzzKeyValue` / `FuzzNewIteratorWith*` | `tests/conformance_prefixdb.rs` `prop::db_oracle_btreemap` | ported |
| `prefixdb` `BenchmarkInterface` | n/a (benchmark) | na — perf bench |

## `database/versiondb` (M1.6)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `versiondb` `TestInterface` | `tests/conformance_versiondb.rs` `conformance::run_database_suite` | ported |
| `versiondb` `TestIterate` | merge-iterator covered by `dbtest::iterator*` over `VersionDb` | ported |
| `versiondb` `TestCommit` | write-through after commit + merge-iterator (src `versiondb.rs` `#[cfg(test)]`) | ported |
| `versiondb` `TestCommitClosed` | `dbtest::*_closed` over `VersionDb` (post-close → `Error::Closed`) | ported |
| `versiondb` `TestCommitClosedWrite` | `dbtest::simple_key_value_closed` (write path) over `VersionDb` | ported |
| `versiondb` `TestCommitClosedDelete` | `dbtest::*_closed` (delete path) over `VersionDb` | ported |
| `versiondb` `TestAbort` | overlay-discard without `write()` (src `versiondb.rs` `#[cfg(test)]`) | ported |
| `versiondb` `TestCommitBatch` | `commit_batch()` → `VersionCommitBatch` buffered ops (src `versiondb.rs` `#[cfg(test)]`) | ported |
| `versiondb` `TestSetDatabase` | base-swap path | na — Rust `VersionDb<D>` owns a typed base; Go's runtime interface-swap not expressible under generics and unused by the storage tier (04 §2.4) |
| `versiondb` `TestSetDatabaseClosed` | (see above) | na — no runtime SetDatabase |
| `versiondb` `FuzzKeyValue` / `FuzzNewIteratorWith*` | `tests/conformance_versiondb.rs` `prop::db_oracle_btreemap` | ported |
| `versiondb` `BenchmarkInterface` | n/a (benchmark) | na — perf bench |

## `database/meterdb` (M1.7)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `meterdb` `TestInterface` | `tests/conformance_meterdb.rs` `conformance::run_database_suite` | ported |
| `meterdb` (metric-name set) | `tests/golden_meterdb_metrics.rs` `golden::meterdb_metric_names` (21-label Go-extracted vector, 04 §2.5) | ported |
| `meterdb` `FuzzKeyValue` / `FuzzNewIteratorWith*` | `tests/conformance_meterdb.rs` `prop::db_oracle_btreemap` | ported |
| `meterdb` `BenchmarkInterface` | n/a (benchmark) | na — perf bench |

## `database/corruptabledb` (M1.8)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `corruptabledb` `TestInterface` | `tests/conformance_corruptabledb.rs` `conformance::run_database_suite` | ported |
| `corruptabledb` `TestCorruption` | poison-on-`Other` latch (src `corruptabledb.rs` `#[cfg(test)]`); post-corruption ops fail (27 §6.1) | ported |
| `corruptabledb` `TestIterator` | iterator-error latches corruption (src `corruptabledb.rs` `#[cfg(test)]`) | ported |
| `corruptabledb` `FuzzKeyValue` | `tests/conformance_corruptabledb.rs` `prop::db_oracle_btreemap` | ported |
| `corruptabledb` `FuzzNewIteratorWith*` | `prop::db_oracle_btreemap` (Iterate ops) | ported |

## `database/linkeddb` (M1.9)

`LinkedDb` is a reader/writer/deleter + LIFO iterator (not a full `Database`),
so it does not run the shared conformance battery.

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `linkeddb` `TestInterface` | (LinkedDb is not a full `Database`) | na — Go's LinkedDB satisfies `Database`; Rust `LinkedDb` is reader/writer/deleter + LIFO only (04 §10.6) |
| `linkeddb` (node codec) | `tests/golden_linkeddb.rs` `golden::linkeddb_node_codec` (Go-extracted node-byte vector, 04 §10.6) | ported |
| `linkeddb` `TestLinkedDB` | src `linkeddb.rs` `#[cfg(test)]` put/get/delete + head relinking | ported |
| `linkeddb` `TestLinkedDBDuplicatedPut` | src `linkeddb.rs` `#[cfg(test)]` duplicate-put idempotence | ported |
| `linkeddb` `TestLinkedDBMultiplePuts` | src `linkeddb.rs` `#[cfg(test)]` multi-put ordering | ported |
| `linkeddb` `TestEmptyLinkedDBIterator` | src `linkeddb.rs` `#[cfg(test)]` empty iterator | ported |
| `linkeddb` `TestLinkedDBLoadHeadKey` | src `linkeddb.rs` `#[cfg(test)]` head-key load + LRU cache | ported |
| `linkeddb` `TestSingleLinkedDBIterator` | src `linkeddb.rs` `#[cfg(test)]` single-node LIFO iterator | ported |
| `linkeddb` `TestMultipleLinkedDBIterator` | src `linkeddb.rs` `#[cfg(test)]` multi-node LIFO iterator | ported |
| `linkeddb` `TestMultipleLinkedDBIteratorStart` | src `linkeddb.rs` `#[cfg(test)]` iterator-with-start | ported |
| `linkeddb` `TestSingleLinkedDBIteratorStart` | src `linkeddb.rs` `#[cfg(test)]` single-node iterator-with-start | ported |
| `linkeddb` `TestEmptyLinkedDBIteratorStart` | src `linkeddb.rs` `#[cfg(test)]` empty iterator-with-start | ported |
| `linkeddb` `TestLinkedDBIsEmpty` | src `linkeddb.rs` `#[cfg(test)]` is-empty | ported |
| `linkeddb` `TestLinkedDBHeadKey` | src `linkeddb.rs` `#[cfg(test)]` head-key accessor | ported |
| `linkeddb` `TestLinkedDBHead` | src `linkeddb.rs` `#[cfg(test)]` head-value accessor | ported |
| `linkeddb` `FuzzKeyValue` | src `linkeddb.rs` `#[cfg(test)]` put/get round-trips | ported |

## `database/rpcdb` (M1.11)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `rpcdb` `TestInterface` | `tests/conformance_rpcdb.rs` `rpcdb_conformance::run_database_suite_rpcdb` (in-process loopback tonic channel) | ported |
| `rpcdb` `TestHealthCheck` | `health_check` over the RPC surface (src `rpcdb/*.rs` `#[cfg(test)]`) + battery `health_check` coverage | ported |
| `rpcdb` `FuzzKeyValue` / `FuzzNewIteratorWith*` | `tests/conformance_rpcdb.rs` `rpcdb_conformance::db_oracle_btreemap_rpcdb` | ported |
| `rpcdb` `BenchmarkInterface` | n/a (benchmark) | na — perf bench |

## `database/heightindexdb/...` (M1.10)

`HeightIndex` is a height-keyed store with its own battery (`run_heightindex_suite`).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `heightindexdb/dbtest` `TestPutGet` | `src/dbtest.rs` `hi_put_get` (in `run_heightindex_suite`) | ported |
| `heightindexdb/dbtest` `TestHas` | `src/dbtest.rs` `hi_*` Has coverage | ported |
| `heightindexdb/dbtest` `TestSync` | `src/dbtest.rs` `hi_*` Sync coverage | ported |
| `heightindexdb/dbtest` `TestClose` | `src/dbtest.rs` `hi_close` | ported |
| `heightindexdb/dbtest` `TestCloseAndGet` | `src/dbtest.rs` `hi_close_and_get` | ported |
| `heightindexdb/dbtest` `TestCloseAndHas` | `src/dbtest.rs` `hi_close_and_has` | ported |
| `heightindexdb/dbtest` `TestCloseAndPut` | `src/dbtest.rs` `hi_close_and_put` | ported |
| `heightindexdb/dbtest` `TestCloseAndSync` | `src/dbtest.rs` `hi_close_and_sync` | ported |
| `heightindexdb/memdb` `TestInterface` | `tests/conformance_heightindex.rs` `conformance::run_heightindex_suite_memdb` | ported |
| `heightindexdb/meterdb` `TestPutGet`/`TestHas`/`TestClose` | `tests/conformance_heightindex.rs` `conformance::run_heightindex_suite_meterdb` | ported |

## `database/databasemock`, `database/factory`

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `database/databasemock` | mockall-based local mocks where needed | na — generated mock package; Rust uses `mockall` per crate (no committed mock pkg) |
| `database/factory` | backend selection wired in node assembly (M-node) | na — DB-config→backend factory deferred to `ava-node` / `ava-config` |

## Rust-side additions — R2 Go-data-dir import tool (M1.24)

No Go test counterpart (this is the Rust migration tool, 04 §11 / 00 R2).

| Go source (test) | Rust counterpart (`tests/migrate.rs`) | Status |
|---|---|---|
| — (R2 import driver, byte-preserving) | `unit::migrate_preserves_bytes` | ported |
| — (R2 resumable import + cursor) | `unit::migrate_resumable` | ported |
| — (R2 `--verify roots` mismatch detection) | `unit::verify_roots_detects_mismatch` | ported |
| — (R2 `--verify none` no-op) | `unit::verify_none_is_noop` | ported |

## Notes / deviations

- **leveldb + pebbledb backends are deliberately not ported** (`na`): per 00
  §4.4 the on-disk default is **rocksdb**. Their `TestInterface`/fuzz coverage
  is reproduced by the identical `conformance::run_database_suite` +
  `prop::db_oracle_btreemap` over `RocksDb` (`tests/conformance_rocksdb.rs`).
- **`Benchmark*` rows are `na`** everywhere — they are perf benchmarks, not
  parity assertions (02 §10.1 tracks correctness tests).
- **`versiondb` `TestSetDatabase`/`TestSetDatabaseClosed` are `na`:** Rust
  `VersionDb<D>` owns a typed base; Go's runtime interface-swap is not
  expressible under generics and is unused by the storage tier (04 §2.4).
- **`linkeddb` `TestInterface` is `na`:** Go's LinkedDB implements the full
  `Database` interface; the Rust `LinkedDb` is intentionally a
  reader/writer/deleter + LIFO iterator only (04 §10.6), so it does not run the
  shared conformance battery. Its behavioral tests live in
  `src/linkeddb.rs` `#[cfg(test)]` + the node-codec golden.
- **`database/databasemock` / `database/factory` are `na`:** the mock package
  is generated (Rust uses `mockall` locally) and backend-selection lives in
  node assembly (`ava-node`/`ava-config`), not this crate.
- **dbtest battery rows** map to private fns inside
  `src/dbtest.rs::run_database_suite`; each backend's
  `conformance::run_database_suite` row above asserts the full battery passes
  for that backend.
