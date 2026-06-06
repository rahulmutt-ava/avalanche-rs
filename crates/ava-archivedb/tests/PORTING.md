# PORTING.md — `ava-archivedb`

Tracks parity of this crate against avalanchego package `x/archivedb`
(height-versioned key/value store with `^height` key encoding). One row per
upstream Go test; status is one of `todo` / `wip` / `ported` / `na`. The
milestone exit gate (M1.26) requires no `wip` rows for shipped surfaces. See
`specs/02-testing-strategy.md` §10.1.

Seeded from `go test -list '.*'` over `./x/archivedb/` at avalanchego rev
`fb174e8925`.

Owning task: M1.23 (archivedb `^height` encoding + read-newest-at-or-below).

## Key encoding + sorting

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestDBKeySpace` | `tests/golden_encoding.rs` `golden::archivedb_key_encoding` (Go-extracted `^height` layout, 04 §5.2) | ported |
| `TestParseDBKey` | `tests/golden_encoding.rs` `golden::archivedb_key_encoding` (parse round-trip) | ported |
| `TestNaturalDescSortingForSameKey` | `tests/versioned.rs` `reads_newest_at_or_below` (descending-height read order) | ported |
| `TestSortingDifferentPrefix` | `src/*.rs` `#[cfg(test)]` cross-prefix sort ordering | ported |
| `TestSkipHeight` | `tests/versioned.rs` `reads_newest_at_or_below` (skips intermediate heights) | ported |
| `FuzzMetadataKeyInvariant` | `tests/golden_encoding.rs` `metadata_never_overlaps_user_prefix` (metadata/user key-space disjointness) | ported |

## Read / write / delete semantics

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestDBEntries` | `tests/versioned.rs` `height_tracks_last_written_batch` + `reads_newest_at_or_below` | ported |
| `TestDelete` | `tests/versioned.rs` `empty_value_is_distinct_from_tombstone` (tombstone = empty DB value; `0x00‖value` for a real value, 04 §5.2) | ported |
| `TestDBEfficientLookups` | `tests/versioned.rs` `reads_newest_at_or_below` (seek to newest ≤ height) | ported |
| `TestDBMoreEfficientLookups` | `tests/versioned.rs` `reads_newest_at_or_below` + `missing_key_not_found` | ported |

## Notes / deviations

- **Tombstone precision (04 §5.2):** a *delete* stores an empty (zero-length) DB
  value; a real value stores `0x00 ‖ value`; an explicitly-stored empty *user*
  value still "exists" and is distinct from a delete
  (`empty_value_is_distinct_from_tombstone`). `get_height` on a tombstone
  returns `NotFound`; the lower-level `get_entry` exposes the delete height for
  strict Go `GetEntry` parity.
- The `^height` suffix is a local LEB128 uvarint (`put_uvarint`/`read_uvarint`)
  — no shared uvarint helper exists in the storage tier yet (candidate for
  promotion if a second consumer appears).
- Additional Rust-side behavioral tests (`reset_drops_buffered_ops`) have no Go
  counterpart and guard the buffered-write/reset contract.
- No `Benchmark*` tests exist in `x/archivedb` at this rev.
