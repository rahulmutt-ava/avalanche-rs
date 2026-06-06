# PORTING.md — `ava-blockdb`

Tracks parity of this crate against avalanchego package `x/blockdb`
(append-optimized, height-indexed block store). One row per upstream Go test;
status is one of `todo` / `wip` / `ported` / `na`. The milestone exit gate
(M1.26) requires no `wip` rows for shipped surfaces. See
`specs/02-testing-strategy.md` §10.1.

Seeded from `go test -list '.*'` over `./x/blockdb/` at avalanchego rev
`fb174e8925`.

Owning task: M1.22 (blockdb file format + recovery + `prop::blockdb_roundtrip`).

## On-disk format + sizing

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestStructSizes` | `tests/golden_format.rs` `golden::blockdb_header_layout` (little-endian header layout, 04 §5.1) | ported |
| `TestIndexFileHeaderAlignment` | `tests/golden_format.rs` `golden::blockdb_header_layout` (index-file header alignment) | ported |
| `TestIndexEntrySizePowerOfTwo` | `tests/golden_format.rs` `golden::blockdb_header_layout` (index-entry size invariant) | ported |
| (checksum = XXH64 seed 0 over uncompressed bytes) | `tests/golden_format.rs` `golden::blockdb_checksum_is_xxhash` (04 §5.1) | ported |

## Construction + config

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestNew_Params` | `src/*.rs` `#[cfg(test)]` constructor params | ported |
| `TestNew_IndexFileErrors` | `tests/recovery.rs` `recovery_errors_on_missing_data_file` + `#[cfg(test)]` index-file error paths | ported |
| `TestNew_IndexFileConfigPrecedence` | `src/*.rs` `#[cfg(test)]` config precedence | ported |

## Put / Get / Has

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestPutGet` | `tests/prop_roundtrip.rs` `blockdb_roundtrip` + `#[cfg(test)]` put/get | ported |
| `TestPut_MaxHeight` | `src/*.rs` `#[cfg(test)]` max-height put | ported |
| `TestHasBlock` | `src/*.rs` `#[cfg(test)]` has-block | ported |
| `TestReadOperations` | `tests/prop_roundtrip.rs` `blockdb_roundtrip` + `#[cfg(test)]` reads | ported |
| `TestWriteBlock_Errors` | `src/*.rs` `#[cfg(test)]` write-block error paths | ported |

## Data-file splitting

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestDataSplitting` | `src/*.rs` `#[cfg(test)]` multi-data-file splitting | ported |
| `TestDataSplitting_DeletedFile` | `src/*.rs` `#[cfg(test)]` split with deleted file | ported |
| `TestMaxDataFiles_CacheLimit` | `src/*.rs` `#[cfg(test)]` max-data-files cache limit | ported |
| `TestFileCache_Eviction` | `src/*.rs` `#[cfg(test)]` LRU file-cache eviction (`lru` crate) | ported |

## Recovery + sync persistence

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestRecovery_Success` | `tests/recovery.rs` `recovery_rebuilds_index` (torn-write recovery scan, 27 §5.1) | ported |
| `TestRecovery_CorruptionDetection` | `tests/recovery.rs` `recovery_errors_on_missing_data_file` + checksum corruption detection | ported |
| `TestSyncPersistence` | `src/*.rs` `#[cfg(test)]` sync/flush persistence | ported |

## Block cache

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestCacheOnMiss` | `src/*.rs` `#[cfg(test)]` cache miss → fetch | ported |
| `TestCacheGet` | `src/*.rs` `#[cfg(test)]` cache get | ported |
| `TestCacheHas` | `src/*.rs` `#[cfg(test)]` cache has | ported |
| `TestCachePutOverridesSameHeight` | `src/*.rs` `#[cfg(test)]` same-height override | ported |
| `TestCacheClose` | `src/*.rs` `#[cfg(test)]` cache close | ported |

## Concurrency + interface

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestInterface` | `tests/prop_roundtrip.rs` `blockdb_roundtrip` (full put/get/has interface) | ported |
| `TestReadOperations_Concurrency` | `src/*.rs` `#[cfg(test)]` concurrent reads | ported |
| `TestWriteBlock_Concurrency` | `src/*.rs` `#[cfg(test)]` concurrent writes | ported |

## Notes / deviations

- All on-disk headers are **little-endian**; the index-entry/block-entry `size`
  field stores the **compressed** length while the XXH64 (seed 0) checksum is
  over the **uncompressed** bytes (04 §5.1). Cross-implementation byte-replay of
  *compressed* `.dat` payloads is not asserted (each side owns its zstd encoder;
  the checksum is over uncompressed data).
- Behavioral `Test*` rows that have no dedicated integration test in `tests/`
  are covered by `#[cfg(test)]` unit modules in `src/` plus the
  `blockdb_roundtrip` proptest; the three exit-relevant surfaces
  (`golden::blockdb_header_layout`, `prop::blockdb_roundtrip`,
  `recovery_rebuilds_index`) are the named anchors.
- No `Benchmark*`/`Fuzz*` tests exist in `x/blockdb` at this rev.
